# workspace 化：内核插件独立 crate，整个 workspace 编译成一个二进制

pivot（ADR-0006）后，`sl-agent` 成为主交付形态；P2 存储转向（ADR-0007）落地了
会话事实日志与 JSONL 落盘。**协作决策（2026-08-14）**：仓库从**单 crate** 改为
**Cargo workspace**——内核态插件由维护者以**独立 crate** 编写，整个 workspace
最终**编译成一个二进制**（`sl-agent`）。这是对 ADR-0006「单 crate」与 AGENTS.md
红线「单 crate」的**修订**。

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

## 结构草案（P3+ 落地，不在 P2 执行）

```
so-lite-agent/                 workspace 根（[workspace] members）
├─ crates/
│  ├─ engine/                  ← 现 crate 本体（so-lite-agent，通用运行时）
│  │  ├─ src/                  ← 现有 src/ 迁入；src/plugin/ 移出
│  │  └─ Cargo.toml            ← 保留默认 features 空、server/rune-plugins 门控
│  ├─ plugin-storage/          ← 内核插件：JSONL 会话事实日志（从 src/plugin/storage/ 迁出）
│  │  └─ src/lib.rs
│  └─ bin/                     ← sl-agent 可执行（现 src/bin/sl-agent/ 迁入）
│     └─ src/main.rs
├─ build.rs                    ← 移至 workspace 根或各 crate（插件自动发现改扫描 crates/）
├─ web/  plugins/  docs/       ← 不变
└─ Cargo.toml                  ← workspace 元数据 + [workspace.dependencies]
```

- **引擎不依赖插件 crate**：`engine` 只提供契约（`SessionStore` trait 等）；
  插件 crate 依赖 `engine`（实现契约）；`bin` 依赖 `engine` + 全部内置插件
  crate，把它们装配进官方二进制。装配点 = `bin` 的 `main`（对应现在
  `KernelBuilder::register_kernel_plugin` 循环）。
- **build.rs 插件自动发现（ADR-0036）改造**：从扫描 `src/plugin/` 改为
  workspace 级约定——`crates/plugin-*/` 目录即插件（或保留 `src/plugin/`
  在 engine 内作为**可选内置**，插件 crate 用 `[workspace.dependencies]`
  显式挂载）。两种方案 P3 定夺；原则是"新增内核插件不改任何装配代码"。
- **crate 边界即信任边界**：插件 crate 可以带自己的依赖（如 storage 的
  `uuid`、`chrono`），不回流引擎；引擎 crate 的公共面（RPC 子集、事件子集、
  ServiceId、SessionStore 契约）保持通用。
- **lib 形态保留**：`crates/engine` 仍是可 `cargo add so-lite-agent` 的库
  （M2 验收不变）；workspace 化不影响库消费者。

## 影响与边界

- **P2 不重构**：本 ADR 只记录决策与结构草案；P2 存储转向按已落地的
  `src/plugin/` 目录形态继续（build.rs 自动发现已就位），workspace 迁移
  排在 P3（与分发形态评估、mistake-agent 迁移评估同批）。
- **红线修订**：AGENTS.md「单 crate」→「workspace 多 crate，最终单二进制；
  引擎 crate 保持业务无关（ADR-0004）」。内核插件从"目录"升级为"crate"，
  Linus 模式语义不变（仅维护者编译进官方二进制）。
- **文档同步**：本 ADR + AGENTS.md（红线） + CONTEXT.md（新增 workspace 词条）
  + plan.md（P3 增加 workspace 化条目）。

## 被否备选

- **维持单 crate + src/plugin/ 目录**：编译边界模糊，插件依赖混入引擎
  依赖面，业务插件迟早污染通用引擎；
- **插件 crate 发布到 crates.io 单独分发**：Linus 模式要求插件只随官方
  二进制分发，不鼓励第三方独立安装内核插件；workspace 内部 crate 足够。
