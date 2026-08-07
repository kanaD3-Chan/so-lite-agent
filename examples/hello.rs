//! M2 验收示例：`cargo add so-lite-agent` 后十行代码跑通 hello 回合（默认 mock 模型）。

use std::sync::Arc;

use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::events::MemoryEventSink;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events)
        .system_prompt(|| "你是 so-lite-agent，一个通用 Agent 运行时。".to_string())
        .build()?;

    let outcome = kernel.send_user_message(Default::default(), "你好").await?;
    println!(
        "stop_reason={:?} tool_calls={}",
        outcome.stop_reason, outcome.tool_calls
    );
    for msg in &outcome.messages {
        println!("{msg:?}");
    }
    Ok(())
}
