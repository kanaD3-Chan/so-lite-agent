//! 内核插件参考模板：复制即开工。
//! 实现 `KernelPlugin` → `KernelDescriptor::from_plugin::<MyKernelPlugin>()` → `register_kernel_plugin`。

use std::sync::Arc;

use serde_json::{Value, json};
use so_lite_agent::agent::dispatch::ToolCallContext;
use so_lite_agent::context::KernelContext;
use so_lite_agent::contract::{CallerPolicy, Info, PluginError, ToolError};
use so_lite_agent::registry::{KernelDescriptor, KernelPlugin, tool_def};
use so_lite_agent::services::ServiceId;

pub struct MyKernelPlugin;

impl KernelPlugin for MyKernelPlugin {
    fn info() -> Info {
        Info {
            namespace: "kernel_my".into(),
            provides: vec![ServiceId::custom("my_service")],
            tools: vec![tool_def("stats", "服务统计", CallerPolicy::UserAndModel)],
            ..Default::default()
        }
    }

    fn register(ctx: KernelContext<'_>) -> Result<(), PluginError> {
        // 内核插件拿全量句柄。
        let service = ctx
            .handles
            .get_custom::<MyServiceType>(&ServiceId::custom("my_service"))
            .expect("服务已注入");
        ctx.registrar.tool(
            "stats",
            Arc::new(move |_ctx: &ToolCallContext, _params: Value| {
                let service = service.clone();
                Box::pin(async move {
                    let n = service.count().await.map_err(ToolError::handler)?;
                    Ok(json!({"count": n}))
                })
            }),
        )
    }
}

pub fn descriptor() -> KernelDescriptor {
    KernelDescriptor::from_plugin::<MyKernelPlugin>()
}

// 占位：与 user_plugin.rs 共享的业务服务具体类型。
pub struct MyServiceType;
impl MyServiceType {
    async fn count(&self) -> Result<usize, String> {
        Ok(0)
    }
}
