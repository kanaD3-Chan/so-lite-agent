//! 内核插件目录（Linus 模式，ADR-0006）：仅维护者编译进官方二进制。
//!
//! - `storage`：JSONL 会话事实日志落盘（服务提供者，无工具入口）；
//! - 构建期自动发现（ADR-0036，build.rs）：目录即插件，新增插件复制
//!   `docs/plugin-dev/reference/kernel-plugin/` 到本目录即可，无需改本文件。

include!(concat!(env!("OUT_DIR"), "/builtin_kernel_plugins.rs"));

#[cfg(test)]
mod tests {
    // 编译锚定：参考模板必须始终与真实契约一致（不注册，仅编译检查）。
    include!("../../docs/plugin-dev/reference/kernel_plugin.rs");

    #[test]
    fn kernel_plugin_reference_typechecks() {
        let _ = descriptor();
    }
}
