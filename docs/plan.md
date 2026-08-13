# 里程碑计划

> 决策背景：mistake-agent 本仓库不修改（ADR-0001），so-lite-agent 独立推进。
>
> 状态：**M2–M4 已落地**（2026-08-07 评审通过）；**pivot 取代 M5 冻结计划**
> （2026-08-13，ADR-0006）：定位从 crate lib 转向**业务无关的通用 Agent 可执行文件**
> （二进制 `sl-agent`，浏览器 Web 应用形态：HTTP/WS + 内嵌前端）；内核插件 = **Linus 模式**
> （仅维护者编译进官方二进制，无动态内核扩展）；用户插件 = **Rune 脚本**（eBPF 模型：
> 安全 VM + requires 函数白名单）；能力 seam 化，内核/用户插件创新不变。

## M2：新仓库骨架（已落地）

验收：`cargo add so-lite-agent` 后十行代码跑通 hello 回合（mock 模型）。

| 项 | 状态 |
|---|---|
| 仓库骨架（Cargo.toml / LICENSE / git） | ✅ |
| 通用模块：contract / registry / context / events / audit / message | ✅ |
| 服务契约：ServiceId / SessionStore / InMemorySessionStore / ServiceHandles | ✅ |
| model：ModelService 全套 trait + MockModelService + ProviderRegistry 骨架 | ✅ |
| agent：session / dispatch / loop（含压缩、中断、护栏） | ✅ |
| KernelBuilder（默认服务自动补齐、插件注册 fail-fast）+ Kernel 直连 API | ✅ |
| hello 回合示例 + 集成测试（默认 mock、工具调用、持久化） | ✅ |
| cargo test / clippy / fmt 全绿（18 测试） | ✅ |
| 文档：README / CONTEXT.md / AGENTS.md / kernel-dev / api / ADR 0001-0005 | ✅ |

## M3：Provider 层（已落地）

- ✅ OpenAI 兼容适配器（Responses API + Chat Completions，覆盖 DeepSeek/SiliconFlow/Ollama）；
- ✅ Anthropic Messages API 适配器（流式 text/tool_use）；
- ✅ `register_openai_compatible()` / `ProviderRegistry` 接真实 API；
- ✅ 验收通过（2026-08-07）：`tests/live_api.rs` 真实 DeepSeek Responses 回合——直接 complete 与 Kernel 回合均返回正常回复（usage 含 reasoning_tokens）。

## M4：通用 RPC 与插件手册（已落地）

- ✅ `RpcRequest`/`RpcFrame` + 通用 Method 子集（send_user_message / trigger_command / edit_message / switch_branch / abort / get_state / list_sessions / read_session / list_tools）+ `custom` 兜底 + `RpcExtension`；
- ✅ Kernel 补齐：abort / edit_message / switch_branch / get_state / 附件（中性 path+name）；
- ✅ 插件开发手册 + 参考模板（`docs/plugin-dev/reference/`，复制即开工）。

## M5：发布与切换（已被 pivot 取代）

- ~~决策（2026-08-07）：不落地。crates.io 上架暂不做；mistake-agent 消费切换推迟到 v3 再评估。~~
- **2026-08-13（ADR-0006）**：M5 被 pivot 取代，crates.io 上架与 mistake-agent
  切换后移至 P3+ 评估。`docs/m5-switchover.md` 保留为历史蓝图（标注已被 pivot 取代）。

## P1：pivot 骨架（当前）

能力 seam 化 + Rune 用户插件桥 + 服务端入口。验收：`sl-agent` 服务入口（HTTP/WS +
内嵌静态资源）在浏览器最小聊天页跑通 hello 回合；一个 Rune 脚本用户插件（两段式契约 +
requires 句柄）端到端注册并调用；AgentLoop trait 化后默认实现行为不变（全量测试保持绿）。

> **检查点（协作约定）**：P1 推进到「Rune 用户插件桥」（info 结构化声明 + register 绑定 +
> requires 句柄注入）时**暂停**，先与协作开发者对齐方案再继续——这是「保留内核/用户插件
> 创新不变」最敏感的一步，涉及脚本侧契约表达、句柄注入边界与暴露面取舍，不许一口气改完。
> 内核插件按 Linus 模式**维持现状不动**（ADR-0006），不在 P1 改造范围内。

| 项 | 状态 |
|---|---|
| ADR-0006 落地 + 文档同步（CONTEXT / plan / plugin-dev 草案） | ⏳ |
| 能力 seam 三角形式化：Service Definition / Provider / Consumer 角色标注与命名 | ⏳ |
| AgentLoop 抽象为 trait + 默认实现（loop 可替换第一步） | ⏳ |
| rune 依赖 + 宿主上下文（宿主函数安装、async 调用桥） | ⏳ |
| **Rune 用户插件桥：info() 结构化声明 + register() 绑定（脚本函数包装为 ToolHandler）+ requires 白名单注入** | ⏳（到此处暂停，先对齐方案） |
| `sl-agent` 服务入口：HTTP/WS + 事件流/RPC 桥 + 内嵌静态资源 + 浏览器最小聊天页（前端工程化后置，纯 HTML/JS 起步） | ⏳ |
| 门禁全绿 + hello 回合经浏览器 + Rune 插件通过 | ⏳ |

## P2：Rune 插件一等支持 + GUI 长全

- 脚本插件目录约定（一插件一 .rn / 一目录）与显式清单（enabled 语义沿用 ADR-0005）；
- 热重载（rune 热重载）：脚本变更后撤销/重挂注册（对应 DSH 的可逆副作用语义）；
- 事件 / 审计 / RPC 桥：脚本插件经宿主函数触发 `Event::Custom`、读审计、调通用 RPC；
- GUI 长全：流式输出、会话列表、工具调用面板（事件/RPC 走 WS）；前端工程化（Vue/TS）
  在此阶段或 P3 定夺；
- 插件手册 + 参考模板补 Rune 路径；examples 新增脚本插件示例。

## P3：分发形态评估（评估项）

- web 打磨：会话持久化落盘、模型/凭据配置界面、错误与日志的用户面；
- 配置驱动组合评估：重议 ADR-0005（profile/patch 类似物）——只有明确需求才做；
- mistake-agent 非特权业务迁移为 Rune 用户插件评估（其内核级业务留在自己二进制）；
- crates.io 上架与 mistake-agent 切换重新评估（原 M5 内容后移至此）。
