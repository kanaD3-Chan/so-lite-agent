# 转向可执行 harness：Linus 内核模式 + Rune 用户插件 + 浏览器 Web GUI（pivot）

DeepSeek Harness（MIT）发布后，其「一切皆插件 + 能力可替换（agent loop 本身也是插件）+ 事件即扩展点 + 运行时组合」被验证为通用 agent 运行时的正确形态。对照本 crate：能力三角（Service Definition / Provider / Consumer）已有雏形但未形式化，loop 焊死在 Kernel 内不可替换，事件只做播报没有拦截语义，装配是编译期代码链。**定位决策**：so-lite-agent 从「cargo add 库」转向「业务无关的通用 Agent 可执行文件」——二进制 `sl-agent` 以**浏览器 Web 应用**形态交付（HTTP/WS 服务 + 内嵌前端，对标 dsh web），单二进制分发；**不发布 crates.io**（ADR-0009 修订：当初设想的"crate 库形态保留，mistake-agent 以 crate 集成"从未兑现，正式废除）。ADR-0004（通用运行时与业务分离）不破：可执行文件内置的是**能力**（会话/模型/RPC/通用工具），不是业务。

**内核插件（Linus 模式）**：官方二进制的内核能力**只由维护者编译**，不存在任何动态内核扩展机制——无 cdylib、无签名脚本、无运行时加载面。第三方需要新内核能力 = 交 PR（经受信审查合入官方二进制）或 fork 自治。内核插件的职责是**收紧权限与能力供给**（provides 服务、特权入口、护栏/压缩等运行时能力）；用户插件的职责是**扩展 Agent 业务功能**（工具/命令/事件回调）。crate 库形态下，使用方编译自己的二进制时可自写 Rust 内核插件编入（受信集成路径，与官方二进制无关）。理由：内核层是信任边界的根，完整性靠「**没有可加载物**」保证——参考 Linux .ko（加载前验签、加载后完全信任、ring 0 无沙箱），我们选择"不加载"，因此连签名机制都不需要，比"签名即信任即全权"更强且零机制成本。

**用户插件（eBPF 模型）**：以 **Rune 脚本**扩展业务功能。Rune 是内存安全的嵌入 VM（一等 async、serde 互通、热重载、栈隔离；rune 0.14.x，MIT OR Apache-2.0）；脚本只能经宿主安装的函数触达外部世界。Rune 用户插件走同一两段式契约：info() 结构化声明（namespace/requires/入口点/策略/enabled），register() 经宿主函数把脚本函数包装为 ToolHandler；**requires 决定宿主装哪些函数**——脚本结构性拿不到未声明能力（防越权），脚本明文可改也不怕（篡改只能在白名单内作恶；防篡改不是用户层目标）。Rust 用户插件路径保留（高级/编译期场景）。参考 Linux eBPF：静态验证 + helper 白名单 = 安全 VM + 宿主函数白名单。

**威胁模型（两个正交维度）**：防篡改（完整性）→ 内核层由编译产物保证，无动态加载面；防越权（能力边界）→ 用户层由 Rune VM + requires 函数白名单保证。两层都不引入签名机制。

**能力 seam 化（本轮组合机制不变）**：可替换能力形式化为三角角色，第一步把 AgentLoop 抽象为可替换 trait + 默认实现（对标 dsh 的 `ctx.agentLoop`），换 loop 不换内核其余部分。装配仍为 KernelBuilder 显式链式注册（ADR-0005 不动）；配置驱动组合与前端工程化（Vue/TS）推迟评估。ADR-0002（混合服务标识）、0003（builder 默认补齐）、0004（通用边界）全部保持。

**mistake-agent**：保持独立二进制消费 crate（其内核级业务 memory/compute 内核插件留在自己二进制，不进入 sl-agent 官方二进制）；非特权业务未来可评估迁移为 sl-agent 上的 Rune 用户插件（P3+）。

**里程碑**：pivot 取代 M5 冻结计划（crates.io 上架与 mistake-agent 切换后移至 P3+ 评估）；plan.md 重排为 P1（seam 化 + Rune 用户插件桥 + 服务端入口 + 浏览器最小聊天页）→ P2（Rune 一等支持 + GUI 长全）→ P3（web 打磨/配置/发行/迁移评估）。

**备选被否**：动态内核扩展（cdylib 或签名脚本，.ko 模型）——Linus 模式下无此需求，"签名即全权"还引入密钥管理与审计面；WASM 插件——平台坑与构建链成本远超脚本方案；移植 Cordis 风格 IoC 框架——Rust 无等价物，重写成本高，且与两段式契约/信任边界重叠；立即引入配置驱动组合——与 ADR-0005 冲突，推迟评估；Tauri 桌面壳——浏览器 Web 先行，桌面壳只作为未来薄包装（非重写）。

**影响**：新增 rune 依赖（MIT/Apache 双许可，与 AGPL-3.0 无冲突，不涉抄码）；服务端依赖（HTTP/WS，如 axum）经 feature 门控挂在二进制侧，不进 crate 引擎公共面；前端工程化后置（P1 以零工具链纯 HTML/JS 最小聊天页起步）；docs/plugin-dev.md 与 examples 需补 Rune 路径；CONTEXT.md 词条按本 ADR 更新。
