// 内核插件参考模板：复制即开工（Linus 模式，ADR-0006/0008——内核插件只在官方二进制内，
// 由维护者编译，以**独立 crate** 编写）。用法：
//   1. 新建 crates/plugin-<name>/（Cargo.toml 依赖 `so-lite-agent` 引擎，本文件即 src/lib.rs）；
//   2. 根 Cargo.toml 的 [workspace].members 加一行；crates/sl-agent/Cargo.toml 的依赖加一行；
//   3. 注册装配由 crates/sl-agent/build.rs 自动发现（ADR-0036 改造），无需改任何 Rust 代码。
// 实现 `KernelPlugin` → `KernelDescriptor::from_plugin::<MyKernelPlugin>()` → 注册表收录。
// 本文件被引擎的编译锚定测试 include（路径用 `so_lite_agent::…`，与插件 crate 视角一致）。

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
