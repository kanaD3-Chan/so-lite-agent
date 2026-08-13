//! 服务契约与受控句柄：
//! - `session`：SessionStore 通用契约 + InMemory 默认实现；
//! - `dynamic`：DynamicService 动态调用接口（Rune 脚本插件访问通道，ADR-0006）；
//! - `handles`：ServiceId 与 ServiceHandles 类型化容器（ADR-0002）。

mod dynamic;
mod handles;
mod session;

pub use dynamic::DynamicService;
pub use handles::{ServiceHandles, ServiceId};
pub use session::{InMemorySessionStore, SessionError, SessionHandle, SessionStore, active_chain};
