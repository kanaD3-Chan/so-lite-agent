//! so-lite-agent：开箱即用的通用 Agent 运行时（workspace 引擎 crate，ADR-0008）。
//!
//! 参考 earendil-works/pi 的分层（模型 Provider 层 + Agent core 层随包提供，
//! 领域层由使用方编写），模块边界分层：
//! - `model`：ModelService 抽象、流式事件归一化、Provider 注册表、Mock 桩；
//! - `agent`：agent loop、工具调度（dispatch）、会话通用语义（session）；
//! - `contract` / `registry` / `context`：两段式插件契约与注册表；
//! - `services`：ServiceId、SessionStore 契约与 InMemory/Jsonl 实现、ServiceHandles；
//! - `events` / `audit` / `message`：事件流、审计、消息树；
//! - `builder`：KernelBuilder 装配入口与 Kernel 直连 API；
//! - `rune`（feature rune-plugins）：Rune 脚本用户插件桥（ADR-0006）。
//!
//! 内核插件（ADR-0006 Linus 模式）在 workspace 里是**独立 crate**
//! （`crates/plugin-*/`，ADR-0008），不在本 crate 内；本 crate 只提供契约
//! （`KernelPlugin` / `KernelDescriptor` / `SessionStore` 等）。
//!
//! `extern crate self`：让参考模板（`docs/plugin-dev/reference/`）里
//! `so_lite_agent::…` 的路径在本 crate 的编译锚定测试中也能解析。

extern crate self as so_lite_agent;

pub mod agent;
pub mod audit;
pub mod builder;
pub mod context;
pub mod contract;
pub mod events;
pub mod logger;
pub mod message;
pub mod model;
pub mod registry;
pub mod rpc;
#[cfg(feature = "rune-plugins")]
pub mod rune;
pub mod services;

#[cfg(test)]
mod tests {
    // 编译锚定：内核插件参考模板必须始终与真实契约一致（不注册，仅编译检查）。
    // 模板面向"插件 crate"视角（`so_lite_agent::…` 路径），`extern crate self`
    // 让它在引擎内也能通过同样路径解析。
    include!("../../../docs/plugin-dev/reference/kernel_plugin.rs");

    #[test]
    fn kernel_plugin_reference_typechecks() {
        let _ = descriptor();
    }
}
