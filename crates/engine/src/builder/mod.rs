//! KernelBuilder 装配入口与 Kernel 直连 API。
//!
//! - `assembly`：KernelBuilder——默认服务自动补齐、插件注册 fail-fast；
//! - `kernel`：组装完成的 Kernel——直连 Rust API 与通用 RPC 入口。

mod assembly;
mod kernel;

pub use assembly::KernelBuilder;
pub use kernel::Kernel;

#[cfg(test)]
mod tests;
