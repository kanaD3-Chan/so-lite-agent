# AGENTS.md

## 项目速览

So Lite Agent（官方简写 **SL Agent**，crate 名 `so-lite-agent`）是**可执行文件项目**（ADR-0009，不发布 crates.io）：agent loop、工具注册与调度、会话生命周期、模型 Provider 抽象与通用 RPC 随源码提供；**内核插件与用户插件由使用方自行编写**（fork 定制者写 Rust 内核插件 / sl-agent 扩展者写 Rune 脚本）。Rust 2024 edition；仓库为 Cargo workspace（ADR-0008，P3 落地），P3 前保持单 crate。主交付是业务无关的通用 Agent API 服务（二进制 `sl-agent`，HTTP/WS；**前后端分离**——官方参考前端 = `frontend/` React 工程，ADR-0010）与 Rune 脚本用户插件路径，内核能力仅由维护者编译进官方二进制（Linus 模式，见 [docs/adr/0006](docs/adr/0006-pivot-harness-and-rune.md)）。mistake-agent 是本仓库的参考实现（保持独立二进制，见 [docs/adr/0001](docs/adr/0001-independent-repo-skip-m1.md)）。基于本项目开发自己的 Agent 见 [docs/agent-dev-guide.md](docs/agent-dev-guide.md)。

## 文档启动流程（每次开始工作前执行）

按顺序读文档，读完先向协作开发者确认本次任务（做什么 / 范围 / 验收标准），确认后再动手：

1. **README.md** —— 定位、快速开始、模块一页、里程碑
2. **CONTEXT.md** —— 术语表；README 里不懂的词在这里查
3. **docs/plan.md** —— 里程碑与验收状态
4. **docs/adr/** —— 决策留痕；改设计必须新增 ADR（见开发约定）

任务相关细节按「读文档路由」精确定位。

## 读文档路由：做什么 → 读什么

| 你要做什么 | 先去读 | 重点内容 |
|---|---|---|
| 基于本项目开发自己的 Agent | docs/agent-dev-guide.md | 两条主线（sl-agent 扩展者 / fork 定制者）选型与步骤 |
| 写 / 改插件 | docs/plugin-dev.md + examples/ | 两段式契约、enabled、目录编排、句柄注入 |
| 写 / 改内核 | docs/kernel-dev.md + src/agent/、src/registry/ | 模块地图、装配、调用链、扩展路径 |
| 接模型 Provider | docs/api.md §5 + src/model/ | ModelService、注册表、内置适配器 |
| 改 RPC / 事件 | docs/api.md §3-§4 + src/rpc.rs、src/events.rs | Method 子集、RpcExtension、Event::Custom |
| 改设计 / 做架构决策 | docs/adr/ 全部 + CONTEXT.md | 决策留痕；新决策新增 ADR |
| 抄开源代码 | LICENSE + README | 保留许可声明、注明来源 |

## 常用命令

```bash
cargo check
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo run --example hello
cargo run --example script_plugin --features rune-plugins
cargo run --bin sl-agent --features server,rune-plugins
cargo test --test live_api -- --ignored   # 真实 API（key 只从本地配置读取，输出不得打印密钥）
```

## 架构红线（改代码时逐条遵守）

- **通用运行时与业务分离**：错题、记忆、验算、settings 等业务领域类型与语义不进引擎 crate（ADR-0004）。仓库为 **Cargo workspace**（ADR-0008）：内核插件以独立 crate 编写（`crates/plugin-*/`），整个 workspace 最终编译成**一个二进制**（`sl-agent`）；P3 前保持单 crate + `src/plugin/` 目录形态。
- 能力边界：用户插件只经 `requires` 声明的服务句柄；内核插件经 `KernelContext` 拿全量句柄；不引入全局可变状态绕过句柄（`ProviderRegistry` 实例由使用方持有）。
- CallerPolicy：`UserAndModel` 工具模型可调、用户必可调；`UserOnly` 不进模型工具列表，调度层再拒一次（双墙）。
- 入口点命名 `namespace::tool`：插件只写短名，kernel 拼全名；wire name（`::` → `__`）全局唯一，撞名由注册表拒绝。
- `enabled` 缺省 **false**：插件必须显式 `enabled: true` 才注册；禁用插件保留在代码/聚合点中，注册表静默跳过（ADR-0005）。
- 事件/审计/中断只收通用子集；业务语义走 `Event::Custom` / `Method::Custom` / `RpcExtension`（ADR-0004）。
- 审计默认全覆盖，敏感值（API key 等）经 `redact_secret` 脱敏。
- 默认服务（`InMemorySessionStore` / `MockModelService`）只服务开箱即用，不因默认值放宽注册校验（ADR-0003）。
- 抄开源代码必须保留原许可证声明并在文档注明来源。

## 开发约定

- 提交信息用简洁中文描述（如 `feat(builder): 注入摘要器与会话切换钩子`）。
- 改动必须通过 `cargo test --all-targets --all-features` 与 `cargo clippy --all-targets --all-features -- -D warnings`（`--all-features` 覆盖 rune-plugins / server 门控代码）。
- 改设计不留痕 = 没改：必须同步 CONTEXT.md 或新增 docs/adr/。
- **代码改动必须同步文档**：代码修改在测试通过后，凡受影响的文档必须同步修改；文档同步完成前任务不得视为完成。
- **职责先行的模块组织**：新功能先按职责规划模块边界；`mod.rs` 只负责公共面、装配与 `pub use` 重导出，职责实现放子模块；~400 行只是审查预警线，不是拆分触发条件。拆分保持外部引用稳定、零行为变化，并通过全量测试复验。
- docs 里的代码示例保持可编译/可运行（优先以 `examples/` 为准）。
