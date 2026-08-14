//! 服务契约与受控句柄：
//! - `session`：SessionStore 通用契约 + InMemory 默认实现；
//! - `dynamic`：DynamicService 动态调用接口（Rune 脚本插件访问通道，ADR-0006）；
//! - `handles`：ServiceId 与 ServiceHandles 类型化容器（ADR-0002）。

mod dynamic;
mod handles;
mod jsonl;
mod session;

pub use dynamic::DynamicService;
pub use handles::{ServiceHandles, ServiceId};
pub use jsonl::JsonlSessionStore;
pub use session::{
    InMemorySessionStore, SessionError, SessionEvent, SessionHandle, SessionStore, SurfaceFold,
    SurfaceOp, chain_from, fold_surface, project_messages,
};
