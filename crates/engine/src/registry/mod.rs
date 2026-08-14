//! 注册表：两段式插件契约（`plugin`）+ 启动 fail-fast 校验与懒注册（`core`）。

mod core;
mod plugin;

pub use core::Registry;
pub use plugin::{
    EntryKind, Handler, KernelDescriptor, KernelPlugin, PluginDescriptor, RegisteredEntry,
    UserPlugin, tool_def,
};

#[cfg(test)]
mod tests;
