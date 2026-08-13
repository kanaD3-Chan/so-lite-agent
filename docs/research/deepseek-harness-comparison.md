# DeepSeek Harness 与 SL Agent 对照

调查日期：2026-08-14

## 结论

两者确实在同一条架构谱系上：都不是把 Agent 写死成聊天函数，而是把模型 Provider、Agent loop、工具、会话、插件和能力边界拆成可替换部件。

但两者的中心不同：

- **DeepSeek Harness（DSH）** 是“事件驱动的插件运行时”。Cordis context、插件、waterfall 事件和可持久化 session event log 组成它的骨架，几乎所有产品部件都可以被替换或拦截。
- **SL Agent** 是“强信任 Rust 内核 + Rune 用户插件 + capability seam”的轻量运行时。它明确区分 Kernel plugin 与 User plugin，用 `requires` 限制用户插件能力，保留编译期装配和 Linus 模式信任边界。

因此，SL Agent 不是重复实现 DSH，而是已经采用了 DSH 最值得借鉴的方向，但选择了更窄、更可控的扩展面。当前最明显的产品级差距不是 Agent loop，而是**持久化事件模型、事件拦截/决策语义、运行时组合和工具并发策略**。

## 架构对照

| 维度 | DeepSeek Harness | SL Agent | 判断 |
|---|---|---|---|
| 基本组成 | Cordis context 上的插件、服务、事件和可逆 effect；官方架构文档明确称所有产品部件都是插件 | `KernelBuilder` 装配 Kernel；Rust Kernel plugin + Rust/Rune User plugin；loop/model/session 以 seam 抽象 | 概念相似，DSH 更彻底地插件化 |
| 扩展方式 | 注册/替换服务、注册工具、增加 system prompt section、监听 waterfall 事件、扩展 session event map | 两段式 `Info + register`；`requires` 过滤 ServiceHandles；登记 Tool/Command/Event；Rune 宿主函数白名单 | SL Agent 的入口更少、更容易控制；DSH 的拦截能力更强 |
| Agent loop | 以 `turn`/`step` 为单位；每步一次模型请求及其工具调用，输入从 inbox claim，流程由 typed waterfall 介入 | `DefaultAgentLoop` 执行流式模型、串行工具、护栏、压缩、中断；`AgentLoop` trait 可替换 | 主循环职责高度相似；DSH 的事件生命周期更完整 |
| 会话真相 | append-only、lossless-JSON 的 typed session event log；模型 history 从事件投影得到 | message tree + active path；`SessionStore` 保存消息和会话元数据；默认 `InMemorySessionStore` | 两者都保留分支/压缩思想，但 DSH 的事件日志更适合回放和恢复 |
| 持久化 | JSONL/Zstandard 与 SQLite；追加、序号连续、崩溃尾部修复和中断回合闭合 | 只有 `SessionStore` 抽象和内存实现；文件实现由使用方提供 | 这是当前最大结构性差距 |
| 工具 | ToolDefinition 同时描述 schema、canonical JSON 输出、执行、finalize、并发元数据、UI presenter；pre/post policy 和 around dispatch | Info 声明参数、CallerPolicy、超时；Dispatch 负责双墙、schema 校验、取消、审计；工具串行执行 | SL Agent 的安全基础已经有了；DSH 在 canonical result、政策链和并发调度上更深 |
| 模型边界 | `LlmAdapter.stream` + provider route；统一 typed stream delta 协议；`llm/stream` 可包裹/替换请求 | `ModelService` + ProviderRegistry；OpenAI/Anthropic 适配器；`ModelChunk` 归一化 | 方向一致；DSH 的 route/waterfall 组合更灵活 |
| 事件 | 事件既是观察面，也是 waterfall 拦截/决策面；工具、模型和 loop 都有生命周期事件 | `EventSink` 是 fire-and-forget 播报；`Event::Custom` 承载业务扩展；Interrupt 是另一条内部信号 | SL Agent 目前的 Event 不等价于 DSH 的可拦截事件 |
| 组合 | profile = bundle + profile/home/CLI patch，按行 id 替换或插入，支持 live reload | `KernelBuilder` 显式链式注册；配置驱动组合暂缓至 P3 评估 | DSH 更像运行时产品平台；SL Agent 更像稳定库/单二进制内核 |
| 分发 | Web 与 headless profile | `sl-agent` HTTP/WS + 内嵌 Web；crate 库路径保留 | 产品形态已经直接对齐 |

## DSH 的执行模型

DSH 的默认循环可以概括为：

1. 打开 turn，claim inbox 中的 next-turn/next-step 输入。
2. 经过 `agent/pre-step`，可以拒绝或改写输入。
3. 记录 user message，组装 system prompt、工具 schema 和当前 session history。
4. 发起一次流式 LLM 请求，记录原始 chunk，并落一条完整 assistant message。
5. 对工具调用执行 pre-policy、guard、around-dispatch、post-policy，再把 canonical result 放回下一 step。
6. 根据 inbox、停止事件和护栏继续 step，最后写入结构化 `turn/end`。

这和 SL Agent `run_turn_inner` 的循环非常接近：模型流式输出、累积工具调用、Dispatch 执行、失败计数、超时/取消、压缩和 `TurnOutcome`。真正不同的是 DSH 把每个中间阶段都暴露成可组合事件，而 SL Agent 主要把这些阶段收束在默认 loop 与 Dispatch 内部。

来源：

- [DSH architecture.md](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/architecture.md)
- [DSH agent.ts](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/core/agent-loop/src/agent.ts)
- [SL Agent loop engine](../../src/agent/loop/engine.rs)
- [SL Agent dispatch](../../src/agent/dispatch.rs)

## 与 SL Agent 最相似的部分

- **能力 seam**：DSH 用 `ctx.llm`、`ctx.tools`、`ctx.sessions`、`ctx.agentLoop` 等服务键；SL Agent 已经把 `ModelService`、`SessionStore`、`AgentLoop` 形式化成 Definition/Provider/Consumer 三角。
- **工具不是裸函数**：两者都把模型 schema、权限/策略和运行时执行分开处理。SL Agent 的 `CallerPolicy` 双墙与 DSH 的工具 policy pipeline 属于同一类安全设计。
- **流式 provider 边界**：两者都用 provider-neutral 的流事件承接文本、推理、工具调用和 usage，而不是把某一家 API 的响应类型泄漏进 loop。
- **上下文压缩不等于删除历史**：SL Agent 保留消息树原文，只改变 active path；DSH 通过 replacement/shadowing 在 append-only log 上投影摘要。
- **Web harness 形态**：`sl-agent` 的 HTTP/WS + 内嵌前端与 DSH 的 web/headless profile 在产品交付形态上接近。

## 不应直接照搬的部分

### 1. “一切皆插件”不等于“所有东西都可动态加载”

DSH 的插件化是运行时组合机制。SL Agent 的 ADR-0006 有意把 Kernel plugin 留在编译产物内，把动态能力限制在 Rune User plugin；这是完整性与越权威胁模型的明确选择，不是尚未实现的 DSH 插件系统。

如果 SL Agent 引入 DSH 风格扩展，优先扩展 Rune 可见的受限事件/工具能力，不要因此开放动态 Rust/cdylib 内核加载。

### 2. DSH 的 session event log 与 SL Agent 的 message tree 不是同一个模型

DSH 把 request header、raw assistant chunk、tool lifecycle、turn close 等运行时事实也纳入 session event vocabulary；SL Agent 的 EventSink 主要面向 GUI 播报，SessionStore 主要保存消息树。直接把 `Event` 当成持久化日志会混淆播报事件和会话事实。

更稳妥的演进是新增独立的持久化事实协议，例如 `SessionRecord`/`SessionEvent`，由 Kernel 在关键边界写入；不要改变现有 `EventSink` 的 fire-and-forget 语义。

### 3. DSH 的并发工具执行需要明确的副作用契约

DSH 根据工具的 concurrency metadata 允许安全工具重叠，并对 exclusive 工具建立 barrier。SL Agent 当前串行执行工具，默认更保守。若要引入并发，必须先为工具声明“可并发/独占”语义，并明确结果落盘顺序、取消时已启动调用的收尾和跳过调用的结果，否则会破坏消息树与审计一致性。

## 建议路线

按收益/风险排序：

1. **P2：把 Event 的“观察”与“决策”分开建模。** 保留 `EventSink`，新增少量 typed hook/waterfall（优先 `before_tool`、`after_tool`、`before_model_request`、`turn_stopping`），让内核插件可以拒绝、改写或要求重试；Rune 只暴露经过白名单筛选的安全事件。
2. **P2/P3：定义可持久化的 session fact log。** 先固定事件 vocabulary、序号、幂等和崩溃恢复语义，再实现 JSONL；SQLite 可以后置。它应与现有 message tree 投影关系清楚，而不是替换消息树。
3. **P3：增加 headless/profile 级配置组合。** 只有当 `sl-agent` 需要用户替换模型、工具包、存储实现时再引入 profile/patch；当前 `KernelBuilder` 显式装配仍更简单，也符合 ADR-0005。
4. **后置：工具并发。** 先补 ToolDefinition/工具元数据的 canonical result 与 policy seam，再评估并发；不要为了对齐 DSH 直接改变当前串行语义。
5. **保持不变：Linus 内核模式、`requires` 能力白名单、CallerPolicy 双墙、业务语义走 `Custom`。** 这些是 SL Agent 相对 DSH 的明确设计取舍和安全优势。

## 参考资料

DeepSeek Harness 官方仓库（调查时固定到 commit `47f943859bef60e4160492346772ded9b24f765a`）：

- [README](https://github.com/deepseek-ai/deepseek-harness/tree/47f943859bef60e4160492346772ded9b24f765a)
- [Architecture](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/architecture.md)
- [Session subsystem](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/session.md)
- [Tools subsystem](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/tools.md)
- [LLM streaming](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/subsystems/llm-streaming.md)
- [Capability seams](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/capability-seams.md)
- [Adding a tool](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/docs/cookbook/adding-a-tool.md)
- [JSONL persistence](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/session/session-persistence-jsonl/README.md)
- [SQLite persistence](https://github.com/deepseek-ai/deepseek-harness/blob/47f943859bef60e4160492346772ded9b24f765a/packages/session/session-persistence-sqlite/README.md)

SL Agent 本地实现：

- [ADR-0006](../adr/0006-pivot-harness-and-rune.md)
- [Context glossary](../../CONTEXT.md)
- [SessionStore](../../src/services/session.rs)
- [Registry](../../src/registry/core.rs)
- [Events](../../src/events.rs)
