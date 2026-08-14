# 不发布 crates.io：定位从库项目转为可执行文件项目

**协作决策（2026-08-14）**：so-lite-agent **不再上传 crates.io**。pivot（ADR-0006）
后项目定位已是「业务无关的通用 Agent 可执行文件」（二进制 `sl-agent`，浏览器
Web 应用形态），而非「cargo add 库」；ADR-0006/0008 中"crate 库形态保留
（mistake-agent 等消费方以 crate 集成）"的承诺**从未兑现**（mistake-agent 保持
独立二进制，不依赖本 crate）。本决策正式废除 crates.io 上架计划。

## 决策内容

- **不发布 crates.io**：源码仍是单个 Rust crate（P3 workspace 化后 engine 仍是
  crate），但**只作为仓库内部/依赖路径存在**，不做公共发布、不维护 semver 承诺、
  不面向 `cargo add so-lite-agent` 的第三方库消费者；
- **开发自己 agent 的方式**（对应用户选定的主线）：
  1. **sl-agent 扩展者**：跑官方二进制，写 **Rune 脚本用户插件**（目录形态：
     manifest.json + plugin.rn），无需 cargo、无需 fork；
  2. **fork 定制者**：fork 仓库改**内核插件**（`crates/plugin-*/`，Linus 模式，
     build.rs 自动发现，ADR-0036 改造）与装配，编译自己的 `sl-agent` 二进制；
- **mistake-agent**：维持独立二进制（其内核级业务留在自己二进制），不再存在
  "以 crate 集成"的依赖关系；参考实现价值不变。

## 动机

- 主交付形态 pivot 后已是可执行文件，库分发没有消费方（唯一假设的消费方
  mistake-agent 从未接入 crate）；
- crates.io 发布附带成本与约束（semver、文档、yank 纪律、依赖审查），
  对无消费方的库是纯负担；
- 与 workspace 化（ADR-0008）一致：仓库以"编译成一个二进制"为目标，
  内部 crate 边界服务于组织，不服务于公共分发。

## 影响

- **文档同步**：README（快速开始以 `sl-agent` 为主线，"cargo add" 表述降级为
  源码依赖说明）、plan.md（P3 删除 crates.io 上架评估项）、api.md（§1 面向
  fork 定制者而非库消费者）、AGENTS.md（项目速览）、ADR-0006/0008（修订
  "crate 库形态保留"表述）；
- 新增 **agent-dev-guide.md**：面向 sl-agent 扩展者 + fork 定制者的端到端
  开发流程（最小可跑 → Rust 业务插件 → Rune 插件）。

## 被否备选

- **照常发布 crates.io**：无消费方，纯负担；
- **保留"库形态"作为宣传**：定位模糊，误导潜在用户以为可以 `cargo add` 集成。
