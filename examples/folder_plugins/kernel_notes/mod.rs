//! 内核插件：mod.rs 只承载两段式契约，实现放 core.rs。

mod core;

use so_lite_agent::context::KernelContext;
use so_lite_agent::contract::{CallerPolicy, Info, PluginError};
use so_lite_agent::registry::{KernelDescriptor, KernelPlugin, tool_def};

use crate::notes::notes_service_id;

use crate::kernel_notes::core::register_handlers;

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
        register_handlers(ctx)
    }
}

pub fn descriptor() -> KernelDescriptor {
    KernelDescriptor::from_plugin::<NotesKernelPlugin>()
}
