//! 插件开发上手示例：自定义服务 + 内核插件 + 用户插件端到端。
//!
//! 心智模型：
//! - 服务**实例**放进 `ServiceHandles`（`builder.service_handles(...)`）；
//! - 内核插件声明 `provides` 并注册特权入口（register 拿全量句柄）；
//! - 用户插件声明 `requires` 并注册业务工具（register 只拿声明过的句柄）；
//! - 自定义服务经 `with_custom` 注入、`get_custom::<T>()` 按具体类型取回。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use so_lite_agent::agent::dispatch::ToolCallContext;
use so_lite_agent::audit::{Auditor, MemoryAuditSink};
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::context::{KernelContext, PluginContext};
use so_lite_agent::contract::{CallerPolicy, Info, PluginError, ToolError};
use so_lite_agent::events::MemoryEventSink;
use so_lite_agent::model::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelHandle, ModelRequest, ModelService,
    ModelStream,
};
use so_lite_agent::registry::{
    KernelDescriptor, KernelPlugin, PluginDescriptor, UserPlugin, tool_def,
};
use so_lite_agent::services::{ServiceHandles, ServiceId};

// ---------- 第一步：使用方自己的业务服务 ----------

#[async_trait]
pub trait NoteService: Send + Sync {
    async fn save(&self, content: &str) -> Result<u64, String>;
    async fn count(&self) -> Result<usize, String>;
}

#[derive(Default)]
pub struct MemoryNoteService {
    notes: Mutex<Vec<String>>,
}

#[async_trait]
impl NoteService for MemoryNoteService {
    async fn save(&self, content: &str) -> Result<u64, String> {
        let mut notes = self.notes.lock().expect("notes poisoned");
        notes.push(content.to_string());
        Ok(notes.len() as u64)
    }

    async fn count(&self) -> Result<usize, String> {
        Ok(self.notes.lock().expect("notes poisoned").len())
    }
}

fn notes_service_id() -> ServiceId {
    ServiceId::custom("notes")
}

// ---------- 第二步：内核插件（信任边界内，注册特权入口 + 声明 provides） ----------

pub struct NotesKernelPlugin;

impl KernelPlugin for NotesKernelPlugin {
    fn info() -> Info {
        Info {
            namespace: "kernel_notes".into(),
            enabled: true,
            provides: vec![notes_service_id()],
            tools: vec![tool_def(
                "stats",
                "查看笔记统计",
                CallerPolicy::UserAndModel,
            )],
            ..Default::default()
        }
    }

    fn register(ctx: KernelContext<'_>) -> Result<(), PluginError> {
        // 内核插件拿全量句柄：可取回任意自定义服务。
        let notes = ctx
            .handles
            .get_custom::<MemoryNoteService>(&notes_service_id())
            .expect("builder 已注入 notes 服务");
        ctx.registrar.tool(
            "stats",
            Arc::new(move |_ctx: &ToolCallContext, _params: Value| {
                let notes = notes.clone();
                Box::pin(async move {
                    let count = notes.count().await.map_err(ToolError::handler)?;
                    Ok(json!({ "count": count }))
                })
            }),
        )
    }
}

// ---------- 第三步：用户插件（业务工具，只拿声明过的句柄） ----------

pub struct StudyPlugin;

impl UserPlugin for StudyPlugin {
    fn info() -> Info {
        Info {
            namespace: "study".into(),
            enabled: true,
            requires: vec![notes_service_id()],
            tools: vec![tool_def(
                "remind",
                "提醒复习最近笔记",
                CallerPolicy::UserAndModel,
            )],
            ..Default::default()
        }
    }

    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError> {
        let notes = ctx
            .handles
            .get_custom::<MemoryNoteService>(&notes_service_id())
            .expect("requires 已校验，服务必然注入");
        ctx.registrar.tool(
            "remind",
            Arc::new(move |_ctx: &ToolCallContext, _params: Value| {
                let notes = notes.clone();
                Box::pin(async move {
                    let count = notes.count().await.map_err(ToolError::handler)?;
                    Ok(json!({ "remind": format!("你有 {count} 条笔记待复习") }))
                })
            }),
        )
    }
}

// ---------- 脚本化模型：模拟 LLM 先调两个工具、再给最终回答 ----------

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

fn tool_call(name: &str, call_id: &str, args: &str) -> Vec<Result<ModelChunk, ModelError>> {
    vec![
        Ok(ModelChunk::ToolCallStart {
            index: 0,
            call_id: call_id.into(),
            name: name.into(),
        }),
        Ok(ModelChunk::ToolCallDelta {
            index: 0,
            data: args.into(),
        }),
        Ok(ModelChunk::ItemDone {
            kind: ItemKind::FunctionCall,
        }),
        Ok(ModelChunk::Done),
    ]
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 装配：服务实例 + 两个插件 + 脚本化模型，一次跑通。
    let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
    let handles = ServiceHandles::default()
        .with_model(ModelHandle::new(
            Arc::new(ScriptedModel::new(vec![
                tool_call("study__remind", "call_1", "{}"),
                tool_call("kernel_notes__stats", "call_2", "{}"),
                text_reply("复习提醒已生成。"),
            ])),
            Duration::from_secs(30),
            auditor,
        ))
        .with_custom(notes_service_id(), Arc::new(MemoryNoteService::default()));

    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events.clone())
        .service_handles(handles)
        .register_kernel_plugin(KernelDescriptor::from_plugin::<NotesKernelPlugin>())
        .register_plugin(PluginDescriptor::from_plugin::<StudyPlugin>())
        .build()?;

    // 模型可见两个工具（wire name）。
    println!("tools={:?}", kernel.list_tools());

    let outcome = kernel
        .send_user_message(Default::default(), "帮我安排复习")
        .await?;
    println!(
        "stop_reason={:?} tool_calls={}",
        outcome.stop_reason, outcome.tool_calls
    );
    for msg in &outcome.messages {
        println!("{msg:?}");
    }
    Ok(())
}
