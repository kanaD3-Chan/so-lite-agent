# 注册保持显式链式装配，插件启用标记 enabled 缺省 false

决策：插件注册保持**显式链式调用**（`register_kernel_plugin` / `register_plugin` 各一行），
不引入宏或链接期自动发现；`Info.enabled` 缺省 **false**——插件必须显式 `enabled: true`
才会注册，未启用插件保留在聚合点/代码中，注册表静默跳过（不做 fail-fast）。

背景：mistake-agent 用 build.rs 编译期插件自动发现 + `disabled` 标记文件（其 ADR-0036）；
本 crate 面向第三方开发者、以 `cargo add` 分发，不应绑定构建脚本，也不应让"新增插件目录"
隐式改变注册表。显式链式装配让 `build()` 的 fail-fast 校验可预期，聚合点即唯一显式清单。

备选被否：

- 属性宏 + 一次聚合：仍需显式清单，收益有限；
- `inventory` / `linkme` 链接期自动收集：隐式全局注册表、平台坑，且破坏构建期
  fail-fast 的可预期性；
- 沿用 `disabled` 标记文件：那是 mistake-agent 编译期 include! 聚合的配套，本 crate
  没有构建期聚合，反向标记语义不成立。

影响：WIP 插件可安全保留在聚合点（不注册）；注册表校验逻辑（namespace/wire 唯一、
requires 可满足、provides 唯一）不因启用标记放宽。
