//! Agent loop：LLM 唯一决策者，kernel 执行工具调用。
//!
//! - `types`：回合输入/输出与停止原因（TurnInput / TurnOutcome / StopReason …）；
//! - `engine`：AgentLoop 主循环（护栏、压缩、中断消费、session::switch）；
//! - `tests`：压缩与中断消费的回归测试。

mod engine;
mod types;

pub use engine::AgentLoop;
pub use types::{CompactionInfo, InterruptReason, LoopError, StopReason, TurnInput, TurnOutcome};

#[cfg(test)]
mod tests;
