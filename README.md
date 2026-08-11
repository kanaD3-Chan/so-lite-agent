# So Lite Agent

So Lite Agent 是开箱即用的通用 Agent 运行时（官方简写 **SL Agent**，crate 名为 `so-lite-agent`）：`cargo add so-lite-agent` 后即可上手开发自己的 Agent。

参考 [earendil-works/pi](https://github.com/earendil-works/pi) 的分层——模型 Provider 层（pi-ai 等价物）与 Agent core 层（pi-agent-core 等价物）内置随包，领域层（业务插件）由使用方编写。mistake-agent 是本仓库的参考实现与消费方（保持独立，不做 M1 解耦，见 [ADR-0001](docs/adr/0001-independent-repo-skip-m1.md)）。

> **只做 Agent 的开发者**：`cargo add so-lite-agent` 后按 [docs/plugin-dev.md](docs/plugin-dev.md)
> 写内核/用户插件即可上手，**无需理解内核设计**；内核细节（[docs/kernel-dev.md](docs/kernel-dev.md)）
> 只给维护者/深度集成者，接口参考见 [docs/api.md](docs/api.md)。

## 快速开始

```bash
cargo add so-lite-agent
```

```rust
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

## 开发上手

- 只做 Agent：`cargo add so-lite-agent` → 按 [docs/plugin-dev.md](docs/plugin-dev.md) 写插件（不需要懂内核）；
- 先跑 [examples/hello.rs](examples/hello.rs)：最小内核 + hello 回合；
- 再跑 [examples/plugins.rs](examples/plugins.rs)：自定义服务 + 内核插件 + 用户插件端到端（脚本化模型模拟两次工具调用）；
- 目录编排见 [examples/folder_plugins](examples/folder_plugins/main.rs)：一插件一目录（mod.rs 契约 + core.rs 实现）+ 聚合点；
- 插件怎么下手：见 [docs/plugin-dev.md](docs/plugin-dev.md)。
- 内核怎么改：见 [docs/kernel-dev.md](docs/kernel-dev.md)；接口/RPC/事件：见 [docs/api.md](docs/api.md)。

## 协作约定

协作者/贡献者先读 [AGENTS.md](AGENTS.md)：文档启动流程、常用命令、架构红线与开发约定。

## 模块一页

| 模块 | 职责 |
|---|---|
| `agent/loop` | Agent loop（types/engine）：模型流式消费、串行工具执行、护栏、压缩、中断消费 |
| `agent/dispatch` | 统一执行入口：CallerPolicy 双墙、懒注册、schema 校验、超时/取消、审计 |
| `agent/session` | SessionKey/Goal/SessionMeta、中断总线、摘要器、会话切换钩子 |
| `contract` | 入口点元数据、CallerPolicy、ToolError、PluginError |
| `registry`（plugin/core）/ `context` | 两段式插件契约（info + register）、懒注册、模型工具列表过滤 |
| `services`（handles/session） | ServiceId、SessionStore 契约 + InMemory 实现、ServiceHandles |
| `model`（contract/handle/providers/mock） | ModelService 抽象、ModelChunk 归一化、ProviderRegistry、Mock |
| `events` / `audit` / `message` | 事件流、审计记录、消息树 |
| `builder`（assembly/kernel） | KernelBuilder 装配入口 + Kernel 直连 API |

## 里程碑状态

| 阶段 | 内容 | 状态 |
|---|---|---|
| M2 | 新仓库骨架：通用模块 + 默认服务，hello 回合（mock） | ✅（评审通过：消费者实测 + 门禁全绿） |
| M3 | Provider 层：内置适配器 + register_provider | ✅（真实 API 验收通过） |
| M4 | 通用 RPC + KernelBuilder 定型；插件手册/参考模板迁移 | ✅ |
| M5 | 发布与切换（crates.io 上架除外） | ⏸ 推迟：切换留待 v3 评估 |

详见 [docs/plan.md](docs/plan.md)。

## 开发命令

```bash
cargo check
cargo test
cargo clippy -- -D warnings
cargo fmt --check
cargo run --example hello
```

## 术语

见 [CONTEXT.md](CONTEXT.md)。架构决策见 [docs/adr/](docs/adr/)。

## 许可证

AGPL-3.0（与 mistake-agent 同源）。
