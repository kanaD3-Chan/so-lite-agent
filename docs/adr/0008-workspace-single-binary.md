# workspace 化：内核插件独立 crate，整个 workspace 编译成一个二进制

pivot（ADR-0006）后，`sl-agent` 成为主交付形态；P2 存储转向（ADR-0007）落地了
会话事实日志与 JSONL 落盘。**协作决策（2026-08-14）**：仓库从**单 crate** 改为
**Cargo workspace**——内核态插件由维护者以**独立 crate** 编写，整个 workspace
最终**编译成一个二进制**（`sl-agent`）。这是对 ADR-0006「单 crate」与 AGENTS.md
红线「单 crate」的**修订**。

> **落地状态（P3，2026-08-14 ✅）**：结构按下方「落地结构」执行完毕——引擎
> `crates/engine` + 插件 `crates/plugin-storage` + 二进制 `crates/sl-agent`；
> build.rs 自动发现改造为**二进制侧扫描 `crates/plugin-*/`**（ADR-0036 改造，
> 生成注册清单，装配零样板）；`server` feature 随 axum 迁入二进制（二进制即服务端，
> 不再 feature 门控）；引擎保留 `rune-plugins`（默认关，二进制启用）；
> examples/tests 随引擎迁入 `crates/engine/`。门禁全绿（test/clippy/fmt）。

## 动机

- **内核插件（Linus 模式）需要一个独立于引擎的编译边界**：当前 `src/plugin/`
  目录形态（build.rs 自动发现，ADR-0036）把插件编进引擎 crate；插件若依赖
  业务领域类型（如 mistake-agent 迁移来的 storage/memory），会污染通用引擎
  crate 的依赖面，与 ADR-0004 业务分离红线冲突。独立 crate 让**插件自己声明
  依赖**，引擎 crate 保持通用、零业务依赖。
- **内核插件即 crate 的形态更贴近"插件"语义**：每个内核插件 = 一个 crate
  （`sl-plugin-storage` 等），有独立版本、独立 feature、独立测试；维护者
  改插件不动引擎，引擎不动插件（Linus 模式从"目录"升级为"crate 边界"）。
- **最终单二进制**：workspace 只是编译组织，分发物仍是 `sl-agent` 一个
  可执行文件——内核插件 crate 作为**依赖**编进官方二进制（Linus 模式：
  只有维护者能加依赖），用户侧感知不变（Rune 脚本用户插件、HTTP/WS、
  内嵌前端都无变化）。

## 落地结构（P3，2026-08-14 已执行）

```
so-lite-agent/                 workspace 根（[workspace] members + workspace.dependencies）
├─ crates/
│  ├─ engine/                  ← 引擎 crate（so-lite-agent，通用运行时）
│  │  ├─ src/                  ← 原 src/ 迁入（plugin/、bin/ 移出）
│  │  ├─ examples/  tests/     ← 原 examples/、tests/ 随引擎迁入
│  │  └─ Cargo.toml            ← 默认 features 空，rune-plugins 门控保留
│  ├─ plugin-storage/          ← 内核插件 crate（sl-plugin-storage，JSONL 会话事实日志）
│  │  └─ src/lib.rs
│  └─ sl-agent/                ← 官方二进制（axum/WS 桥在二进制侧，无 server feature）
│     ├─ build.rs              ← 插件自动发现（扫 ../plugin-*，ADR-0036 改造）
│     ├─ src/main.rs  ws.rs  builtin.rs
│     └─ Cargo.toml            ← 依赖引擎 + 全部内置插件 crate
├─ plugins/  docs/  frontend/  ← 不变
└─ Cargo.toml                  ← workspace 元数据 + [workspace.dependencies]
```

- **引擎不依赖插件 crate**：`engine` 只提供契约（`SessionStore` trait 等）；
  插件 crate 依赖 `engine`（实现契约）；`bin` 依赖 `engine` + 全部内置插件
  crate，把它们装配进官方二进制。装配点 = `bin` 的 `main`（`builtin_kernel_plugins()`
  循环，清单由 build.rs 生成）。
- **build.rs 插件自动发现（ADR-0036 改造，P3 定夺 = 方案 A）**：扫描
  `crates/plugin-*/`（一层深，目录 `plugin-<name>` + 包名约定 `sl-plugin-<name>`），
  读取包名生成 `builtin_kernel_plugins()` 注册清单；新增内核插件 = 建目录 +
  根 members 与 bin 依赖各加一行清单，**注册装配零 Rust 代码改动**。
  （被否：引擎保留 `src/plugin/` 目录形态——插件依赖仍混入引擎依赖面。）
- **crate 边界即信任边界**：插件 crate 可以带自己的依赖（如 storage 的
  `uuid`、`chrono`），不回流引擎；引擎 crate 的公共面（RPC 子集、事件子集、
  ServiceId、SessionStore 契约）保持通用。
- **engine 是内部 crate，不发布**：`crates/engine` 作为 workspace 内部依赖存在，
  不面向 `cargo add` 库消费者（ADR-0009：不发布 crates.io，定位为可执行文件项目）。

## 影响与边界

- **P2 不重构（历史）**：P2 存储转向按当时的 `src/plugin/` 目录形态落地
  （build.rs 自动发现已就位）；workspace 迁移在 P3 执行（本 ADR 落地）。
- **红线修订（已生效）**：AGENTS.md「单 crate」→「workspace 多 crate，最终单二进制；
  引擎 crate 保持业务无关（ADR-0004）」。内核插件从"目录"升级为"crate"，
  Linus 模式语义不变（仅维护者编译进官方二进制）。
- **文档同步（已完成）**：本 ADR + AGENTS.md（红线） + CONTEXT.md（workspace 词条）
  + plan.md（P3 workspace 化 ✅）+ kernel-dev.md §4.4 / plugin-dev.md / api.md /
  agent-dev-guide.md（路径与命令随迁移更新）。

## 被否备选

- **维持单 crate + src/plugin/ 目录**：编译边界模糊，插件依赖混入引擎
  依赖面，业务插件迟早污染通用引擎；
- **插件 crate 发布到 crates.io 单独分发**：Linus 模式要求插件只随官方
  二进制分发，不鼓励第三方独立安装内核插件；workspace 内部 crate 足够。
