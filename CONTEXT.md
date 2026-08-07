# so-lite-agent Context

通用 Agent 运行时的领域词汇：模型 Provider 抽象、Agent loop、插件契约、会话与消息树。

## 命名

**SL Agent（官方简写）**:
so-lite-agent 的官方简写；另一种写法是 **So Lite Agent**。文档、标题与口头交流优先用 SL Agent；代码与包名仍是 `so-lite-agent` / `so_lite_agent`，不因简写改动。
_Avoid_: SLA、LiteAgent、SL-Agent（连字符变体）

## 运行时结构

**Agent loop（Agent 循环）**:
kernel 的一次执行单元驱动器：LLM 是唯一决策者，kernel 执行工具调用并保证安全边界；停止条件包括模型自然停止、工具调用上限、连续失败、回合超时与用户取消。
_Avoid_: 调度器（指会话切换决策）、引擎

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
运行在内核信任边界内的特权子系统，负责敏感资源与能力（如会话存储、模型 Provider、业务服务）；经两段式契约注册，注册上下文为全量服务句柄。
_Avoid_: 系统服务、内核级插件（口语）

**User plugin（用户插件）**:
通过内核注册工具、命令与事件回调提供业务能力的插件；回调由 kernel 主动调用，只经服务句柄访问资源。
_Avoid_: 业务插件（过早限定业务范围）

**Service（服务）**:
内核插件向 kernel 提供的受控能力，在 info 中以 `provides` 声明；用户插件只能通过服务句柄访问。
_Avoid_: API（含义过泛）

**ServiceId（服务标识）**:
服务的唯一标识：内置 session/model 两个，业务服务用 custom 字符串标识；注册表按 provides 全局唯一。
_Avoid_: 服务名（无唯一性语义）

**Service handle（服务句柄）**:
kernel 按能力声明注入插件的受限接口；会话/模型走类型化句柄，自定义服务走类型擦除包 + downcast。
_Avoid_: 全局单例、直接依赖

**Plugin directory（插件目录）**:
插件的推荐组织方式：一插件一目录，mod.rs 承载两段式契约（info + register + descriptor），子模块放 handler 实现；禁用插件 = 从聚合点移除注册行（显式装配，无 disabled 标记语义）。
_Avoid_: 插件文件夹（指实现细节）、disabled 标记（mistake-agent 编译期语义，本 crate 不用）

**ProviderRegistry（Provider 注册表）**:
具名模型 Provider 的登记与查询入口（M2 骨架，M3 接 HTTP 适配器）；不做全局可变状态。
_Avoid_: 模型管理器（含义过宽）

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

**Message tree（消息树）**:
会话内消息的组织结构：每条消息有 id 与 parentId，追加式存储；编辑或重新生成时派生新分支，历史不截断。
_Avoid_: 对话树（口语）、版本历史

**Active path（活跃路径）**:
消息树中从根到当前节点的唯一路径；LLM 上下文只包含活跃路径上的消息。
_Avoid_: 当前分支（口语）

**Compaction（上下文压缩）**:
上下文用量达阈值时，对活跃路径旧消息生成摘要并替换为摘要条目（原文保留在存储）的机制。
_Avoid_: 截断（原文被删）

**Summarizer（摘要器）**:
生成压缩/交接摘要的组件；crate 默认计数桩，真实 LLM 摘要由使用方注入。
_Avoid_: 总结模型（指具体实现）
