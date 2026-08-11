# Kernel 开发手册

> 本文档面向**想理解或修改 so-lite-agent 内核**的开发者（crate 维护者、深度集成者）。
> **只做 Agent 的开发者不需要读本文档**：`cargo add so-lite-agent` 后按
> [docs/plugin-dev.md](plugin-dev.md) 写内核/用户插件即可，内核设计细节对使用方不可见。
>
> 本文档由 mistake-agent 的 `docs/kernel-dev.md` 按通用语义改编（AGPL-3.0 同源），
> 描述的是本 crate 的通用运行时，不含任何业务（错题/记忆/验算等）语义。

## 1. 内核定位

so-lite-agent 是通用 Agent 运行时，不实现任何业务。它负责：

- **Agent loop**：组织模型请求、流式输出、工具调用、停止护栏和上下文压缩；
- **Dispatch**：统一执行入口点（CallerPolicy 双墙、schema 校验、超时/取消、审计）；
- **Session 通用语义**：SessionKey/Goal、消息树活跃路径、中断总线、摘要器与会话切换钩子；
- **Registry**：注册 KernelPlugin/UserPlugin，校验入口点和服务能力；
- **Model Provider 层**：`ModelService` 抽象 + OpenAI 兼容 / Anthropic 适配器 + ProviderRegistry；
- **通用 RPC**：Method 子集 + `custom` 兜底 + `RpcExtension`；
- **事件/审计/日志**：通用子集的事件流、审计记录与分级日志门面。

业务（错题、记忆、验算、settings、GUI 协议等）由使用方以插件、自定义服务和 `RpcExtension`
实现（ADR-0004 通用边界），不进入 crate。

## 2. 模块地图

```text
src/
├── lib.rs                    crate 出口与模块声明
├── builder.rs                KernelBuilder 装配 + Kernel 直连 API + 默认值
├── contract.rs               Info/EntryPoint/CallerPolicy/LoadPolicy/ToolError/PluginError
├── context.rs                PluginContext/KernelContext/EntryRegistrar
├── registry.rs               注册表 + UserPlugin/KernelPlugin 两段式契约
├── services.rs               ServiceId/ServiceHandles/SessionStore/InMemorySessionStore
├── events.rs                 Event 事件流（通用子集 + Custom 扩展口）
├── audit.rs                  AuditRecord/Auditor/AuditSink
├── message.rs                Message/MessageKind/MessageId/消息树辅助
├── logger.rs                 分级日志门面 + 敏感值脱敏
├── rpc.rs                    通用 RPC（RpcRequest/RpcFrame/Method/RpcExtension）
├── agent/
│   ├── dispatch.rs           统一工具/命令执行（策略、校验、超时、审计）
│   ├── loop.rs               Agent loop（护栏、压缩、中断消费、session::switch）
│   └── session.rs            SessionKey/Goal/InterruptBus/Summarizer/SessionSwitch
└── model/
    ├── mod.rs                ModelService/ModelChunk/ModelHandle/ProviderRegistry/Mock
    ├── openai.rs             OpenAI 兼容共享工具 + register_openai_compatible()
    ├── responses.rs          Responses API 流式适配器
    ├── completions.rs        Chat Completions 流式适配器
    └── anthropic.rs          Anthropic Messages API 流式适配器
```

`mod.rs` 只负责公共面、装配和 `pub use` 重导出；职责实现放子模块（如
`model/` 一目录一职责，`openai.rs` 拆出两种传输协议）。子模块间共享的私有项经父模块
`pub(crate) use` 桥接。

## 3. 启动装配顺序

入口是 `KernelBuilder::build()`（`src/builder.rs`）。顺序不能随意交换，因为组件之间存在依赖：

1. 补齐默认值：事件 sink = `MemoryEventSink`、审计 sink = `MemoryAuditSink`、
   system_prompt = 空串 provider、摘要器 = `StubSummarizer`、会话切换 = 无（ADR-0003）；
2. 补齐服务句柄：`ServiceHandles` 未显式提供会话/模型时，自动注入
   `InMemorySessionStore` 与 `MockModelService`（包 `ModelHandle`）；
3. 创建 `Registry`（持有句柄 + 日志门面）；
4. 先注册内核插件（`register_kernel_plugin`），再注册用户插件（`register_plugin`）——
   注册期 fail-fast：namespace/wire 撞名、`requires` 缺失、重复 `provides`、
   用户插件声明 `provides` 都当场报错；
5. 创建 `Dispatch`（注册表 + 审计 + 默认超时 + 宽限 + 回合预算 + 事件）；
6. 创建共享 `InterruptBus` 与 `AgentLoop`（注入模型、摘要器、会话切换钩子、压缩与护栏参数）；
7. 返回 `Kernel`（持有 RPC 扩展链）。

新增一个需要启动依赖的内核组件，应在 `KernelBuilder` 完成实例化和注入，再经插件/句柄暴露；
不要在 handler 第一次调用时偷偷创建全局单例。

## 4. 插件注册与能力边界

### 4.1 两段式契约

插件先通过 `info()` 声明元数据，再由 `register(ctx)` 绑定 handler；`PluginDescriptor` /
`KernelDescriptor` 把两者打包后显式交给注册表（**trait 是契约，描述符才是注册**）：

```rust
let desc = PluginDescriptor::from_plugin::<MyPlugin>();
// 或 KernelDescriptor::from_plugin::<MyKernelPlugin>()
KernelBuilder::new().register_plugin(desc);
```

- `Info.enabled` **缺省 false**：插件必须显式 `enabled: true` 才注册；未启用插件保留在
  聚合点/代码中，注册表静默跳过（ADR-0005）。
- `LoadPolicy::Lazy`（默认）首次命中入口点才执行 `register`；`Eager` 注册时立即绑定。
- `EntryRegistrar` 只允许登记 `info()` 声明过的短名（声明与实现一致）。

### 4.2 注册表校验（fail-fast）

- namespace 全局唯一；
- 内核插件 `provides` 的每个 `ServiceId` 至多一个提供者（`ServiceTaken`）；
- 用户插件不得声明 `provides`（`ProvisionNotAllowed`）；
- 用户插件 `requires` 中的服务必须可用（`CapabilityUnavailable`）；
- 插件内入口短名不重复（`DuplicateEntry`）；
- 内部全名 `namespace::tool` 与模型 wire name（`::` → `__`）均全局唯一（`WireNameCollision`）。

### 4.3 句柄过滤

- `PluginContext.handles`：只含 `requires` 声明过的服务（结构性受限，无运行时检查可绕）；
- `KernelContext.handles`：全量句柄（内核插件在信任边界内，是服务的提供者）。

## 5. 服务句柄

`ServiceId` 是字符串背书的 newtype：内置 `session()` / `model()`，业务服务用
`custom(name)`。`ServiceHandles` 为会话、模型保留类型化槽位（`with_session` /
`with_model`），其余自定义服务进 `HashMap<ServiceId, Arc<dyn Any + Send + Sync>>`，
插件侧经 `get_custom::<T>()` 按**具体类型** downcast 取回（ADR-0002 混合式设计）。

| 能力 | 使用方接线 | 插件取回 |
|---|---|---|
| 会话存储 | `ServiceHandles::with_session(Arc<dyn SessionStore>)` | 类型化槽位（内置） |
| 模型 | `ServiceHandles::with_model(ModelHandle)` | 类型化槽位（内置） |
| 业务服务 | `ServiceHandles::with_custom(id, Arc<T>)` | `get_custom::<T>(&id)` |

`ServiceHandles::filter(&requires)` 按能力声明裁剪句柄；`available()` 返回当前服务集合，
注册表用它校验 `requires`。

> 本 crate 不含文件 IO 语义（mistake-agent 的 `DomainIo`/`TmpIo`/`RelPath` 是应用侧设计）。
> 使用方需要落盘时，自行实现 `SessionStore` / `AuditSink` / 日志后端，或在自己的服务句柄里封装。

## 6. Dispatch 调用链

所有模型工具和用户命令都经过 `Dispatch`：

```text
Caller
  → Registry.ensure_tool / ensure_command（懒注册）
  → CallerPolicy 双墙校验（UserOnly 拒绝模型调用，记 AccessDenied）
  → JSON Schema 参数校验（jsonschema，失败记 InvalidParams）
  → 看门狗执行（超时 → 宽限 → 取消 → abort）
  → handler(ToolCallContext, params)
  → 结构化 ToolError / JSON 结果
  → AuditRecord::EntryPointCall
```

- `call_command` 找不到 Command 时回退放行同名 Tool（调用方 = User，用户必可调）；
- `ToolCallContext` 提供 `AbortSignal`（取消链）、`DeadlineHandle`（handler 可申请延期，
  受回合预算钳制）、`TurnControl`（请求内部中断）、`LoggerHandle` 与 `EventSink`（进度播报）；
- 同一轮多个工具调用**串行执行**（v2 设计，ADR-0010 对应语义在 crate 内保持）。

## 7. Agent loop

`AgentLoop::run_turn` 是 LLM 唯一决策循环：

1. 回合边界消费 `InterruptBus` 中断并记录审计；
2. 注入系统提示（`system_prompt` provider 每轮调用，不落消息树）；
3. 流式调用主模型（文本/推理/工具调用/usage 归一化为 `ModelChunk`）；
4. 串行执行模型产生的工具调用，结构化结果回填对话；
5. 模型自然停止或护栏中止后返回 `TurnOutcome`。

护栏（均可经 `KernelBuilder` 调整）：

- 单回合最多 25 次工具调用（`max_tool_calls`）；
- 相同错误连续 3 次停止（`max_consecutive_failures`）；
- 单回合总超时（`turn_budget`，默认 10 分钟）与用户取消；
- 模型瞬时错误重试一次；系统性错误（鉴权/配额/模型不存在）直接中止；
- 上下文用量 ≥ 窗口 75% 时在回合边界压缩：最近 15 条保留（`compaction_keep_last`），
  其余交给注入的 `Summarizer` 生成摘要，原文仍保留在存储；摘要失败下回合再试。

`forced_tool`（wire name）支持首轮强制调用工具（全程 `thinking=none`）；
`session::switch` 是 loop 特殊处理的入口：不走普通 handler，而是调用注入的
`SessionSwitch` 钩子，切换后的新会话键回填到 `TurnOutcome.session_key`。

## 8. 会话与消息树

`SessionStore` 是通用持久化契约（`InMemorySessionStore` 为默认实现，文件实现由使用方提供）：

- `create_session` / `get_session` / `set_goal` / `archive` / `list_sessions` / `set_last_activity`；
- `append_message` / `read_path` / `read_all` / `set_active_path`；
- `derive_branch`（编辑派生新分支，历史不截断）/ `switch_branch`（切活跃路径）；
- `splice_compaction`（摘要接入消息树：保留段首条挂到摘要下）。

活跃路径从末端沿 parent 链回溯（`active_chain`）；LLM 上下文只包含活跃路径。

会话切换的**决策**（新消息先判断、回合末 continue/update_goal/start_new）不是通用语义，
由使用方实现：crate 只提供 `SessionSwitch` 钩子（`KernelBuilder::session_switch` 注入）
与 `Summarizer`（`KernelBuilder::summarizer` 注入）；`session::switch` 工具也由使用方注册
（参照 [docs/plugin-dev.md](plugin-dev.md)）。

## 9. RPC 与事件

`src/rpc.rs` 提供通用 RPC：`RpcRequest`（id + `Method`）+ `RpcFrame`（Response/Event）。
通用 `Method` 子集：`send_user_message` / `trigger_command` / `edit_message` /
`switch_branch` / `abort` / `get_state` / `list_sessions` / `read_session` / `list_tools` /
`custom`。业务方法（settings/balance/cache/compute 等）经 `RpcExtension` 挂到 `custom`
兜底链：不认识的 method 返回 `not_handled`，kernel 继续问下一个扩展（ADR-0004）。

`Event` 只含通用语义：`message_delta` / `reasoning_delta` / `tool_start` / `tool_end` /
`tool_progress` / `turn_end` / `session_switched` / `compaction` / `error` + `Custom`
业务扩展口（与 `Method::Custom` 对称，kernel 不解析 name/payload，只负责运输）。

新增业务 RPC/事件优先走 `RpcExtension` / `Event::Custom`；不要往通用 `Method` / `Event`
里塞业务字段。

## 10. 扩展路径

### 新增自定义服务

1. 使用方在 crate 外定义 trait + 实现（如 `NoteService` / `MemoryNoteService`）；
2. `ServiceHandles::with_custom(ServiceId::custom("notes"), Arc::new(...))` 注入；
3. 内核插件在 `info().provides` 声明服务身份，register 里 `get_custom::<T>()` 取回并绑定入口。

### 新增内核/用户插件

按 [docs/plugin-dev.md](plugin-dev.md)：一插件一目录（`mod.rs` 契约 + `core.rs` 实现 +
聚合点显式注册），用户插件只写 `requires`，内核插件可 `provides`。

### 新增 RPC 业务方法

实现 `RpcExtension`（`handle(&self, method, params) -> Result<Value, RpcError>`），
`KernelBuilder::rpc_extension` 挂入；`custom` 兜底链按注册顺序询问。

### 注入真实模型/摘要器/会话切换

- 模型：`register_openai_compatible(&registry, name, config)` 或
  `AnthropicModelService::new(...)`，包 `ModelHandle` 后 `with_model`；
- 摘要器：`KernelBuilder::summarizer(Arc<dyn Summarizer>)`；
- 会话切换：`KernelBuilder::session_switch(Arc<dyn SessionSwitch>)`。

## 11. 验证清单

```bash
cargo fmt --check
cargo check
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo test --test live_api -- --ignored   # 真实 API（key 只从本地配置读取，输出不得打印密钥）
```

涉及模型协议、注册表、loop、会话或 RPC 的改动，不能只依赖 mock 单测；必须补真实链路或
现有 `live_api` 覆盖。

## 12. 设计红线

- 单 crate；通用运行时不实现业务，业务领域类型不进 crate；
- 用户插件不直触资源（只能经 `requires` 句柄）；内核插件特权入口经 `KernelContext`；
- `UserOnly` 工具不进入模型工具列表，所有入口点经 Registry/Dispatch；
- 审计默认全覆盖，敏感值脱敏；
- `mod.rs` 保持薄，职责实现放子模块；
- 设计改变必须同步 CONTEXT.md 或新增 ADR。
