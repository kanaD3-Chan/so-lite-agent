# so-lite-agent

开箱即用的通用 Agent 运行时：`cargo add so-lite-agent` 后即可上手开发自己的 Agent。

参考 [earendil-works/pi](https://github.com/earendil-works/pi) 的分层——模型 Provider 层（pi-ai 等价物）与 Agent core 层（pi-agent-core 等价物）内置随包，领域层（业务插件）由使用方编写。mistake-agent 是本仓库的参考实现与消费方（保持独立，不做 M1 解耦，见 [ADR-0001](docs/adr/0001-independent-repo-skip-m1.md)）。

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

默认装配：`InMemorySessionStore` + `MockModelService`（固定文本桩）+ `MemoryEventSink` + `MemoryAuditSink`，不配任何东西也能跑通 hello 回合。真实模型 Provider（OpenAI 兼容 / Anthropic 兼容 / 自定义端点）在 M3 提供。

## 开发上手

- 先跑 [examples/hello.rs](examples/hello.rs)：最小内核 + hello 回合；
- 再跑 [examples/plugins.rs](examples/plugins.rs)：自定义服务 + 内核插件 + 用户插件端到端（脚本化模型模拟两次工具调用）；
- 插件怎么下手：见 [docs/plugin-dev.md](docs/plugin-dev.md)。

## 模块一页

| 模块 | 职责 |
|---|---|
| `agent/loop` | Agent loop：模型流式消费、串行工具执行、护栏、压缩、中断消费 |
| `agent/dispatch` | 统一执行入口：CallerPolicy 双墙、懒注册、schema 校验、超时/取消、审计 |
| `agent/session` | SessionKey/Goal/SessionMeta、中断总线、摘要器、会话切换钩子 |
| `contract` | 入口点元数据、CallerPolicy、ToolError、PluginError |
| `registry` / `context` | 两段式插件契约（info + register）、懒注册、模型工具列表过滤 |
| `services` | ServiceId、SessionStore 契约 + InMemory 实现、ServiceHandles |
| `model` | ModelService 抽象、ModelChunk 归一化、ProviderRegistry、Mock |
| `events` / `audit` / `message` | 事件流、审计记录、消息树 |
| `builder` | KernelBuilder 装配入口 + Kernel 直连 API |

## 里程碑状态

| 阶段 | 内容 | 状态 |
|---|---|---|
| M2 | 新仓库骨架：通用模块 + 默认服务，hello 回合（mock） | 🟡 进行中（骨架完成，待评审） |
| M3 | Provider 层：内置适配器 + register_provider | 未开始 |
| M4 | 通用 RPC + KernelBuilder 定型；插件手册/参考模板迁移 | 未开始 |
| M5 | 发布 crates.io（0.x）；mistake-agent 切换消费 | 未开始 |

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
