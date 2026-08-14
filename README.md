# So Lite Agent

So Lite Agent 是**可执行文件项目**（官方简写 **SL Agent**，ADR-0009 不发布
crates.io）：主交付 = 业务无关的通用 Agent API 服务（二进制 `sl-agent`，HTTP/WS
API，**前后端分离**：官方参考前端是 `frontend/` 的 React 工程，ADR-0010），第三方
以 Rune 脚本用户插件扩展、无需 cargo 构建；内核能力仅由维护者编译进官方二进制
（Linus 模式，见 [ADR-0006](docs/adr/0006-pivot-harness-and-rune.md)）。仓库是
**Cargo workspace**（ADR-0008）：引擎 crate `so-lite-agent`（`crates/engine`，
业务无关的通用运行时）+ 内核插件 crate（`crates/plugin-*/`）+ 官方二进制
（`crates/sl-agent`），最终**编译成一个二进制** `sl-agent`；引擎作为仓库内部依赖
存在，不做公共发布。

参考 [earendil-works/pi](https://github.com/earendil-works/pi) 的分层——模型
Provider 层与 Agent core 层内置随包，领域层（业务插件）由使用方编写。
mistake-agent 是本仓库的参考实现（保持独立二进制，见
[ADR-0001](docs/adr/0001-independent-repo-skip-m1.md)）。

> **要开发自己的 Agent？** 看 [docs/agent-dev-guide.md](docs/agent-dev-guide.md)：
> 两条路线（sl-agent 扩展者 = 写 Rune 脚本 / fork 定制者 = 改 Rust 内核插件），
> 从零到跑通。内核细节（[docs/kernel-dev.md](docs/kernel-dev.md)）只给维护者/
> 深度集成者，接口参考见 [docs/api.md](docs/api.md)。

## 快速开始（fork / 源码运行）

```bash
cargo test --all-targets --all-features   # 门禁自检
cargo run -p sl-agent                     # API 服务（http://127.0.0.1:8080）
```

```rust
// crates/engine/examples/hello.rs：最小内核 + hello 回合（fork 定制者在自己的二进制里这么装配）
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::events::MemoryEventSink;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events)
        .system_prompt(|| "你是 so-lite-agent。".to_string())
        .build()?;
    let outcome = kernel.send_user_message(Default::default(), "你好").await?;
    println!("{outcome:?}");
    Ok(())
}
```

默认装配：`InMemorySessionStore` + `MockModelService`（固定文本桩）+ `MemoryEventSink` + `MemoryAuditSink`，不配任何东西也能跑通 hello 回合。

接真实模型（OpenAI 兼容端点，如 DeepSeek）：

```rust
let registry = ProviderRegistry::new();
let service = register_openai_compatible(&registry, "deepseek", OpenAiCompatibleConfig {
    api_url: "https://api.deepseek.com".into(),
    api_key: key.into(),
    model: "deepseek-v4-flash".into(),
    transport: OpenAiTransport::Responses,
    ..Default::default()
})?;
// KernelBuilder::new().service_handles(ServiceHandles::default().with_model(ModelHandle::new(service, timeout, auditor)))
```

Anthropic 兼容端点用 `AnthropicModelService`；自定义端点就是改 `api_url`。

## 快速开始：`sl-agent` API 服务（前后端分离，ADR-0010）

```bash
cargo run -p sl-agent   # API 服务：http://127.0.0.1:8080
```

零配置即跑通 hello 回合（默认 `MockModelService`）；接真实模型：

```bash
SL_AGENT_API_URL=https://api.deepseek.com SL_AGENT_API_KEY=xxx SL_AGENT_MODEL=deepseek-chat \
  SL_AGENT_PORT=8080 cargo run -p sl-agent
```

**前端独立部署**（前后端分离，ADR-0010）：sl-agent 只提供 API（`/ws` + `/healthz`），
不内嵌页面。官方参考前端是 React 工程：

```bash
cd frontend && npm install && npm run dev    # http://localhost:5173，自动连 WS
```

Rune 脚本用户插件放 `./plugins/`（一插件一目录：`manifest.json` 声明 + `plugin.rn`
脚本，目录名即 namespace，见 [docs/plugin-dev.md](docs/plugin-dev.md)）；本仓库自带
`plugins/demo/` 示例。内核能力仅由维护者编译进官方二进制（Linus 模式），
第三方扩展业务 = 写 Rune 脚本，无需 cargo 构建。

> **要基于本项目开发自己的 Agent？** 看 [docs/agent-dev-guide.md](docs/agent-dev-guide.md)
> ——按路线选型（sl-agent 扩展者 / fork 定制者）从零到跑通。

## 开发上手

- 只做 Agent：按 [docs/agent-dev-guide.md](docs/agent-dev-guide.md) 选路线（Rune 扩展 / fork 定制）；
- 先跑 [crates/engine/examples/hello.rs](crates/engine/examples/hello.rs)：最小内核 + hello 回合；
- 再跑 [crates/engine/examples/plugins.rs](crates/engine/examples/plugins.rs)：自定义服务 + 内核插件 + 用户插件端到端（脚本化模型模拟两次工具调用）；
- 目录编排见 [crates/engine/examples/folder_plugins](crates/engine/examples/folder_plugins/main.rs)：一插件一目录（mod.rs 契约 + core.rs 实现）+ 聚合点；
- 插件怎么下手：见 [docs/plugin-dev.md](docs/plugin-dev.md)。
- 内核怎么改：见 [docs/kernel-dev.md](docs/kernel-dev.md)；接口/RPC/事件：见 [docs/api.md](docs/api.md)。

## 协作约定

协作者/贡献者先读 [AGENTS.md](AGENTS.md)：文档启动流程、常用命令、架构红线与开发约定。

## 模块一页

workspace（ADR-0008）三个 crate，最终编译成一个二进制 `sl-agent`：

| crate | 职责 |
|---|---|
| `crates/engine`（`so-lite-agent`） | 业务无关的通用 Agent 运行时（下表各模块） |
| `crates/plugin-*` | 内核插件（Linus 模式，ADR-0006/0008）：独立 crate 边界，build.rs 自动发现；首个 `plugin-storage`（JSONL 会话落盘） |
| `crates/sl-agent` | 官方二进制：API 服务（HTTP/WS）+ 内核插件装配 + Rune 脚本插件加载 |

引擎内部模块：

| 模块 | 职责 |
|---|---|
| `agent/loop` | Agent loop（types/engine）：模型流式消费、串行工具执行、护栏、压缩、中断消费 |
| `agent/dispatch` | 统一执行入口：CallerPolicy 双墙、懒注册、schema 校验、超时/取消、审计 |
| `agent/session` | SessionKey/Goal/SessionMeta、中断总线、摘要器、会话切换钩子 |
| `contract` | 入口点元数据、CallerPolicy、ToolError、PluginError |
| `registry`（plugin/core）/ `context` | 两段式插件契约（info + register）、懒注册、模型工具列表过滤 |
| `services`（session/jsonl/handles/dynamic） | SessionStore 事件日志契约、InMemory/JsonlSessionStore、ServiceHandles |
| `model`（contract/handle/providers/mock） | ModelService 抽象、ModelChunk 归一化、ProviderRegistry、Mock |
| `events` / `audit` / `message` | 事件流、审计记录、消息（投影视图） |
| `builder`（assembly/kernel） | KernelBuilder 装配入口 + Kernel 直连 API |

## 里程碑状态

| 阶段 | 内容 | 状态 |
|---|---|---|
| M2 | 新仓库骨架：通用模块 + 默认服务，hello 回合（mock） | ✅（评审通过：消费者实测 + 门禁全绿） |
| M3 | Provider 层：内置适配器 + register_provider | ✅（真实 API 验收通过） |
| M4 | 通用 RPC + KernelBuilder 定型；插件手册/参考模板迁移 | ✅ |
| P1 | pivot 骨架：能力 seam 化（loop 可替换）+ Rune 用户插件桥 + `sl-agent` 服务入口（HTTP/WS + 浏览器最小聊天页） | ✅（2026-08-13 验收：浏览器 hello 回合 + Rune 插件端到端） |
| P2 | 会话事实日志（事件日志 + 遮蔽投影 + JSONL 落盘）+ Rune 一等支持（热重载/超时）+ 事件决策分离（LoopHook）+ GUI 前后端分离（React 参考前端） | ✅（2026-08-14 收尾，见 docs/plan.md） |
| P3 | 分发形态：**workspace 化（✅ 已落地，ADR-0008）** / web 打磨 / 配置组合 / mistake 迁移与切换评估 | 🔶 进行中（workspace 化 ✅） |

> M5（发布与切换）已被 pivot 取代，详见 [docs/plan.md](docs/plan.md)。

详见 [docs/plan.md](docs/plan.md)。

## 开发命令

```bash
cargo check
cargo test --all-targets --all-features   # 含 rune-plugins 门控代码
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
cargo run -p so-lite-agent --example hello
cargo run -p so-lite-agent --example script_plugin --features rune-plugins   # Rune 脚本插件示例
cargo run -p sl-agent     # API 服务（前端见 frontend/README）
```

## 术语

见 [CONTEXT.md](CONTEXT.md)。架构决策见 [docs/adr/](docs/adr/)。

## 许可证

AGPL-3.0（与 mistake-agent 同源）。
