//! 用户插件实现：handler 绑定与业务逻辑。

use std::sync::Arc;

use serde_json::{Value, json};
use so_lite_agent::agent::dispatch::ToolCallContext;
use so_lite_agent::context::PluginContext;
use so_lite_agent::contract::{PluginError, ToolError};

use crate::notes::{MemoryNoteService, NoteService, notes_service_id};

pub fn register_handlers(ctx: PluginContext<'_>) -> Result<(), PluginError> {
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
