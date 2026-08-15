use super::*;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent::dispatch::ToolCallContext;
use crate::agent::r#loop::StopReason;
use crate::agent::session::{
    Goal, Interrupt, InterruptBus, SessionKey, SessionMeta, SessionSwitch, Summarizer,
};
use crate::audit::{Auditor, MemoryAuditSink};
use crate::contract::{CallerPolicy, Info};
use crate::events::MemoryEventSink;
use crate::message::{Message, MessageKind};
use crate::model::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelHandle, ModelRequest, ModelService,
    ModelStream,
};
use crate::registry::{PluginDescriptor, tool_def};
use crate::services::{
    InMemorySessionStore, ServiceHandles, SessionEvent, SessionStore, SurfaceOp,
};
use serde_json::{Value, json};

struct CountingSummarizer(Arc<AtomicUsize>);

#[async_trait]
impl Summarizer for CountingSummarizer {
    async fn summarize(&self, _messages: &[Message], _goal: Option<&Goal>) -> String {
        self.0.fetch_add(1, Ordering::SeqCst);
        "计数摘要".into()
    }
}

struct SequenceModel {
    queues: Mutex<VecDeque<Vec<Result<ModelChunk, ModelError>>>>,
}

impl SequenceModel {
    fn new(queues: Vec<Vec<Result<ModelChunk, ModelError>>>) -> Self {
        Self {
            queues: Mutex::new(queues.into()),
        }
    }
}

#[async_trait]
impl ModelService for SequenceModel {
    async fn stream(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let chunks = self
            .queues
            .lock()
            .expect("script poisoned")
            .pop_front()
            .ok_or_else(|| ModelError::Protocol("脚本耗尽".into()))?;
        Ok(Box::new(futures_util::stream::iter(chunks)))
    }
}

fn switch_call_chunks() -> Vec<Result<ModelChunk, ModelError>> {
    vec![
        Ok(ModelChunk::ToolCallStart {
            index: 0,
            call_id: "switch_1".into(),
            name: "session__switch".into(),
        }),
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            data: r#"{"goal":"切换去整理错题"}"#.into(),
        }),
        Ok(ModelChunk::ItemDone {
            kind: ItemKind::FunctionCall,
        }),
        Ok(ModelChunk::Done),
    ]
}

fn text_chunks(text: &str) -> Vec<Result<ModelChunk, ModelError>> {
    vec![
        Ok(ModelChunk::TextDelta(text.into())),
        Ok(ModelChunk::ItemDone {
            kind: ItemKind::Message,
        }),
        Ok(ModelChunk::Done),
    ]
}

struct TestSwitch {
    switched: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl SessionSwitch for TestSwitch {
    async fn switch(&self, goal: &str) -> Result<SessionKey, String> {
        self.switched
            .lock()
            .expect("switched poisoned")
            .push(goal.into());
        Ok(SessionKey::new())
    }
}

#[tokio::test]
async fn builder_wires_summarizer_for_compaction() {
    let counted = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemorySessionStore::default());
    let key = SessionKey::new();
    store
        .create_session(&key, &SessionMeta::new(key))
        .await
        .unwrap();
    for i in 0..3 {
        store
            .append_event(
                &key,
                SessionEvent::new(
                    Message::user(format!("第 {i} 条长消息：{}", "长内容填充。".repeat(40))),
                    SurfaceOp::Append,
                ),
            )
            .await
            .unwrap();
    }

    let kernel = KernelBuilder::new()
        .service_handles(ServiceHandles::default().with_session(store))
        .summarizer(Arc::new(CountingSummarizer(counted.clone())))
        .context_limit_tokens(100)
        .compaction_keep_last(2)
        .build()
        .unwrap();

    let outcome = kernel.send_user_message(key, "继续").await.unwrap();
    assert!(outcome.compaction.is_some(), "达到阈值应触发压缩");
    assert!(
        counted.load(Ordering::SeqCst) > 0,
        "注入的摘要器应被 loop 调用"
    );
}

#[tokio::test]
async fn builder_wires_session_switch_hook() {
    let switched = Arc::new(Mutex::new(Vec::new()));
    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let handles = ServiceHandles::default().with_model(ModelHandle::new(
        Arc::new(SequenceModel::new(vec![
            switch_call_chunks(),
            text_chunks("已切换会话。"),
        ])),
        Duration::from_secs(30),
        auditor,
    ));

    let kernel = KernelBuilder::new()
        .event_sink(Arc::new(MemoryEventSink::default()))
        .service_handles(handles)
        .register_plugin(PluginDescriptor {
            info: Info {
                namespace: "session".into(),
                enabled: true,
                tools: vec![tool_def("switch", "切换会话", CallerPolicy::UserAndModel)],
                ..Default::default()
            },
            register: |ctx| {
                ctx.registrar.tool(
                    "switch",
                    Arc::new(|_ctx: &ToolCallContext, _params: Value| {
                        Box::pin(async move { Ok(serde_json::json!({ "switched": false })) })
                    }),
                )
            },
        })
        .session_switch(Arc::new(TestSwitch {
            switched: switched.clone(),
        }))
        .build()
        .unwrap();

    let outcome = kernel
        .send_user_message(SessionKey::new(), "切换去整理错题")
        .await
        .unwrap();
    assert!(matches!(outcome.stop_reason, StopReason::Natural));
    assert!(outcome.session_key.is_some(), "回合内切换应回填新会话键");
    assert_eq!(
        switched.lock().expect("switched poisoned").len(),
        1,
        "注入的切换钩子应被 loop 调用"
    );
}

#[tokio::test]
async fn context_compact_tool_compacts_immediately_and_keeps_history() {
    // 会话既有 6 条历史消息；回合内模型调用 context::compact 手动压缩
    // （keep_last=2），压缩后追加 compact 调用气泡与助手回复——全部应落盘，
    // 历史不因压缩消失；活跃路径推进到回合真实末条。
    let counted = Arc::new(AtomicUsize::new(0));
    let store = Arc::new(InMemorySessionStore::default());
    let key = SessionKey::new();
    store
        .create_session(&key, &SessionMeta::new(key))
        .await
        .unwrap();
    for i in 0..6 {
        store
            .append_event(
                &key,
                SessionEvent::new(Message::user(format!("历史消息 {i}")), SurfaceOp::Append),
            )
            .await
            .unwrap();
    }

    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let handles = ServiceHandles::default()
        .with_session(store.clone())
        .with_model(ModelHandle::new(
            Arc::new(SequenceModel::new(vec![
                vec![
                    Ok(ModelChunk::ToolCallStart {
                        index: 0,
                        call_id: "compact_1".into(),
                        name: "context__compact".into(),
                    }),
                    Ok(ModelChunk::ToolCallDelta {
                        index: 0,
                        data: "{}".into(),
                    }),
                    Ok(ModelChunk::ItemDone {
                        kind: ItemKind::FunctionCall,
                    }),
                    Ok(ModelChunk::Done),
                ],
                text_chunks("已压缩。"),
            ])),
            Duration::from_secs(30),
            auditor,
        ));

    let kernel = KernelBuilder::new()
        .event_sink(Arc::new(MemoryEventSink::default()))
        .service_handles(handles)
        .summarizer(Arc::new(CountingSummarizer(counted.clone())))
        .compaction_keep_last(2)
        .register_plugin(PluginDescriptor {
            info: Info {
                namespace: "context".into(),
                enabled: true,
                tools: vec![tool_def(
                    "compact",
                    "压缩上下文",
                    CallerPolicy::UserAndModel,
                )],
                ..Default::default()
            },
            register: |ctx| {
                ctx.registrar.tool(
                    "compact",
                    Arc::new(|_ctx: &ToolCallContext, _params: Value| {
                        Box::pin(async move { Ok(serde_json::json!({ "compacted": false })) })
                    }),
                )
            },
        })
        .build()
        .unwrap();

    let outcome = kernel
        .send_user_message(key, "帮我压缩一下上下文")
        .await
        .unwrap();
    assert!(outcome.compaction.is_some(), "手动压缩应产出压缩信息");
    assert!(counted.load(Ordering::SeqCst) > 0, "摘要器应被调用");

    // 事件全量：6 历史 + 1 用户 + 摘要 + compact 调用气泡 + 助手回复 = 10。
    let all = store.read_all(&key).await.unwrap();
    assert_eq!(all.len(), 10, "历史消息不因压缩而消失");
    assert!(
        all.iter().any(|m| matches!(
            &m.kind,
            MessageKind::System { text, .. } if text.contains("上下文压缩摘要")
        )),
        "摘要节点应落盘"
    );
    assert!(
        all.iter().any(|m| matches!(
            &m.kind,
            MessageKind::ToolCall { entry, .. } if entry == "context::compact"
        )),
        "compact 调用消息应如实落时间线"
    );

    // 活跃链 = 摘要 + 保留 2 条 + compact 气泡 + 助手回复（压缩后消息不丢）。
    let path = store.read_path(&key).await.unwrap();
    assert_eq!(path.len(), 5, "活跃路径应推进到回合真实末条");
    assert!(
        matches!(&path[0].kind, MessageKind::System { text, .. } if text.contains("上下文压缩摘要")),
        "活跃链应以摘要节点开头"
    );
    assert!(
        matches!(&path.last().unwrap().kind, MessageKind::Assistant { .. }),
        "活跃链末条应是压缩后的助手回复"
    );
}

/// 端到端：JSONL 会话存储（ADR-0007 第二步）完整回合落盘 + 重开恢复 + 编辑遮蔽。
#[tokio::test]
async fn jsonl_store_full_turn_persists_and_restores() {
    let dir = tempfile::TempDir::new().unwrap();
    let key = SessionKey::new();
    let store = crate::services::JsonlSessionStore::open(dir.path()).unwrap();

    // 模型：一轮工具回合 = 文本 → 工具调用（同组 chunks），下一轮文本收尾。
    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let handles = ServiceHandles::default()
        .with_session(std::sync::Arc::new(store.clone()))
        .with_model(ModelHandle::new(
            Arc::new(SequenceModel::new(vec![
                vec![
                    Ok(ModelChunk::TextDelta("你好！我是助手。".into())),
                    Ok(ModelChunk::ItemDone {
                        kind: ItemKind::Message,
                    }),
                    Ok(ModelChunk::ToolCallStart {
                        index: 0,
                        call_id: "call_echo".into(),
                        name: "demo__echo".into(),
                    }),
                    Ok(ModelChunk::ToolCallDelta {
                        index: 0,
                        data: r#"{"text":"hi"}"#.into(),
                    }),
                    Ok(ModelChunk::ItemDone {
                        kind: ItemKind::FunctionCall,
                    }),
                    Ok(ModelChunk::Done),
                ],
                text_chunks("已完成。"),
            ])),
            Duration::from_secs(30),
            auditor,
        ));
    let kernel = KernelBuilder::new()
        .event_sink(Arc::new(MemoryEventSink::default()))
        .service_handles(handles)
        .register_plugin(PluginDescriptor {
            info: Info {
                namespace: "demo".into(),
                enabled: true,
                tools: vec![tool_def("echo", "回显", CallerPolicy::UserAndModel)],
                ..Default::default()
            },
            register: |ctx| {
                ctx.registrar.tool(
                    "echo",
                    Arc::new(|_ctx: &ToolCallContext, params: Value| {
                        Box::pin(async move { Ok(json!({ "echo": params["text"] })) })
                    }),
                )
            },
        })
        .build()
        .unwrap();

    let outcome = kernel.send_user_message(key, "你好").await.unwrap();
    assert!(matches!(outcome.stop_reason, StopReason::Natural));
    assert_eq!(outcome.messages.len(), 3, "文本 + 工具 + 收尾文本");

    // 重开（模拟重启）：事件日志完整恢复，活跃链投影一致。
    let reloaded = crate::services::JsonlSessionStore::open(dir.path()).unwrap();
    let msgs = reloaded.read_path(&key).await.unwrap();
    let all = reloaded.read_all(&key).await.unwrap();
    // 事件：user + assistant + tool + assistant = 4 条；活跃链 = 全 4 条（全部 append）。
    assert_eq!(all.len(), 4);
    assert_eq!(msgs.len(), 4);
    assert!(msgs.iter().any(|m| matches!(
        &m.kind,
        crate::message::MessageKind::ToolCall { entry, .. } if entry == "demo::echo"
    )));

    // 编辑 user 消息（"改完重发"）：遮蔽到链尾的节点，活跃链 = [改后的问题]。
    let user_id = msgs
        .iter()
        .find(|m| matches!(m.kind, crate::message::MessageKind::User { .. }))
        .map(|m| m.id)
        .expect("应有 user 消息");
    let edited = kernel
        .edit_message(key, user_id, "改后的问题")
        .await
        .unwrap();
    assert_eq!(
        edited.len(),
        1,
        "编辑 user 后链 = [改后的问题]（旧后续被遮蔽）"
    );
    assert!(matches!(
        &edited[0].kind,
        crate::message::MessageKind::User { text, .. } if text == "改后的问题"
    ));
    // 重新生成已禁用：编辑 assistant 消息必须被拒绝。
    let assistant_id = msgs
        .iter()
        .find(|m| matches!(m.kind, crate::message::MessageKind::Assistant { .. }))
        .map(|m| m.id)
        .expect("应有 assistant 消息");
    assert!(
        kernel
            .edit_message(key, assistant_id, "改写")
            .await
            .is_err()
    );
    // 重开后编辑仍生效（遮蔽是事件日志的一部分）。
    let reloaded2 = crate::services::JsonlSessionStore::open(dir.path()).unwrap();
    let msgs2 = reloaded2.read_path(&key).await.unwrap();
    assert_eq!(msgs2.len(), 1);
    assert!(matches!(
        &msgs2[0].kind,
        crate::message::MessageKind::User { text, .. } if text == "改后的问题"
    ));
}

// ---------- 事件决策分离（P2）：LoopHook ----------

struct DenyEchoHook {
    denied: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl crate::agent::r#loop::LoopHook for DenyEchoHook {
    async fn before_tool(&self, entry: &str, _params: &Value) -> crate::agent::r#loop::ToolVerdict {
        if entry == "demo::echo" {
            self.denied
                .lock()
                .expect("denied poisoned")
                .push(entry.into());
            crate::agent::r#loop::ToolVerdict::Deny("demo::echo 已被 hook 拒绝".into())
        } else {
            crate::agent::r#loop::ToolVerdict::Allow(None)
        }
    }
}

struct RewriteEchoHook;

#[async_trait]
impl crate::agent::r#loop::LoopHook for RewriteEchoHook {
    async fn before_tool(&self, entry: &str, _params: &Value) -> crate::agent::r#loop::ToolVerdict {
        if entry == "demo::echo" {
            crate::agent::r#loop::ToolVerdict::Allow(Some(json!({ "text": "改写后的参数" })))
        } else {
            crate::agent::r#loop::ToolVerdict::Allow(None)
        }
    }
}

struct ObserveHook {
    after: Arc<Mutex<Vec<String>>>,
    stops: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl crate::agent::r#loop::LoopHook for ObserveHook {
    async fn after_tool(&self, entry: &str, _result: &Result<Value, crate::contract::ToolError>) {
        self.after
            .lock()
            .expect("after poisoned")
            .push(entry.into());
    }
    async fn turn_stopping(&self, stop: &StopReason) {
        self.stops
            .lock()
            .expect("stops poisoned")
            .push(format!("{stop:?}"));
    }
}

/// 组装带 demo::echo 工具 + hook 的 kernel；模型先调工具、后文本收尾。
async fn kernel_with_hooks(hooks: Vec<Arc<dyn crate::agent::r#loop::LoopHook>>) -> Kernel {
    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let handles = ServiceHandles::default().with_model(ModelHandle::new(
        Arc::new(SequenceModel::new(vec![
            vec![
                Ok(ModelChunk::ToolCallStart {
                    index: 0,
                    call_id: "call_echo".into(),
                    name: "demo__echo".into(),
                }),
                Ok(ModelChunk::ToolCallDelta {
                    index: 0,
                    data: r#"{"text":"原始参数"}"#.into(),
                }),
                Ok(ModelChunk::ItemDone {
                    kind: ItemKind::FunctionCall,
                }),
                Ok(ModelChunk::Done),
            ],
            text_chunks("已完成。"),
        ])),
        Duration::from_secs(30),
        auditor,
    ));
    let mut builder = KernelBuilder::new()
        .event_sink(Arc::new(MemoryEventSink::default()))
        .service_handles(handles)
        .register_plugin(PluginDescriptor {
            info: Info {
                namespace: "demo".into(),
                enabled: true,
                tools: vec![tool_def("echo", "回显", CallerPolicy::UserAndModel)],
                ..Default::default()
            },
            register: |ctx| {
                ctx.registrar.tool(
                    "echo",
                    Arc::new(|_ctx: &ToolCallContext, params: Value| {
                        Box::pin(async move { Ok(json!({ "echo": params["text"] })) })
                    }),
                )
            },
        });
    for hook in hooks {
        builder = builder.loop_hook(hook);
    }
    builder.build().unwrap()
}

#[tokio::test]
async fn hook_denies_tool_and_keeps_history() {
    let denied = Arc::new(Mutex::new(Vec::new()));
    let kernel = kernel_with_hooks(vec![Arc::new(DenyEchoHook {
        denied: denied.clone(),
    })])
    .await;
    let outcome = kernel
        .send_user_message(Default::default(), "调用 echo")
        .await
        .unwrap();
    assert_eq!(
        denied.lock().expect("denied poisoned").len(),
        1,
        "hook 应拒绝工具"
    );
    // tool_calls 计模型请求数（含被拒的）；核心验证：拒绝结果作为工具错误落回。
    assert!(outcome.tool_calls >= 1);
    // 拒绝结果作为工具错误消息落回（模型可见）。
    assert!(outcome.messages.iter().any(|m| matches!(
        &m.kind,
        crate::message::MessageKind::ToolCall { result: Err(_), .. }
    )));
}

#[tokio::test]
async fn hook_rewrites_params() {
    let kernel = kernel_with_hooks(vec![Arc::new(RewriteEchoHook)]).await;
    let outcome = kernel
        .send_user_message(Default::default(), "调用 echo")
        .await
        .unwrap();
    assert_eq!(outcome.tool_calls, 1);
    assert!(outcome.messages.iter().any(|m| matches!(
        &m.kind,
        crate::message::MessageKind::ToolCall { result: Ok(v), .. } if v["echo"] == "改写后的参数"
    )));
}

#[tokio::test]
async fn hooks_observe_after_tool_and_turn_stopping() {
    let after = Arc::new(Mutex::new(Vec::new()));
    let stops = Arc::new(Mutex::new(Vec::new()));
    let kernel = kernel_with_hooks(vec![Arc::new(ObserveHook {
        after: after.clone(),
        stops: stops.clone(),
    })])
    .await;
    let outcome = kernel
        .send_user_message(Default::default(), "调用 echo")
        .await
        .unwrap();
    assert_eq!(
        after.lock().expect("after poisoned").len(),
        1,
        "after_tool 应被调用"
    );
    assert!(
        !stops.lock().expect("stops poisoned").is_empty(),
        "turn_stopping 应被调用"
    );
    assert!(outcome.tool_calls >= 1);
}

#[tokio::test]
async fn registry_arc_shares_kernel_registry() {
    // registry_arc() 返回与 kernel 内部**同一**注册表实例的 Arc：外部装配
    // （ScriptPluginLoader 热插拔加载器等）据此向运行中 kernel 注册/查询条目。
    let kernel = kernel_with_hooks(vec![]).await;
    let registry = kernel.registry_arc();
    assert!(
        registry.namespaces().iter().any(|n| n == "demo"),
        "registry_arc 应能看到 kernel 已注册的 namespace，实际：{:?}",
        registry.namespaces()
    );
    // 同一实例（Arc 内层地址 == &Registry 视图地址）。
    assert!(std::ptr::eq(&*registry, kernel.registry()));
}

// ---------- 外部驱动回合（ADR-0011）：run_turn + 共享中断总线 ----------

#[tokio::test]
async fn run_turn_drives_turn_on_active_path_without_user_message() {
    // 业务中断（告警通知）场景：外部先落盘一条 tool 消息（alarm::notify），
    // 再经 Kernel::run_turn 空闲开回合——模型看到工具结果并回复，落盘管线与
    // send_user_message 一致（时间线可见、可审计）。
    let key = SessionKey::new();
    let store = Arc::new(InMemorySessionStore::default());
    store
        .create_session(&key, &SessionMeta::new(key))
        .await
        .unwrap();
    store
        .append_event(
            &key,
            SessionEvent::new(
                Message::tool_call(
                    "alarm::notify",
                    json!({}),
                    Ok(json!({
                        "items": [{
                            "kind": "threshold",
                            "device_id": "env_sensor",
                            "metric": "temp",
                            "value": 33.5,
                            "triggered_at": 1723700000000u64,
                        }]
                    })),
                ),
                SurfaceOp::Append,
            ),
        )
        .await
        .unwrap();
    let last = store.read_path(&key).await.unwrap().pop().unwrap();
    store.set_active_path(&key, Some(last.id)).await.unwrap();

    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let handles = ServiceHandles::default()
        .with_session(store.clone())
        .with_model(ModelHandle::new(
            Arc::new(SequenceModel::new(vec![text_chunks(
                "收到告警，我来处理。",
            )])),
            Duration::from_secs(30),
            auditor,
        ));
    let kernel = KernelBuilder::new()
        .event_sink(Arc::new(MemoryEventSink::default()))
        .service_handles(handles)
        .build()
        .unwrap();

    let outcome = kernel.run_turn(key).await.unwrap();
    assert!(matches!(outcome.stop_reason, StopReason::Natural));
    assert_eq!(outcome.messages.len(), 1, "回合新增 = 助手回复");
    assert!(matches!(
        &outcome.messages[0].kind,
        MessageKind::Assistant { text } if text == "收到告警，我来处理。"
    ));

    // 落盘管线一致：活跃链 = [notify 工具消息, 助手回复]；活跃末端推进到回复。
    let path = store.read_path(&key).await.unwrap();
    assert_eq!(path.len(), 2);
    assert!(matches!(
        &path[0].kind,
        MessageKind::ToolCall { entry, .. } if entry == "alarm::notify"
    ));
    assert!(matches!(&path[1].kind, MessageKind::Assistant { .. }));
}

#[tokio::test]
async fn injected_interrupt_bus_is_shared_with_kernel() {
    // KernelBuilder::interrupt_bus 注入：build 前外部持同一总线，build 后
    // kernel.interrupt_bus() 看到的是同一实例（外部 send → kernel take_all）。
    let bus = InterruptBus::new();
    let kernel = KernelBuilder::new()
        .event_sink(Arc::new(MemoryEventSink::default()))
        .interrupt_bus(bus.clone())
        .build()
        .unwrap();
    bus.send(Interrupt::Custom {
        name: "iot.alert".into(),
        payload: json!({"value": 1}),
    });
    let taken = kernel.interrupt_bus().take_all();
    assert_eq!(taken.len(), 1);
    assert!(matches!(
        &taken[0],
        Interrupt::Custom { name, .. } if name == "iot.alert"
    ));
}
