//! Agent 核心调度层：agent loop、工具调度、会话通用语义。
//!
//! - `dispatch`：统一工具执行（CallerPolicy 双墙、懒注册、schema 校验、超时/取消、审计）；
//! - `session`：SessionKey/Goal/中断总线/摘要器/会话切换钩子；
//! - `r#loop`：agent loop（LLM 唯一决策者，串行工具执行，护栏/压缩/中断消费）。

pub mod dispatch;
pub mod session;

#[path = "loop.rs"]
pub mod r#loop;
