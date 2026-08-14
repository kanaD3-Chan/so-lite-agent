//! 内核插件实现：全量句柄下注册特权入口。

use std::sync::Arc;

use serde_json::{Value, json};
use so_lite_agent::agent::dispatch::ToolCallContext;
use so_lite_agent::context::KernelContext;
use so_lite_agent::contract::{PluginError, ToolError};

use crate::notes::{MemoryNoteService, NoteService, notes_service_id};

pub fn register_handlers(ctx: KernelContext<'_>) -> Result<(), PluginError> {
    let notes = ctx
        .handles
        .get_custom::<MemoryNoteService>(&notes_service_id())
        .expect("服务已注入");
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
