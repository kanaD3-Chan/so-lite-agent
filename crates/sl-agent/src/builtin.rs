//! 内置内核插件注册清单（ADR-0036 改造）：由 `build.rs` 扫描 `crates/plugin-*/`
//! 自动生成（装配零样板——新增插件不改任何 Rust 代码，见 build.rs 头注释）。

include!(concat!(env!("OUT_DIR"), "/builtin_kernel_plugins.rs"));
