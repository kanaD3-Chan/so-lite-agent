//! 用户插件参考模板：复制即开工。
//! 实现 `UserPlugin` → `PluginDescriptor::from_plugin::<MyPlugin>()` → `register_plugin`。

use std::sync::Arc;

use serde_json::{Value, json};
use so_lite_agent::agent::dispatch::ToolCallContext;
use so_lite_agent::context::PluginContext;
use so_lite_agent::contract::{CallerPolicy, Info, PluginError, ToolError};
use so_lite_agent::registry::{PluginDescriptor, UserPlugin, tool_def};
use so_lite_agent::services::ServiceId;

pub struct MyPlugin;

impl UserPlugin for MyPlugin {
    fn info() -> Info {
        Info {
            namespace: "my".into(),
            requires: vec![ServiceId::custom("my_service")],
            tools: vec![tool_def("hello", "打招呼", CallerPolicy::UserAndModel)],
            ..Default::default()
        }
    }

    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError> {
        let service = ctx
            .handles
            .get_custom::<MyServiceType>(&ServiceId::custom("my_service"))
            .expect("requires 已校验");
        ctx.registrar.tool(
            "hello",
            Arc::new(move |_ctx: &ToolCallContext, _params: Value| {
                let service = service.clone();
                Box::pin(async move {
                    let msg = service.say().await.map_err(ToolError::handler)?;
                    Ok(json!({"message": msg}))
                })
            }),
        )
    }
}

pub fn descriptor() -> PluginDescriptor {
    PluginDescriptor::from_plugin::<MyPlugin>()
}

// 占位：你的业务服务具体类型。
pub struct MyServiceType;
impl MyServiceType {
    async fn say(&self) -> Result<String, String> {
        Ok("hi".into())
    }
}
