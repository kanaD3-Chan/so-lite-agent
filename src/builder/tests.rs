use super::*;
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent::dispatch::ToolCallContext;
use crate::agent::r#loop::StopReason;
use crate::agent::session::{Goal, SessionKey, SessionMeta, SessionSwitch, Summarizer};
use crate::audit::{Auditor, MemoryAuditSink};
use crate::contract::{CallerPolicy, Info};
use crate::events::MemoryEventSink;
use crate::message::Message;
use crate::model::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelHandle, ModelRequest, ModelService,
    ModelStream,
};
use crate::registry::{PluginDescriptor, tool_def};
use crate::services::{InMemorySessionStore, ServiceHandles, SessionStore};
use serde_json::Value;

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
            .append_message(
                &key,
                &Message::user(format!("第 {i} 条长消息：{}", "长内容填充。".repeat(40))),
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
