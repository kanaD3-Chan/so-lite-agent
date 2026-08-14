//! Kernel：组装完成的 Agent 运行时实例（直连 Rust API 与通用 RPC 入口）。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use crate::agent::dispatch::Dispatch;
use crate::agent::r#loop::{AgentLoop, LoopError, TurnInput, TurnOutcome};
use crate::agent::session::{InterruptBus, SessionDecision, SessionKey, SessionMeta};
use crate::audit::{AuditRecord, Auditor};
use crate::contract::{CallerPolicy, ToolError, full_to_wire};
use crate::events::EventSink;
use crate::message::{Attachment, Message, MessageId, MessageKind};
use crate::model::{AbortSignal, ToolSchema};
use crate::registry::Registry;
use crate::rpc::{Method, RpcAttachment, RpcError, RpcExtension, RpcFrame, RpcRequest};
use crate::services::{SessionEvent, SessionStore, SurfaceOp, fold_surface};

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
    loop_engine: Arc<dyn AgentLoop>,
    store: Arc<dyn SessionStore>,
    auditor: Auditor,
    events: Arc<dyn EventSink>,
    bus: InterruptBus,
    turn_budget: Duration,
    rpc_extensions: Vec<Arc<dyn RpcExtension>>,
    active: Mutex<Option<AbortSignal>>,
    /// 会话调度决策器（使用方注入，ADR-0010）：新消息前置决策 + 回合末决策。
    session_decision: Option<Arc<dyn SessionDecision>>,
}

impl Kernel {
    /// 装配构造器：只供 `KernelBuilder::build` 调用（字段保持私有）。
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn assemble(
        registry: Arc<Registry>,
        dispatch: Arc<Dispatch>,
        loop_engine: Arc<dyn AgentLoop>,
        store: Arc<dyn SessionStore>,
        auditor: Auditor,
        events: Arc<dyn EventSink>,
        bus: InterruptBus,
        turn_budget: Duration,
        rpc_extensions: Vec<Arc<dyn RpcExtension>>,
        active: Mutex<Option<AbortSignal>>,
        session_decision: Option<Arc<dyn SessionDecision>>,
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
            session_decision,
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// 注册表 Arc 句柄：外部装配（`ScriptPluginLoader` 热插拔加载器等）需要与
    /// kernel 共享**同一**注册表实例，`&Registry` 不足以构造持有型句柄。
    /// （plugin-dev.md 的 `kernel.registry().clone()` 示例依赖 Registry: Clone，
    /// 实际不存在——本方法才是正解。）
    pub fn registry_arc(&self) -> Arc<Registry> {
        self.registry.clone()
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

    /// 发送一条用户消息：会话不存在自动创建；user 消息先落盘（追加事件），跑完
    /// loop 把新增消息追加为事件，并更新 last_activity_at。
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
        self.send_user_message_inner(key, text, None, attachments, None)
            .await
    }

    /// 显式工具调用（mistake-agent 同款语义，ADR-0012）：text 为模型指令文本，
    /// display_text 为落盘展示文本（两者分离），forced_wire 为强制首轮调用的
    /// wire name（None = 普通回合）。loop 层已支持首轮 tool_choice + 整回合
    /// 关闭思考（TurnInput.forced_tool），本方法只负责校验后的接线与落盘。
    pub async fn send_user_message_forced(
        &self,
        key: SessionKey,
        text: &str,
        display_text: Option<String>,
        attachments: Vec<Attachment>,
        forced_wire: String,
    ) -> Result<TurnOutcome, LoopError> {
        self.send_user_message_inner(key, text, display_text, attachments, Some(forced_wire))
            .await
    }

    async fn send_user_message_inner(
        &self,
        key: SessionKey,
        text: &str,
        display_text: Option<String>,
        attachments: Vec<Attachment>,
        forced_wire: Option<String>,
    ) -> Result<TurnOutcome, LoopError> {
        // 会话调度决策注入时：前置决策（新消息先判断要不要切换上下文，再追加/
        // 分叉/切换），返回进入回合的会话 key 与消息链；否则默认 create + append。
        let (effective_key, messages) = match &self.session_decision {
            Some(decider) => {
                let (k, msgs) = decider
                    .on_new_message(key, text, display_text)
                    .await
                    .map_err(|e| LoopError::Internal(format!("会话调度决策失败：{e}")))?;
                (k, msgs)
            }
            None => {
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

                let mut user_msg = Message::user_with_display(text, display_text);
                if let MessageKind::User {
                    attachments: atts, ..
                } = &mut user_msg.kind
                {
                    *atts = attachments;
                }
                self.store
                    .append_event(&key, SessionEvent::new(user_msg, SurfaceOp::Append))
                    .await
                    .map_err(session_err)?;

                // 模型可见消息 = 活跃链投影（含刚追加的 user 消息）。
                let messages = self.store.read_path(&key).await.map_err(session_err)?;
                (key, messages)
            }
        };

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
                forced_tool: forced_wire,
            })
            .await;
        *self.active.lock().expect("active poisoned") = None;
        let outcome = outcome?;

        // 追加本回合新增消息（assistant / reasoning / tool 均为 append）；
        // 记录末条消息 id——活跃路径推进到回合末（mistake-agent 消息树分支语义：
        // active_path 指向链尾，否则 read_path 停在最后一条 user，树内分叉会挂错点、
        // 前端树视图回溯断链）。
        let mut persisted_last: Option<MessageId> = None;
        for msg in &outcome.messages {
            // session::switch 是控制动作不是对话内容：不落会话树、不随历史携带
            // （ADR-0034，mistake-agent 同款——避免模型在新上下文看到切换调用
            // 而反复切换）。
            if msg.is_switch_tool_call() {
                continue;
            }
            let op = match &msg.kind {
                MessageKind::System { .. } => {
                    // 摘要消息由压缩分支统一处理（replace 遮蔽被压段）。
                    continue;
                }
                _ => SurfaceOp::Append,
            };
            let stored = self
                .store
                .append_event(&effective_key, SessionEvent::new(msg.clone(), op))
                .await
                .map_err(session_err)?;
            persisted_last = Some(stored.message.id);
        }
        if let Some(info) = &outcome.compaction {
            // 压缩 = 追加 summary 事件，replace 遮蔽被压段（ADR-0007）。
            let events = self
                .store
                .read_events(&effective_key)
                .await
                .map_err(session_err)?;
            let fold = fold_surface(&events).map_err(session_err)?;
            let id_to_seq: std::collections::HashMap<MessageId, u64> =
                events.iter().map(|e| (e.message.id, e.seq)).collect();
            let tail_seq = id_to_seq
                .get(&info.tail_start)
                .copied()
                .ok_or_else(|| LoopError::Internal("压缩保留段首条不在事件日志".into()))?;
            let tail_idx = fold
                .chain
                .iter()
                .position(|s| *s == tail_seq)
                .ok_or_else(|| LoopError::Internal("压缩保留段首条不在活跃链".into()))?;
            let compacted = &fold.chain[..tail_idx];
            if let (Some(&start), Some(&end)) = (compacted.first(), compacted.last()) {
                let mut summary_event =
                    SessionEvent::new(info.summary.clone(), SurfaceOp::Replace { start, end });
                summary_event.source_event_seqs = compacted.to_vec();
                self.store
                    .append_event(&effective_key, summary_event)
                    .await
                    .map_err(session_err)?;
                // 活跃末端 = 保留段末端（压缩后链 = [摘要, 保留段…]）。
                self.store
                    .set_active_path(&effective_key, Some(info.tail_end))
                    .await
                    .map_err(session_err)?;
            }
            self.events.emit(crate::events::Event::Compaction {
                session: effective_key,
            });
            self.auditor.record(AuditRecord::Compaction {
                session: effective_key.to_string(),
                summarized: info.summarized,
            });
        }
        // 活跃路径推进到回合末条（mistake-agent 消息树分支语义：compaction 用
        // tail_end，否则用末条落盘消息；回合末决策（on_turn_end）若 start_new
        // 分叉会再推进到摘要节点）。
        if outcome.compaction.is_none()
            && let Some(last) = persisted_last
        {
            self.store
                .set_active_path(&effective_key, Some(last))
                .await
                .map_err(session_err)?;
        }
        // 回合末会话调度决策（continue / update_goal / start_new，失败静默降级
        // continue——存疑即继续，mistake-agent ADR-0030）。
        if let Some(decider) = &self.session_decision
            && let Err(e) = decider.on_turn_end(&effective_key, &outcome.messages).await
        {
            log::warn!("回合末会话调度决策失败，忽略：{e}");
        }
        self.store
            .set_last_activity(&effective_key, chrono::Utc::now())
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

    /// 编辑消息：追加新 user 事件，replace 遮蔽从被编辑消息到链尾的全部节点
    /// （"改完重发"语义：编辑用户自己的输入，附件保留，重新驱动模型；
    /// 编辑点之后的历史留在日志但不属于活跃路径，ADR-0007）。
    ///
    /// 只允许编辑 user 消息；assistant 消息不支持改写（**重新生成已禁用**——
    /// 对 assistant 的 replace 遮蔽不开放任何入口）。
    pub async fn edit_message(
        &self,
        key: SessionKey,
        message_id: MessageId,
        text: &str,
    ) -> Result<Vec<Message>, LoopError> {
        let seq = self
            .store
            .resolve_seq(&key, message_id)
            .await
            .map_err(session_err)?;
        let events = self.store.read_events(&key).await.map_err(session_err)?;
        let fold = fold_surface(&events).map_err(session_err)?;
        let original = events
            .iter()
            .find(|e| e.seq == seq)
            .ok_or_else(|| LoopError::Internal("编辑目标事件不存在".into()))?;
        let MessageKind::User { attachments, .. } = &original.message.kind else {
            return Err(LoopError::Internal("只能编辑 user 消息".into()));
        };
        // 被遮蔽区间 = [被编辑消息 ..= 链尾]（编辑点之后全部丢出新活跃路径）。
        let start_idx = fold
            .chain
            .iter()
            .position(|s| *s == seq)
            .ok_or_else(|| LoopError::Internal("编辑目标不在活跃链".into()))?;
        let shadowed = fold.chain[start_idx..].to_vec();
        let end = *shadowed
            .last()
            .ok_or_else(|| LoopError::Internal("活跃链为空".into()))?;
        // 新 user 消息：文本替换（作为模型指令与展示文本），附件保留（改完重发）。
        let mut new_msg = Message::user_with_display(text, None);
        if let MessageKind::User {
            attachments: atts, ..
        } = &mut new_msg.kind
        {
            *atts = attachments.clone();
        }
        let mut edit_event = SessionEvent::new(new_msg, SurfaceOp::Replace { start: seq, end });
        edit_event.source_event_seqs = shadowed;
        let stored = self
            .store
            .append_event(&key, edit_event)
            .await
            .map_err(session_err)?;
        self.store
            .set_active_path(&key, Some(stored.message.id))
            .await
            .map_err(session_err)?;
        self.auditor.record(AuditRecord::MessageEdited {
            message_id,
            branch_id: stored.message.id,
        });
        self.store.read_path(&key).await.map_err(session_err)
    }

    /// 切换活跃路径（遮蔽链分支）。
    pub async fn switch_branch(
        &self,
        key: SessionKey,
        message_id: MessageId,
    ) -> Result<Vec<Message>, LoopError> {
        let seq = self
            .store
            .resolve_seq(&key, message_id)
            .await
            .map_err(session_err)?;
        self.store
            .set_active_path(&key, Some(message_id))
            .await
            .map_err(session_err)?;
        let chain = self
            .store
            .read_path_from(&key, seq)
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
                force_tool,
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
                // 显式工具调用（mistake-agent 同款）：校验工具 + UserOnly 拒绝，
                // 构造模型指令文本与落盘展示文本（分离），开回合强制首轮调用。
                let (instr, display, forced_wire) = match &force_tool {
                    None => (text.clone(), None, None),
                    Some(ft) => {
                        let entry = match self.registry.ensure_tool(&ft.entry) {
                            Ok(e) => e,
                            Err(e) => {
                                return RpcFrame::err(
                                    id,
                                    RpcError::new("unknown_tool", e.to_string()),
                                );
                            }
                        };
                        if entry.policy == CallerPolicy::UserOnly {
                            return RpcFrame::err(
                                id,
                                RpcError::new(
                                    "forbidden_tool",
                                    "该工具仅用户可调，不能被模型强制调用",
                                ),
                            );
                        }
                        let hint = ft.hint.as_deref().unwrap_or("").trim();
                        let instr = if hint.is_empty() {
                            format!("请调用工具 {} 处理当前请求。", ft.entry)
                        } else {
                            format!("请调用工具 {} 处理：{}", ft.entry, hint)
                        };
                        let display =
                            ft.display
                                .clone()
                                .filter(|s| !s.trim().is_empty())
                                .or_else(|| {
                                    self.registry.entry_title(&ft.entry).map(|title| {
                                        if hint.is_empty() {
                                            title
                                        } else {
                                            format!("{title}：{hint}")
                                        }
                                    })
                                });
                        (instr, display, Some(full_to_wire(&ft.entry)))
                    }
                };
                let result = match forced_wire {
                    Some(wire) => {
                        self.send_user_message_forced(key, &instr, display, atts, wire)
                            .await
                    }
                    None => {
                        self.send_user_message_with_attachments(key, &instr, atts)
                            .await
                    }
                };
                match result {
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

    /// 读取会话**全量消息树**（事件日志顺序，含真实 parent_id 与被遮蔽分支——
    /// 前端据此构建树视图 + < / > 分支导航，mistake-agent read_session 同款）；
    /// 活跃链（模型上下文）另经 `read_path` 获得。
    pub async fn read_session(&self, key: &SessionKey) -> Result<Vec<Message>, LoopError> {
        self.store.read_all(key).await.map_err(session_err)
    }

    /// 用户可见工具列表（wire name；GUI 工具目录数据源，mistake-agent 同款：
    /// 只含 user_visible=true，session::switch 等仅模型工具不出现）。
    pub fn list_tools(&self) -> Vec<ToolSchema> {
        self.registry.user_tools()
    }

    /// 用户触发入口（等价 trigger_command）：找不到 Command 时回退同名 Tool。
    pub async fn call_command(&self, entry: &str, params: Value) -> Result<Value, ToolError> {
        self.dispatch.call_command(entry, params).await
    }
}
