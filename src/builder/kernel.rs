//! Kernel：组装完成的 Agent 运行时实例（直连 Rust API 与通用 RPC 入口）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::agent::dispatch::Dispatch;
use crate::agent::r#loop::{AgentLoop, LoopError, TurnInput, TurnOutcome};
use crate::agent::session::{InterruptBus, SessionKey, SessionMeta};
use crate::audit::{AuditRecord, Auditor};
use crate::contract::ToolError;
use crate::events::EventSink;
use crate::message::{Attachment, Message, MessageId, MessageKind};
use crate::model::{AbortSignal, ToolSchema};
use crate::registry::Registry;
use crate::rpc::{Method, RpcAttachment, RpcError, RpcExtension, RpcFrame, RpcRequest};
use crate::services::SessionStore;

fn session_err(e: crate::services::SessionError) -> LoopError {
    LoopError::Internal(e.to_string())
}

fn rpc_error(e: &LoopError) -> RpcError {
    RpcError::new("internal", e.to_string())
}

/// 组装完成的 Agent 内核（M2 直连 Rust API；通用 RPC 在 M4 定型）。
pub struct Kernel {
    registry: Arc<Registry>,
    dispatch: Arc<Dispatch>,
    loop_engine: Arc<AgentLoop>,
    store: Arc<dyn SessionStore>,
    auditor: Auditor,
    events: Arc<dyn EventSink>,
    bus: InterruptBus,
    turn_budget: Duration,
    rpc_extensions: Vec<Arc<dyn RpcExtension>>,
    active: Mutex<Option<AbortSignal>>,
}

impl Kernel {
    /// 装配构造器：只供 `KernelBuilder::build` 调用（字段保持私有）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn assemble(
        registry: Arc<Registry>,
        dispatch: Arc<Dispatch>,
        loop_engine: Arc<AgentLoop>,
        store: Arc<dyn SessionStore>,
        auditor: Auditor,
        events: Arc<dyn EventSink>,
        bus: InterruptBus,
        turn_budget: Duration,
        rpc_extensions: Vec<Arc<dyn RpcExtension>>,
        active: Mutex<Option<AbortSignal>>,
    ) -> Self {
        Self {
            registry,
            dispatch,
            loop_engine,
            store,
            auditor,
            events,
            bus,
            turn_budget,
            rpc_extensions,
            active,
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    pub fn dispatch(&self) -> &Dispatch {
        &self.dispatch
    }

    pub fn auditor(&self) -> &Auditor {
        &self.auditor
    }

    pub fn events(&self) -> &Arc<dyn EventSink> {
        &self.events
    }

    pub fn interrupt_bus(&self) -> &InterruptBus {
        &self.bus
    }

    /// 发送一条用户消息：会话不存在自动创建；user 消息先落盘，跑完 loop
    /// 把新增消息追加回 SessionStore，并更新 last_activity_at。
    pub async fn send_user_message(
        &self,
        key: SessionKey,
        text: &str,
    ) -> Result<TurnOutcome, LoopError> {
        self.send_user_message_with_attachments(key, text, Vec::new())
            .await
    }

    /// 带附件发送用户消息（附件为中性文件引用，数据由使用方自行填充）。
    pub async fn send_user_message_with_attachments(
        &self,
        key: SessionKey,
        text: &str,
        attachments: Vec<Attachment>,
    ) -> Result<TurnOutcome, LoopError> {
        if self
            .store
            .get_session(&key)
            .await
            .map_err(session_err)?
            .is_none()
        {
            self.store
                .create_session(&key, &SessionMeta::new(key))
                .await
                .map_err(session_err)?;
        }

        let mut messages = self.store.read_path(&key).await.map_err(session_err)?;
        let mut user_msg = Message::user(text);
        if let MessageKind::User {
            attachments: atts, ..
        } = &mut user_msg.kind
        {
            *atts = attachments;
        }
        crate::message::append_to_path(&mut messages, user_msg.clone());
        self.store
            .append_message(&key, &user_msg)
            .await
            .map_err(session_err)?;

        let tools = self.registry.model_tools();
        let signal = AbortSignal::new();
        *self.active.lock().expect("active poisoned") = Some(signal.clone());
        let outcome = self
            .loop_engine
            .run_turn(TurnInput {
                messages,
                tools,
                signal,
                turn_budget: self.turn_budget,
                forced_tool: None,
            })
            .await;
        *self.active.lock().expect("active poisoned") = None;
        let outcome = outcome?;

        for msg in &outcome.messages {
            self.store
                .append_message(&key, msg)
                .await
                .map_err(session_err)?;
        }
        if let Some(info) = &outcome.compaction {
            // 摘要消息已计入 outcome.messages（上面已落盘）；这里只改活跃路径链。
            self.store
                .splice_compaction(&key, &info.summary, info.tail_start)
                .await
                .map_err(session_err)?;
            self.store
                .set_active_path(&key, Some(info.tail_end))
                .await
                .map_err(session_err)?;
            self.events
                .emit(crate::events::Event::Compaction { session: key });
            self.auditor.record(AuditRecord::Compaction {
                session: key.to_string(),
                summarized: info.summarized,
            });
        }
        self.store
            .set_last_activity(&key, chrono::Utc::now())
            .await
            .map_err(session_err)?;
        self.auditor.record(AuditRecord::Lifecycle {
            phase: "turn".into(),
        });
        Ok(outcome)
    }

    /// 取消当前回合（无活动回合时静默）。
    pub fn abort(&self) {
        if let Some(signal) = self.active.lock().expect("active poisoned").as_ref() {
            signal.cancel();
        }
    }

    pub fn get_state(&self) -> Value {
        json!({
            "running": self.active.lock().expect("active poisoned").is_some(),
        })
    }

    /// 编辑消息：在 message_id 处派生新分支（文本替换，历史不截断）。
    pub async fn edit_message(
        &self,
        key: SessionKey,
        message_id: MessageId,
        text: &str,
    ) -> Result<Vec<Message>, LoopError> {
        let new_path = self
            .store
            .derive_branch(&key, message_id, text)
            .await
            .map_err(session_err)?;
        if let Some(branch_id) = new_path.last().map(|m| m.id) {
            self.auditor.record(AuditRecord::MessageEdited {
                message_id,
                branch_id,
            });
        }
        Ok(new_path)
    }

    /// 切换活跃路径（消息树分支）。
    pub async fn switch_branch(
        &self,
        key: SessionKey,
        message_id: MessageId,
    ) -> Result<Vec<Message>, LoopError> {
        let chain = self
            .store
            .switch_branch(&key, message_id)
            .await
            .map_err(session_err)?;
        self.auditor
            .record(AuditRecord::BranchSwitched { message_id });
        Ok(chain)
    }

    /// 通用 RPC 入口：返回带 id 的响应帧。
    pub async fn handle_rpc(&self, request: RpcRequest) -> RpcFrame {
        let id = request.id;
        match request.method {
            Method::SendUserMessage {
                session_key,
                text,
                attachments,
            } => {
                let key = session_key.unwrap_or_default();
                let atts: Vec<Attachment> = attachments
                    .into_iter()
                    .map(|a: RpcAttachment| Attachment {
                        path: a.path,
                        name: a.name,
                        mime: None,
                        data_base64: None,
                    })
                    .collect();
                match self
                    .send_user_message_with_attachments(key, &text, atts)
                    .await
                {
                    Ok(outcome) => RpcFrame::ok(
                        id,
                        json!({
                            "stop_reason": outcome.stop_reason,
                            "tool_calls": outcome.tool_calls,
                            "messages": outcome.messages,
                            "usage": outcome.usage,
                            "session_key": outcome.session_key,
                        }),
                    ),
                    Err(e) => RpcFrame::err(id, rpc_error(&e)),
                }
            }
            Method::TriggerCommand { entry, params } => {
                match self.dispatch.call_command(&entry, params).await {
                    Ok(v) => RpcFrame::ok(id, v),
                    Err(e) => RpcFrame::err(
                        id,
                        RpcError::new("tool_error", format!("{:?}: {}", e.code, e.message)),
                    ),
                }
            }
            Method::EditMessage {
                session_key,
                message_id,
                text,
            } => match self.edit_message(session_key, message_id, &text).await {
                Ok(path) => RpcFrame::ok(id, serde_json::to_value(path).unwrap_or_default()),
                Err(e) => RpcFrame::err(id, rpc_error(&e)),
            },
            Method::SwitchBranch {
                session_key,
                message_id,
            } => match self.switch_branch(session_key, message_id).await {
                Ok(chain) => RpcFrame::ok(id, serde_json::to_value(chain).unwrap_or_default()),
                Err(e) => RpcFrame::err(id, rpc_error(&e)),
            },
            Method::Abort => {
                self.abort();
                RpcFrame::ok(id, Value::Null)
            }
            Method::GetState => RpcFrame::ok(id, self.get_state()),
            Method::ListSessions => match self.list_sessions().await {
                Ok(sessions) => {
                    RpcFrame::ok(id, serde_json::to_value(sessions).unwrap_or_default())
                }
                Err(e) => RpcFrame::err(id, rpc_error(&e)),
            },
            Method::ReadSession { session_key } => match self.read_session(&session_key).await {
                Ok(messages) => {
                    RpcFrame::ok(id, serde_json::to_value(messages).unwrap_or_default())
                }
                Err(e) => RpcFrame::err(id, rpc_error(&e)),
            },
            Method::ListTools => RpcFrame::ok(
                id,
                serde_json::to_value(self.list_tools()).unwrap_or_default(),
            ),
            Method::Custom { method, params } => {
                let mut last_err = RpcError::not_handled(&method);
                for ext in &self.rpc_extensions {
                    match ext.handle(&method, params.clone()).await {
                        Ok(v) => return RpcFrame::ok(id, v),
                        Err(e) if e.code == "not_handled" => last_err = e,
                        Err(e) => return RpcFrame::err(id, e),
                    }
                }
                RpcFrame::err(id, last_err)
            }
        }
    }

    pub async fn list_sessions(&self) -> Result<Vec<SessionMeta>, LoopError> {
        self.store.list_sessions().await.map_err(session_err)
    }

    pub async fn read_session(&self, key: &SessionKey) -> Result<Vec<Message>, LoopError> {
        self.store.read_path(key).await.map_err(session_err)
    }

    /// 模型可见工具列表（wire name）。
    pub fn list_tools(&self) -> Vec<ToolSchema> {
        self.registry.model_tools()
    }

    /// 用户触发入口（等价 trigger_command）：找不到 Command 时回退同名 Tool。
    pub async fn call_command(&self, entry: &str, params: Value) -> Result<Value, ToolError> {
        self.dispatch.call_command(entry, params).await
    }
}
