//! 事件决策分离（P2，调研报告路线 1）：`EventSink` 保留观察，`LoopHook` 提供决策。
//!
//! 对照 DSH waterfall：`EventSink` 是播报（fire-and-forget，不可阻断），
//! `LoopHook` 是决策链（内核插件/使用方实现，可在工具调用前后、模型请求前、
//! 回合停时介入——拒绝、改写或观察）。
//!
//! 四个 hook（优先级排序，全部默认放行/观察，实现方按需重载）：
//! - [`LoopHook::before_tool`]：工具执行前——可**改写参数**或**拒绝**（错误回喂模型）；
//! - [`LoopHook::after_tool`]：工具执行后——观察结果（告警/审计），不可阻断；
//! - [`LoopHook::before_model_request`]：模型请求前——观察消息面（消息数/工具集）；
//! - [`LoopHook::turn_stopping`]：回合停时——观察停止原因。
//!
//! hook 链顺序 = 注册顺序；任一 hook 返回 [`ToolVerdict::Deny`] 短路后续 hook 与
//! 工具执行。Rune 脚本插件**不直接**实现本 trait（脚本无具体类型）；观察/决策
//! 需求经 `Event::Custom` 上浮或由下游 Rust 侧实现本 trait。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use super::StopReason;
use crate::contract::ToolError;
use crate::message::Message;

/// 工具执行前的决策结果。
#[derive(Debug, Clone)]
pub enum ToolVerdict {
    /// 放行；`Some(params)` = 改写后的参数（空 = 用原参数）。
    Allow(Option<Value>),
    /// 拒绝：错误信息回喂模型（工具不执行）。
    Deny(String),
}

impl Default for ToolVerdict {
    fn default() -> Self {
        Self::Allow(None)
    }
}

/// 循环决策 hook（事件决策分离，P2）：内核插件/使用方在关键决策点介入。
#[async_trait]
pub trait LoopHook: Send + Sync {
    /// 工具执行前：可改写参数或拒绝。链式调用，任一 Deny 短路。
    async fn before_tool(&self, _entry: &str, _params: &Value) -> ToolVerdict {
        ToolVerdict::Allow(None)
    }

    /// 工具执行后：观察结果（不可阻断）。`result` 为工具返回值或错误。
    async fn after_tool(&self, _entry: &str, _result: &Result<Value, ToolError>) {}

    /// 模型请求前：观察消息面（不可阻断）。
    async fn before_model_request(&self, _messages: &[Message]) {}

    /// 回合停时：观察停止原因（不可阻断）。
    async fn turn_stopping(&self, _stop_reason: &StopReason) {}
}

/// hook 集合：按注册顺序链式执行。
pub type HookChain = Vec<Arc<dyn LoopHook>>;

/// 运行 before_tool 链：返回第一个非放行结果（改写参数或拒绝）；全放行 = 原参数。
pub(crate) async fn run_before_tool(
    hooks: &HookChain,
    entry: &str,
    params: &Value,
) -> Result<Value, ToolError> {
    let mut current = params.clone();
    for hook in hooks {
        match hook.before_tool(entry, &current).await {
            ToolVerdict::Allow(Some(rewritten)) => current = rewritten,
            ToolVerdict::Allow(None) => {}
            ToolVerdict::Deny(reason) => {
                return Err(ToolError::handler(reason));
            }
        }
    }
    Ok(current)
}

/// 运行 after_tool 链（观察，不阻断）。
pub(crate) async fn run_after_tool(
    hooks: &HookChain,
    entry: &str,
    result: &Result<Value, ToolError>,
) {
    for hook in hooks {
        hook.after_tool(entry, result).await;
    }
}

/// 运行 before_model_request 链（观察）。
pub(crate) async fn run_before_model_request(hooks: &HookChain, messages: &[Message]) {
    for hook in hooks {
        hook.before_model_request(messages).await;
    }
}

/// 运行 turn_stopping 链（观察）。
pub(crate) async fn run_turn_stopping(hooks: &HookChain, stop_reason: &StopReason) {
    for hook in hooks {
        hook.turn_stopping(stop_reason).await;
    }
}
