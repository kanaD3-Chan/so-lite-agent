# so-lite-agent Context

通用 Agent 运行时的领域词汇：模型 Provider 抽象、Agent loop、插件契约、会话与消息树。

## 命名

**SL Agent（官方简写）**:
so-lite-agent 的官方简写；另一种写法是 **So Lite Agent**。文档、标题与口头交流优先用 SL Agent；代码与包名仍是 `so-lite-agent` / `so_lite_agent`，二进制名为 `sl-agent`（pivot 后以**浏览器 Web 应用**形态交付的业务无关通用 Agent 可执行文件：HTTP/WS 服务 + 内嵌前端，ADR-0006），不因简写改动。
_Avoid_: SLA、LiteAgent、SL-Agent（连字符变体）

## 运行时结构

**Kernel（内核）**:
`KernelBuilder::build()` 组装完成的通用 Agent 运行时实例：agent loop、工具调度、会话存储、模型句柄、中断总线与 RPC 入口。使用方只与 Kernel / KernelBuilder 打交道，不必理解内部设计。
_Avoid_: 引擎（含义过宽）

**KernelBuilder（装配入口）**:
crate 的唯一装配入口：默认补齐 `InMemorySessionStore` + `MockModelService` + `MemoryEventSink` + `MemoryAuditSink` + 空系统提示 + `StubSummarizer`（ADR-0003），显式传过的一律优先；插件注册 fail-fast。
_Avoid_: 手动拼装 Kernel（字段私有，构造唯一入口）

**Agent loop（Agent 循环）**:
kernel 的一次执行单元驱动器：LLM 是唯一决策者，kernel 执行工具调用并保证安全边界；停止条件包括模型自然停止、工具调用上限、连续失败、回合超时与用户取消。
_Avoid_: 调度器（指会话切换决策）、引擎

**Event（事件流）**:
kernel 向使用方 GUI 播报的通用事件（消息/推理增量、工具起止与进度、回合结束、会话切换、压缩、错误）；业务事件经 `Event::Custom` 扩展（ADR-0004）。
_Avoid_: 业务事件（记忆变更、验算请求等走 Custom）

**Audit（审计）**:
默认全覆盖的操作记录（EntryPointCall / LlmCall / TurnEnded 等），经 `AuditSink` 落盘；通用变体只含运行时语义，业务审计由使用方 sink 附加。
_Avoid_: 日志（诊断日志与审计分离）

**Turn（回合）**:
kernel 一次完整的 agent 执行单元：从输入触发开始，到模型自然停止或护栏中止结束，期间可多次调用工具。
_Avoid_: 会话（Session 是整个使用生命周期）

**EntryPoint（入口点）**:
插件向 kernel 登记的调用入口，共三类：Tool（模型/用户调度）、Command（用户/GUI 调度）、Event（kernel 生命周期调度）。
_Avoid_: 回调（只指其中一类）、接口

**CallerPolicy（调用方策略）**:
EntryPoint 的调用方边界：UserAndModel（模型可调，用户必可调）或 UserOnly（仅用户可调，模型工具列表不可见且调度拒绝）。
_Avoid_: 权限（含义过泛）

**Wire name（模型可见名）**:
内部规范名 namespace::tool 经 :: → __（双下划线）映射后发给模型的工具名；内部名、审计名与命令通道不变。
_Avoid_: 全名（指内部 namespace::tool）

**Interrupt（内部中断）**:
内核组件向 agent loop 发出的环境变更信号（会话切换、Goal 更新、配置变更、压缩完成），回合边界消费，不抢占当前回合。
_Avoid_: 事件（Event 指面向使用方 GUI 的播报）

**ConfigChanged（配置变更中断）**:
通用配置变化（原 mistake-agent 的 SettingsChanged 通用化）引发的中断，通知 loop 下回合按新环境重组上下文。
_Avoid_: SettingsChanged（业务命名）

## 插件与服务

**Kernel plugin（内核插件）**:
运行在内核信任边界内的特权子系统，负责敏感资源与能力（如会话存储、模型 Provider、业务服务）；经两段式契约注册，注册上下文为全量服务句柄。职责是**收紧权限与能力供给**（provides 服务、特权入口、护栏/压缩等运行时能力）。仅以 Rust 编写，且只由维护者编译进官方二进制（**Linus 模式**，ADR-0006）：不存在动态内核扩展机制（无 dll / 无签名脚本 / 无加载面），第三方需要新内核能力 = 交 PR 或 fork；crate 库形态下使用方自写内核插件编进自己的二进制（受信集成路径）。防篡改由「没有可加载物」保证。
_Avoid_: 系统服务、内核级插件（口语）

**User plugin（用户插件）**:
通过内核注册工具、命令与事件回调提供业务能力的插件，职责是**扩展 Agent 业务功能**；回调由 kernel 主动调用，只经服务句柄访问资源。两种编写路径共享同一契约：Rust（编译期，不变）与 Rune 脚本（运行时加载，pivot 后的一等路径，ADR-0006）。
_Avoid_: 业务插件（过早限定业务范围）

**Rune user plugin（Rune 脚本插件）**:
用户插件的脚本编写路径（eBPF 模型：安全 VM + 宿主函数白名单）：同一两段式契约（info 结构化声明 + register 经宿主函数绑定 handler），以 Rune 脚本随可执行文件分发、运行时加载；**requires 决定宿主装哪些函数**——脚本结构性拿不到未声明能力（防越权），明文可改也不怕（篡改只能在白名单内作恶，防篡改不是用户层目标）；enabled 缺省 false、wire 名全局唯一等校验与 Rust 路径一致（ADR-0006）。P1 形态（检查点结论 c2）：一插件一目录——`manifest.json`（Info 声明，纯数据不执行）+ `plugin.rn`（register + handlers，目录名 = namespace）。
_Avoid_: 脚本工具（只指入口点）、动态插件（含义过宽）

**ScriptPlugin（脚本插件源描述）**:
目录形态 Rune 脚本插件的源：`manifest.json` 反序列化出的 `Info` + `plugin.rn` 源码；经 `ScriptPlugin::from_dir` 加载（目录名必须等于 manifest.namespace），交 `KernelBuilder::script_plugin` 注册。
_Avoid_: 脚本文件（单 .rn 形态是 P2 评估项，P1 只有目录形态）

**DynamicService（动态服务接口）**:
自定义服务被 **Rune 脚本**访问的通道（ADR-0006 检查点 a1）：`async fn call(&self, method, params) -> Result<Value, ToolError>`；脚本没有具体类型（无法 downcast），只能按 method + JSON 调用。实现并经 `ServiceHandles::with_dynamic` 注入的服务才可被脚本 requires；未实现则注册 fail-fast。Rust 插件路径不受影响（仍 downcast，ADR-0002）。
_Avoid_: 脚本服务（它是接口不是服务实例）

**Service（服务）**:
内核插件向 kernel 提供的受控能力，在 info 中以 `provides` 声明；用户插件只能通过服务句柄访问。
_Avoid_: API（含义过泛）

**ServiceId（服务标识）**:
服务的唯一标识：内置 session/model 两个，业务服务用 custom 字符串标识；注册表按 provides 全局唯一。
_Avoid_: 服务名（无唯一性语义）

**Service handle（服务句柄）**:
kernel 按能力声明注入插件的受限接口；会话/模型走类型化句柄，自定义服务走类型擦除包 + downcast。
_Avoid_: 全局单例、直接依赖

**Capability seam（能力 seam）**:
可替换能力的三角角色结构：Service Definition（声明接口）、Service Provider（实现）、Consumer（消费方，通常是面向模型的工具）；换 provider 不换 consumer（如模型、会话存储、loop）。pivot 后内核能力逐步 seam 化（ADR-0006），替换一个 provider 即可改变整个运行时行为。现有实例：**model seam**（`ModelService` Definition → 适配器/Mock Provider → loop Consumer）、**session seam**（`SessionStore` Definition → `InMemorySessionStore` Provider → Kernel/loop Consumer）、**loop seam**（`AgentLoop` trait Definition → `DefaultAgentLoop` Provider → Kernel 直连/RPC Consumer，经 `KernelBuilder::loop_engine` 替换）。
_Avoid_: 服务（单角色）、插件（只是角色之一）

**Plugin directory（插件目录）**:
插件的推荐组织方式：一插件一目录，mod.rs 承载两段式契约（info + register + descriptor），子模块放 handler 实现；禁用插件 = `enabled` 缺省 false，注册表跳过（聚合点可保留注册行）。
_Avoid_: 插件文件夹（指实现细节）、disabled 标记文件（mistake-agent 编译期语义，本 crate 不用）

**enabled（启用标记）**:
`Info` 上的布尔字段，**缺省 false**：插件必须显式 `enabled: true` 才会注册；未启用的插件保留在代码/聚合点中，注册表静默跳过。
_Avoid_: disabled 标记（反向命名，mistake-agent 遗留语义）

**ProviderRegistry（Provider 注册表）**:
具名模型 Provider 的登记与查询入口；HTTP 适配器（OpenAI 兼容 / Anthropic）经 `register_openai_compatible` / `AnthropicModelService` 接入（M3 完成）；不做全局可变状态。
_Avoid_: 模型管理器（含义过宽）

**RpcExtension（业务方法扩展）**:
使用方把业务方法挂到通用 RPC `custom` 兜底链的 trait；返回 `not_handled` 时 kernel 继续询问下一个扩展（ADR-0004）。
_Avoid_: 往通用 Method 枚举塞业务字段

## 会话与消息

**Session（会话）**:
一次对话流的过程记录，由使用方持久化；任务层可经会话切换工具换上下文，对用户隐藏。
_Avoid_: 对话（用户视角的聊天）

**SessionKey（会话键）**:
标识一个会话的内部路由键，不暴露给用户。
_Avoid_: 会话 ID（暗示用户可见）

**Goal（会话目标）**:
当前会话要完成的目标，作为 continue / update_goal / start_new 的决策依据；切换决策由使用方实现。
_Avoid_: 任务名（过窄）

**Session event log（会话事实日志）**:
会话的**不可变真相**（ADR-0007）：per-session append-only 事件序列（lossless JSON、seq 连续、落盘后不修改）；事件类型 = user/assistant message、reasoning、tool/result、edit、compaction/summary；编辑/重新生成/压缩统一为「追加 + replace 遮蔽 + 投影」，可回放/恢复/审计。参考 DSH `SessionEventMap` / `SurfaceOp`。与 `EventSink`（GUI 播报）严格分离。crate 提供 `InMemorySessionStore`（默认）与 `JsonlSessionStore`（JSONL 落盘 + 崩溃尾部修复，`sl-agent` 默认启用，ADR-0007 第二步）。
_Avoid_: 会话日志（与诊断日志混淆）

**Workspace（工作区）**:
仓库编译组织形态（ADR-0008）：从单 crate 改为 Cargo workspace——内核插件以**独立 crate** 编写（`crates/plugin-*/`），整个 workspace 最终**编译成一个二进制**（`sl-agent`）。引擎 crate 保持业务无关（ADR-0004），插件 crate 声明自己的依赖；crate 边界即信任边界（Linus 模式：只有维护者能加插件 crate）。P3 落地，P2 保持 `src/plugin/` 目录形态。
_Avoid_: 多二进制分发（仍是单二进制）

**Message tree（消息树）**:
会话内消息的**投影视图**（ADR-0007 转向后）：由会话事实日志经遮蔽投影派生——模型 history 是活跃投影链，人读 transcript 是全量日志；每条消息有 id 与 parentId，编辑或重新生成 = 追加新事件 + replace 遮蔽旧消息，底层日志不可变、历史永不丢。
_Avoid_: 会话结构（真相是事件日志，消息树只是视图）

**Active path（活跃路径）**:
会话事实日志投影出的当前会话视图末端（ADR-0007）：沿「谁遮蔽了谁」的遮蔽链回溯构成模型可见消息序列；多条遮蔽链 = 分支（switch_branch 换末端）。
_Avoid_: 当前分支（口语）

**Compaction（上下文压缩）**:
上下文用量达阈值时，对活跃路径旧消息生成摘要并替换为摘要条目（原文保留在存储）的机制。
_Avoid_: 截断（原文被删）

**Summarizer（摘要器）**:
生成压缩/交接摘要的组件；crate 默认计数桩（`StubSummarizer`），真实 LLM 摘要经 `KernelBuilder::summarizer` 注入。
_Avoid_: 总结模型（指具体实现）

**SessionSwitch（会话切换钩子）**:
回合内 `session::switch` 由 loop 调用的钩子；默认调度器与 `session::switch` 工具注册由使用方实现，经 `KernelBuilder::session_switch` 注入。
_Avoid_: Session scheduler（mistake-agent 内部模块名）
