//! Rune 脚本用户插件示例（ADR-0006）：目录形态（manifest.json + plugin.rn）+ 端到端调用。
//!
//! 运行：`cargo run -p so-lite-agent --example script_plugin --features rune-plugins`
//!
//! 对应仓库根 `plugins/demo/` 目录（一插件一目录：manifest 声明 + 脚本 register/handler）。
//! （示例位于 crates/engine/，经 `../../plugins/demo` 回到 workspace 根。）

use serde_json::json;
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::rune::ScriptPlugin;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/demo");
    let plugin = ScriptPlugin::from_dir(&dir)?;
    println!(
        "加载脚本插件：{}（requires={:?}）",
        plugin.manifest.namespace, plugin.manifest.requires
    );

    let kernel = KernelBuilder::new().script_plugin(plugin).build()?;
    println!("模型可见工具：{:?}", kernel.list_tools());

    // 用户触发脚本工具（懒加载：首次命中才执行脚本 register() 绑定）。
    let out = kernel
        .call_command("demo::ping", json!({ "hello": "world" }))
        .await
        .map_err(|e| std::io::Error::other(format!("{e:?}")))?;
    println!("demo::ping 结果：{out}");
    Ok(())
}
