// 内核插件参考模板：复制即开工（Linus 模式，ADR-0006——内核插件只在官方二进制内，
// 由维护者编译，复制到 `src/plugin/<你的插件名>/` 使用；build.rs 自动收录，ADR-0036）。
// 实现 `KernelPlugin` → `KernelDescriptor::from_plugin::<MyKernelPlugin>()` → 注册表收录。

use std::sync::Arc;

use serde_json::{Value, json};
use crate::agent::dispatch::ToolCallContext;
use crate::context::KernelContext;
use crate::contract::{CallerPolicy, Info, PluginError, ToolError};
use crate::registry::{KernelDescriptor, KernelPlugin, tool_def};
use crate::services::ServiceId;

pub struct MyKernelPlugin;

impl KernelPlugin for MyKernelPlugin {
    fn info() -> Info {
        Info {
            namespace: "kernel_my".into(),
            enabled: true,
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
