//! 服务契约与受控句柄：
//! - `session`：SessionStore 通用契约 + InMemory 默认实现；
//! - `handles`：ServiceId 与 ServiceHandles 类型化容器（ADR-0002）。

mod handles;
mod session;

pub use handles::{ServiceHandles, ServiceId};
pub use session::{InMemorySessionStore, SessionError, SessionHandle, SessionStore, active_chain};
