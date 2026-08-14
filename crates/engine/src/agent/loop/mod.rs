//! Agent loop：LLM 唯一决策者，kernel 执行工具调用。
//!
//! - `types`：回合输入/输出与停止原因（TurnInput / TurnOutcome / StopReason …）；
//! - `engine`：默认实现 DefaultAgentLoop 主循环（护栏、压缩、中断消费、session::switch、context::compact）；
//! - `hooks`：事件决策分离（P2）——`LoopHook` 决策链（before_tool / after_tool /
//!   before_model_request / turn_stopping），与 `EventSink` 观察分离；
//! - `tests`：压缩与中断消费的回归测试。
//!
//! Capability seam（ADR-0006）：[`AgentLoop`] trait 是可替换能力的 **Definition**——
//! kernel 只依赖 `Arc<dyn AgentLoop>`，换 loop 不换内核其余部分；
//! [`DefaultAgentLoop`] 是默认 **Provider**（经 `KernelBuilder::loop_engine` 可注入替换）。

mod engine;
mod hooks;
mod types;

pub use engine::DefaultAgentLoop;
pub use hooks::{HookChain, LoopHook, ToolVerdict};
pub use types::{CompactionInfo, InterruptReason, LoopError, StopReason, TurnInput, TurnOutcome};

/// Agent loop 能力契约（Capability seam Definition）：一次回合的执行驱动器。
///
/// LLM 是唯一决策者，loop 负责流式消费模型输出、串行执行工具调用、护栏、
/// 压缩与中断消费，返回 `TurnOutcome`。自定义实现需自行消费 `InterruptBus`
/// （经 `Kernel::interrupt_bus` 可达）；P1 不把 bus 塞进 trait。
#[async_trait::async_trait]
pub trait AgentLoop: Send + Sync {
    /// 执行一次完整回合（调用方负责消息落盘与压缩接入存储链）。
    async fn run_turn(&self, input: TurnInput) -> Result<TurnOutcome, LoopError>;
}

#[cfg(test)]
mod tests;
