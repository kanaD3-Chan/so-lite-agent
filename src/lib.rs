//! so-lite-agent：开箱即用的通用 Agent 运行时。
//!
//! 参考 earendil-works/pi 的分层（模型 Provider 层 + Agent core 层随包提供，
//! 领域层由使用方编写），单 crate 内以模块边界分层：
//! - `model`：ModelService 抽象、流式事件归一化、Provider 注册表、Mock 桩；
//! - `agent`：agent loop、工具调度（dispatch）、会话通用语义（session）；
//! - `contract` / `registry` / `context`：两段式插件契约与注册表；
//! - `services`：ServiceId、SessionStore 契约与 InMemory 默认实现、ServiceHandles；
//! - `events` / `audit` / `message`：事件流、审计、消息树；
//! - `builder`：KernelBuilder 装配入口与 Kernel 直连 API。

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
pub mod services;
