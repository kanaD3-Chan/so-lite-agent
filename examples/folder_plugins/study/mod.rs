//! 用户插件：mod.rs 只承载两段式契约（info + register），实现放 core.rs。

mod core;

use so_lite_agent::context::PluginContext;
use so_lite_agent::contract::{CallerPolicy, Info, PluginError};
use so_lite_agent::registry::{PluginDescriptor, UserPlugin, tool_def};

use crate::notes::notes_service_id;

use crate::study::core::register_handlers;

pub struct StudyPlugin;

impl UserPlugin for StudyPlugin {
    fn info() -> Info {
        Info {
            namespace: "study".into(),
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
        register_handlers(ctx)
    }
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor::from_plugin::<StudyPlugin>()
}
