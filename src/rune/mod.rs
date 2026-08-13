//! Rune 脚本宿主（ADR-0006 用户插件路径的基础设施）。
//!
//! eBPF 模型：Rune 是内存安全的嵌入 VM，脚本只能经**宿主安装的函数**触达外部世界；
//! 未安装的函数 = prepare 编译失败（结构性拿不到）。本模块提供通用宿主机制：
//!
//! - [`vm::ScriptVm`]：一次编译、按调用新建 `Vm` 执行（无共享可变状态），
//!   `async_complete` 驱动异步脚本（脚本可 `await` 异步宿主函数）；
//! - [`host`]：宿主函数安装骨架（动态闭包经 `Module::function` 安装，含 async）；
//! - rune `Value` 与 `serde_json::Value` 双向转换（rune Value 自带
//!   Serialize/Deserialize，无需额外 feature）。
//!
//! 插件契约在 [`plugin`]：manifest.json（纯数据 info 声明）+ plugin.rn
//! （register + handlers），按 requires 结构性裁剪宿主函数
//! （P1 检查点结论 a1/b1/c2/d1）。
//!
//! 许可：rune（MIT OR Apache-2.0）与本 crate（AGPL-3.0）兼容；本模块为独立实现，
//! 不复制 rune 源码（ADR-0006 影响分析）。

mod host;
mod plugin;
mod vm;

pub use host::HostError;
pub use plugin::{ScriptPlugin, ScriptPluginHandle};
pub use vm::{CallError, CompileError, ScriptVm};
