//! 插件文件夹编排示例：一插件一目录，mod.rs 承载两段式契约，core.rs 放实现。
//!
//! 目录是组织约定，注册仍是显式链式调用（与 examples/plugins.rs 等价，只是代码分目录）。

mod kernel_notes;
mod notes;
mod study;

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use so_lite_agent::audit::{Auditor, MemoryAuditSink};
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::events::MemoryEventSink;
use so_lite_agent::model::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelHandle, ModelRequest, ModelService,
    ModelStream,
};
use so_lite_agent::services::ServiceHandles;

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
        .with_custom(
            notes::notes_service_id(),
            Arc::new(notes::MemoryNoteService::default()),
        );

    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events)
        .service_handles(handles)
        // 聚合点：每插件一行（文件夹编排下唯一的显式清单）。
        .register_kernel_plugin(kernel_notes::descriptor())
        .register_plugin(study::descriptor())
        .build()?;

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
