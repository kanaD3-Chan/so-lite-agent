//! M2 验收：hello 回合（默认 mock）+ 工具调用回合（脚本化模型 + 用户插件）+ 持久化。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use so_lite_agent::agent::dispatch::ToolCallContext;
use so_lite_agent::agent::r#loop::StopReason;
use so_lite_agent::audit::{Auditor, MemoryAuditSink};
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::context::PluginContext;
use so_lite_agent::contract::{CallerPolicy, Info, PluginError};
use so_lite_agent::events::{Event, MemoryEventSink};
use so_lite_agent::message::MessageKind;
use so_lite_agent::model::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelHandle, ModelRequest, ModelService,
    ModelStream,
};
use so_lite_agent::registry::{PluginDescriptor, UserPlugin, tool_def};
use so_lite_agent::services::ServiceHandles;

struct EchoPlugin;

impl UserPlugin for EchoPlugin {
    fn info() -> Info {
        Info {
            namespace: "demo".into(),
            enabled: true,
            tools: vec![tool_def("echo", "回显参数", CallerPolicy::UserAndModel)],
            ..Default::default()
        }
    }

    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError> {
        ctx.registrar.tool(
            "echo",
            Arc::new(|_ctx: &ToolCallContext, params: Value| {
                Box::pin(async move { Ok(json!({"echo": params})) })
            }),
        )
    }
}

/// 按调用顺序出脚本的模型：每次 stream 弹出下一段 chunks。
struct ScriptedModel {
    queues: Mutex<VecDeque<Vec<Result<ModelChunk, ModelError>>>>,
}

impl ScriptedModel {
    fn new(queues: Vec<Vec<Result<ModelChunk, ModelError>>>) -> Self {
        Self {
            queues: Mutex::new(queues.into()),
        }
    }
}

#[async_trait]
impl ModelService for ScriptedModel {
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

fn text_reply(text: &str) -> Vec<Result<ModelChunk, ModelError>> {
    vec![
        Ok(ModelChunk::TextDelta(text.into())),
        Ok(ModelChunk::ItemDone {
            kind: ItemKind::Message,
        }),
        Ok(ModelChunk::Done),
    ]
}

#[tokio::test]
async fn hello_round_with_default_mock() {
    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events.clone())
        .system_prompt(|| "测试人格".to_string())
        .build()
        .unwrap();

    let outcome = kernel
        .send_user_message(Default::default(), "你好")
        .await
        .unwrap();

    assert!(matches!(outcome.stop_reason, StopReason::Natural));
    assert_eq!(outcome.messages.len(), 1);
    assert!(matches!(
        &outcome.messages[0].kind,
        MessageKind::Assistant { text } if !text.is_empty()
    ));

    let emitted = events.take();
    assert!(
        emitted
            .iter()
            .any(|e| matches!(e, Event::MessageDelta { .. }))
    );
    assert!(emitted.iter().any(|e| matches!(e, Event::TurnEnd { .. })));
}

#[tokio::test]
async fn tool_call_round_with_scripted_model() {
    let events = Arc::new(MemoryEventSink::default());
    let scripted = ScriptedModel::new(vec![
        vec![
            Ok(ModelChunk::ToolCallStart {
                index: 0,
                call_id: "call_1".into(),
                name: "demo__echo".into(),
            }),
            Ok(ModelChunk::ToolCallDelta {
                index: 0,
                data: "{\"x\":1}".into(),
            }),
            Ok(ModelChunk::ItemDone {
                kind: ItemKind::FunctionCall,
            }),
            Ok(ModelChunk::Done),
        ],
        text_reply("回显完成"),
    ]);
    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let handles = ServiceHandles::default().with_model(ModelHandle::new(
        Arc::new(scripted),
        std::time::Duration::from_secs(30),
        auditor,
    ));
    let kernel = KernelBuilder::new()
        .event_sink(events.clone())
        .service_handles(handles)
        .register_plugin(PluginDescriptor::from_plugin::<EchoPlugin>())
        .build()
        .unwrap();

    assert_eq!(kernel.list_tools().len(), 1);
    assert_eq!(kernel.list_tools()[0].name, "demo__echo");

    let outcome = kernel
        .send_user_message(Default::default(), "调用 echo")
        .await
        .unwrap();

    assert!(matches!(outcome.stop_reason, StopReason::Natural));
    assert_eq!(outcome.tool_calls, 1);
    assert_eq!(outcome.messages.len(), 2);
    match &outcome.messages[0].kind {
        MessageKind::ToolCall {
            entry,
            params,
            result,
            ..
        } => {
            assert_eq!(entry, "demo::echo");
            assert_eq!(params, &json!({"x": 1}));
            assert!(result.is_ok());
        }
        other => panic!("首条新增消息应为 ToolCall：{other:?}"),
    }
    assert!(matches!(
        &outcome.messages[1].kind,
        MessageKind::Assistant { text } if text == "回显完成"
    ));
}

#[tokio::test]
async fn messages_are_persisted_to_session_store() {
    let kernel = KernelBuilder::new().build().unwrap();
    let key = Default::default();
    kernel.send_user_message(key, "你好").await.unwrap();

    let sessions = kernel.list_sessions().await.unwrap();
    assert_eq!(sessions.len(), 1);

    let messages = kernel.read_session(&key).await.unwrap();
    assert_eq!(messages.len(), 2); // user + assistant
    assert!(matches!(messages[0].kind, MessageKind::User { .. }));
    assert!(matches!(messages[1].kind, MessageKind::Assistant { .. }));
}
