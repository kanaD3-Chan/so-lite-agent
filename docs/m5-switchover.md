# M5 切换方案：mistake-agent 消费 so-lite-agent（不含 crates.io 上架）

> 目标：mistake-agent 的通用 Agent 运行时换成 so-lite-agent，删除本仓库重复代码，双端回归通过。
> 这是对 mistake-agent 的正式修改（此前 ADR-0001 冻结了它，M5 是计划中的解除时机）。

> **状态：已被 pivot 取代**（2026-08-13，ADR-0006）——pivot 将定位转为可执行
> harness + Rune 脚本扩展，M5 的 crates.io 上架与 mistake-agent 切换后移至 P3 评估；
> 本文保留为历史蓝图，实施前需按 pivot 后架构重新评估差距清单。

## 差距清单

so-lite-agent 已提供（mistake-agent 可删除的重复部分）：

- registry/dispatch/loop（含护栏、压缩、中断、懒加载）；
- contract/context/events/audit/message 通用子集；
- SessionStore trait + InMemory 默认；
- ModelService/ModelChunk/ModelHandle + OpenAI 兼容（Responses/Chat Completions）/ Anthropic 适配器 + ProviderRegistry；
- 通用 RPC：Method 子集 + custom 兜底 + RpcExtension；
- KernelBuilder / Kernel（send/list/read/call_command/abort/edit/switch/get_state）。

mistake-agent 仍是**应用侧**、留在原仓库的部分：

- Settings（settings.json、热更新、用户独占写）；
- FileStorage（会话 JSONL / 错题 JSON / 审计 JSONL 轮转）→ 实现 so-lite-agent 的 `SessionStore` + `AuditSink`；
- 错题领域（MistakeStore/Mistake/…）→ 自定义服务（`ServiceId::custom("mistake")`）；
- memory / compute 内核插件 → 内核插件 + 自定义服务；
- model 双模型路由（Main/Vision + LiveSettingsModelService）→ ProviderRegistry / ModelHandle；
- Session scheduler（LlmTurnDecider / LlmSummarizer / SessionSwitch）→ 使用方实现 `SessionSwitch`，工具 `session::switch` 由 mistake-agent 注册；
- 业务 RPC（settings/balance/cache/compute_result/test_connection）→ `RpcExtension`；
- 业务事件（MemoryChanged/CacheStatsUpdated/ComputeRequest）→ 使用方协议层扩展；
- force_tool / display_text、缓存命中统计等 mistake-agent 特有语义。

## 迁移步骤（建议顺序，每步保持可编译）

1. mistake-agent `Cargo.toml` 加 path 依赖：`so-lite-agent = { path = "../so-lite-agent" }`；
2. 内核核心层替换：`kernel/registry.rs`、`kernel/context.rs`、`kernel/contract.rs`、`kernel/events.rs`、`kernel/audit.rs`、`kernel/message.rs`、`kernel/agent/dispatch.rs`、`kernel/agent/loop_mod.rs` → 改为 re-export / 删除，引用方改到 crate；
3. 服务层适配：`services.rs` 的通用 trait 删除，错题领域保留并实现 crate 的 SessionStore/AuditSink；ServiceHandles 用 crate 的（内置 session/model + custom 槽位）；
4. 内核插件适配：storage/memory/compute/model/session 改为实现 crate 的 `KernelPlugin`；model 插件注册 ProviderRegistry + ModelHandle；
5. RPC 适配：Method 业务子集（settings/balance/cache/compute）挂 `RpcExtension`，通用子集走 crate 的 `handle_rpc`；事件桥接把 crate Event 映射到 GUI 协议（业务事件自定义扩展帧）；
6. 删除重复代码，跑 `cargo test`（80 单测）+ `cargo test --test live_api -- --ignored` + GUI 回归。

## 关键决策点（动手前需要定）

- **force_tool / display_text**：so-lite-agent 的 `TurnInput.forced_tool` 已支持 wire 名强制调用，但 Kernel 方法面没暴露；mistake-agent 需要在 crate 上加 `send_user_message_forced(…)` 还是自己在 crate 外组装 TurnInput？（建议：crate 补一个公开方法，语义通用）
- **Session scheduler 归谁**：crate 只给 `SessionSwitch` 钩子（ADR-0010）；mistake-agent 的实现留在应用侧（推荐，切换策略是业务语义）。
- **缓存命中统计**：crate 的 TurnOutcome 已累计 usage（含 cached/cache_miss），mistake-agent 自行聚合，不进 crate。
- **审计记录扩展**：crate 的 AuditRecord 是封闭枚举；mistake-agent 的 Memory/Compute/Settings 审计建议经自定义 `AuditSink` 包装（在 sink 里附加应用字段），或 M5 时评估给 crate 加 `AuditRecord::Custom` 变体。

## 验收

- mistake-agent：`cargo test` 80 单测全绿；`cargo test --test live_api -- --ignored` 全绿；GUI 五场景回归；
- so-lite-agent：本仓库测试全绿（当前 25 项 + live_api 忽略项）；
- 删除的重复代码量在提交 diff 中可见（预期 kernel 核心 ~4000 行 → re-export/删除）。
