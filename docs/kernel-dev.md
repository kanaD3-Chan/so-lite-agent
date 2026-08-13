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

## 2. 能力 seam（Service Definition / Provider / Consumer）

pivot（ADR-0006）后，可替换能力形式化为三角角色——**换 Provider 不换 Consumer**，
替换一个能力即可改变整个运行时行为（对标 DeepSeek Harness 的「一切皆插件 + 能力可替换」，
但装配仍是本 crate 的编译期显式链式注册，配置驱动组合推迟评估）：

| 角色 | 职责 | 本 crate 落点 |
|---|---|---|
| **Service Definition** | 能力接口（契约） | `SessionStore` / `ModelService` / `AgentLoop` trait、`ServiceId`（能力标识） |
| **Service Provider** | 能力实现（替换点） | `InMemorySessionStore` / `MockModelService`+适配器 / `DefaultAgentLoop`、`ServiceHandles` 容器 |
| **Consumer** | 能力消费方（通常面向模型/loop） | `Dispatch`（调用入口）、`AgentLoop::run_turn`（消费 model/session） |

实例对照：

- **model seam**：Definition = `ModelService`（流式协议归一化）；Provider = `MockModelService` /
  OpenAI 兼容 / Anthropic 适配器（经 `register_openai_compatible` 接线，包 `ModelHandle`）；
  Consumer = `AgentLoop`（唯一消费方）。
- **session seam**：Definition = `SessionStore`（持久化契约）；Provider = `InMemorySessionStore`
  （文件实现由使用方提供）；Consumer = `Kernel` / `AgentLoop`（消息树与压缩接入存储链）。
- **loop seam**：Definition = `AgentLoop` trait（`run_turn`）；Provider = `DefaultAgentLoop`
  （内置默认实现，经 `KernelBuilder::loop_engine` 可替换）；Consumer = `Kernel::send_user_message` /
  通用 RPC `send_user_message`。

代码侧以 rustdoc 标注（`/// Capability seam: ...`）；新增可替换能力时按同一三角落位，
不要在 Consumer 里 new 具体 Provider。

## 3. 模块地图

```text
src/
├── lib.rs                    crate 出口与模块声明
├── builder/
│   ├── mod.rs                KernelBuilder / Kernel 公共面（pub use 重导出）
│   ├── assembly.rs           KernelBuilder（默认服务自动补齐、插件注册 fail-fast）
│   └── kernel.rs             Kernel 直连 API + 通用 RPC 入口
├── contract.rs               Info/EntryPoint/CallerPolicy/LoadPolicy/ToolError/PluginError
├── context.rs                PluginContext/KernelContext/EntryRegistrar
├── registry/
│   ├── mod.rs                注册表公共面（pub use 重导出）
│   ├── plugin.rs             UserPlugin/KernelPlugin/PluginDescriptor/RegisteredEntry
│   └── core.rs               注册表：fail-fast 校验、懒注册、模型工具列表过滤
├── services/
│   ├── mod.rs                服务契约公共面（pub use 重导出）
│   ├── handles.rs            ServiceId + ServiceHandles（类型化容器）
│   └── session.rs            SessionStore 契约 + InMemorySessionStore + 活跃路径
├── events.rs                 Event 事件流（通用子集 + Custom 扩展口）
├── audit.rs                  AuditRecord/Auditor/AuditSink
├── message.rs                Message/MessageKind/MessageId/消息树辅助
├── logger.rs                 分级日志门面 + 敏感值脱敏
├── rpc.rs                    通用 RPC（RpcRequest/RpcFrame/Method/RpcExtension）
├── agent/
│   ├── dispatch.rs           统一工具/命令执行（策略、校验、超时、审计）
│   ├── loop/
│   │   ├── mod.rs            Agent loop 公共面（pub use 重导出）
│   │   ├── types.rs          TurnInput/TurnOutcome/StopReason/CompactionInfo/LoopError
│   │   └── engine.rs         AgentLoop（护栏、压缩、中断消费、session::switch）
│   └── session.rs            SessionKey/Goal/InterruptBus/Summarizer/SessionSwitch
└── model/
    ├── mod.rs                模型层公共面（pub use 重导出）
    ├── contract.rs           ModelService/ModelRequest/ModelChunk/ModelError
    ├── handle.rs             AbortSignal + ModelHandle（超时/abort/审计）
    ├── providers.rs          ProviderRegistry + register_provider
    ├── mock.rs               MockModelService（链路自检/测试）
    ├── openai.rs             OpenAI 兼容共享工具 + register_openai_compatible()
    ├── responses.rs          Responses API 流式适配器
    ├── completions.rs        Chat Completions 流式适配器
    └── anthropic.rs          Anthropic Messages API 流式适配器

feature 门控（默认关，二进制启用）：
├── rune/                    Rune 脚本用户插件（ADR-0006，feature `rune-plugins`）
│   ├── vm.rs                ScriptVm（一次编译、按调用新建 Vm、async 调用桥）
│   ├── host.rs              宿主函数安装骨架（动态闭包，Send 约束）
│   └── plugin.rs            插件桥：manifest 声明 + register 绑定 + requires 白名单
│                            （每插件一条专用执行线程：rune Value !Send × dispatch Send）
├── src/bin/sl-agent/        服务端（feature `server`）：main.rs（HTTP/静态/装配）
│                             + ws.rs（WS/RPC 桥 + 事件广播，单一有序路径）
└── web/                     内嵌前端（rust-embed，P1 纯 HTML/JS）
```

`mod.rs` 只负责公共面、装配和 `pub use` 重导出；职责实现放子模块（如
`model/` 一目录一职责，`openai.rs` 拆出两种传输协议）。子模块间共享的私有项经父模块
`pub(crate) use` 桥接。

## 4. 启动装配顺序

入口是 `KernelBuilder::build()`（`src/builder/assembly.rs`）。顺序不能随意交换，因为组件之间存在依赖：

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

## 5. 插件注册与能力边界

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
- `EntryRegistrar` 只允许登记 `info()` 声明过的短名（声明与实现一致）；
- Rune 脚本路径同一契约（`manifest.json` 声明 + 脚本 register 绑定 + 绑定后校验），
  见 [docs/plugin-dev.md](plugin-dev.md)「Rune 脚本路径」。
  桥的核心约束：rune 0.14 的 `Value`/`Function` 不实现 `Send`，而 dispatch 要求
  handler future `Send`——脚本执行放在每插件专用线程，线程间只传 JSON + 通道。

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

### 4.4 内核插件目录（Linus 模式，ADR-0036）

内核插件放在 `src/plugin/<name>/`（目录即插件，小写蛇形，`mod.rs` 为入口），
**build.rs 自动发现**：构建期扫描目录生成 `builtin_kernel_plugins()` 清单，
`sl-agent` 二进制装配时逐条注册（新增插件无需改任何聚合文件；目录根放空文件
`disabled` 可整目录跳过）。首个内置插件 `storage`（纯服务提供者：JSONL 会话
事实日志落盘，ADR-0007 第二步）。参考模板：`docs/plugin-dev/reference/kernel_plugin.rs`
（复制到 `src/plugin/<你的插件名>/` 即开工）。

> 规划：ADR-0008（P3）将内核插件从目录升级为**独立 crate**（`crates/plugin-*/`），
> 整个 workspace 编译成一个二进制；P3 前保持目录形态。

## 6. 服务句柄

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

## 7. Dispatch 调用链

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

## 8. Agent loop

loop 是**可替换能力**（Capability seam，见 §2）：`AgentLoop` trait 的 `run_turn` 是
LLM 唯一决策循环；`DefaultAgentLoop` 是内置默认实现（`KernelBuilder::loop_engine`
可注入替换）。`run_turn` 流程：

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

## 9. 会话事实日志（ADR-0007）

`SessionStore` 是通用持久化契约：会话真相 = **append-only 事件日志**（lossless JSON、
seq 连续、落盘后不可修改），消息历史由**遮蔽投影**派生（参考 DSH `SessionEventMap` /
`SurfaceOp`）。默认实现 `InMemorySessionStore`（内存事件日志）与
`JsonlSessionStore`（JSONL 落盘 + 崩溃尾部修复，`sl-agent` 默认启用）：

- 元数据：`create_session` / `get_session` / `set_goal` / `archive` / `list_sessions` / `set_last_activity`；
- 事件日志：`append_event`（追加，seq 自动分配，校验 JSON 可序列化 + surface 合法）/
  `read_events`（全量日志）/ `resolve_seq`（消息 id → 事件 seq）；
- 投影：`read_path`（活跃链）/ `read_path_from(end_seq)`（任意末端，分支）/
  `set_active_path` / `read_all`（全量日志 → 消息，人读 transcript）；
- 编辑/重新生成/压缩统一为「追加事件 + `SurfaceOp::Replace` 遮蔽旧事件 +
  `source_event_seqs` 记录被遮蔽 seq」；投影纯函数 `fold_surface` / `chain_from`
  / `project_messages` 从 `services` 导出。

活跃链从末端沿遮蔽链回溯；LLM 上下文只包含活跃链投影。会话切换的**决策**（新消息
先判断、回合末 continue/update_goal/start_new）不是通用语义，由使用方实现：crate 只
提供 `SessionSwitch` 钩子（`KernelBuilder::session_switch` 注入）与 `Summarizer`
（`KernelBuilder::summarizer` 注入）；`session::switch` 工具也由使用方注册
（参照 [docs/plugin-dev.md](plugin-dev.md)）。

## 10. RPC 与事件

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

## 11. 扩展路径

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

## 12. 验证清单

```bash
cargo fmt --check
cargo check
cargo test --all-targets --all-features   # 含 rune-plugins / server 门控代码
cargo clippy --all-targets --all-features -- -D warnings
cargo test --test live_api -- --ignored   # 真实 API（key 只从本地配置读取，输出不得打印密钥）
```

涉及模型协议、注册表、loop、会话或 RPC 的改动，不能只依赖 mock 单测；必须补真实链路或
现有 `live_api` 覆盖。

## 13. 设计红线

- 引擎 crate 保持业务无关（ADR-0004）；仓库为 Cargo workspace（ADR-0008，P3 落地），
  P3 前保持单 crate + `src/plugin/` 内核插件目录（build.rs 自动发现，ADR-0036）；
- 用户插件不直触资源（只能经 `requires` 句柄）；内核插件特权入口经 `KernelContext`；
- `UserOnly` 工具不进入模型工具列表，所有入口点经 Registry/Dispatch；
- 审计默认全覆盖，敏感值脱敏；
- `mod.rs` 保持薄，职责实现放子模块；
- 设计改变必须同步 CONTEXT.md 或新增 ADR。
