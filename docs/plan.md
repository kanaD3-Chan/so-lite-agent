# 里程碑计划

> 决策背景：mistake-agent 本仓库不修改（ADR-0001），so-lite-agent 独立推进。
>
> 状态：**M2–M4 已落地**（2026-08-07 评审通过）；**pivot 取代 M5 冻结计划**
> （2026-08-13，ADR-0006）：定位从 crate lib 转向**业务无关的通用 Agent 可执行文件**
> （二进制 `sl-agent`，HTTP/WS API 服务；**前后端分离**，官方参考前端 = `frontend/`
> React 工程，ADR-0010）；内核插件 = **Linus 模式**
> （仅维护者编译进官方二进制，无动态内核扩展）；用户插件 = **Rune 脚本**（eBPF 模型：
> 安全 VM + requires 函数白名单）；能力 seam 化，内核/用户插件创新不变。
>
> **P1 已落地**（2026-08-13）：seam 化 + AgentLoop trait + rune 宿主 + Rune 用户插件桥
> （检查点结论：a1 DynamicService / b1 事件+日志 / c2 目录+清单 / d1 模块级函数 / e1 默认关）
> + `sl-agent` 服务入口，浏览器 hello 回合与脚本插件端到端经真实 WS 客户端验收。

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
| ADR-0006 落地 + 文档同步（CONTEXT / plan / plugin-dev 草案） | ✅ |
| 能力 seam 三角形式化：Service Definition / Provider / Consumer 角色标注与命名 | ✅ |
| AgentLoop 抽象为 trait + 默认实现（loop 可替换第一步） | ✅ |
| rune 依赖 + 宿主上下文（宿主函数安装、async 调用桥） | ✅ |
| **Rune 用户插件桥：manifest 声明 + register() 绑定（脚本函数包装为 ToolHandler）+ requires 白名单注入** | ✅（检查点结论 a1/b1/c2/d1/e1，见上） |
| `sl-agent` 服务入口：HTTP/WS + 事件流/RPC 桥 + 内嵌静态资源 + 浏览器最小聊天页（前端工程化后置，纯 HTML/JS 起步） | ✅ |
| 门禁全绿 + hello 回合经浏览器 + Rune 插件通过 | ✅（真实 WS 客户端冒烟验收） |

> **P1 检查点记录（2026-08-13）**：Rune 用户插件桥对齐结论——
> a) 自定义服务经新增 `DynamicService` trait（a1，未实现则 requires fail-fast）；
> b) 脚本宿主暴露 emit_event/progress/log（b1，deadline/interrupt 推迟 P2）；
> c) 目录形态提前到 P1（c2）：一插件一目录 manifest.json + plugin.rn；
> d) 模块级宿主函数（d1，结构性白名单最简实现）；
> e) rune-plugins feature 默认关、二进制启用（e1）。
> 内核插件按 Linus 模式全程未动。

## P2：会话事实日志 + Rune 一等支持 + 事件决策分离

- **会话事实日志转向（ADR-0007，✅ 存储转向已落地）**：SessionStore → append-only
  事件日志 + 遮蔽投影（参考 DSH `SessionEventMap` / `SurfaceOp` / 持久化契约）；
  `InMemorySessionStore` 重写为「事件日志 + 投影缓存」，Kernel/RPC 外层语义保持
  （`edit_message` / `switch_branch` / `read_session` 行为不变，前端帧协议零改动）；
  **JSONL 落盘（✅ 已落地）**：`JsonlSessionStore`（每会话 `<key>.jsonl`，首行 meta +
  事件行，崩溃尾部修复 = 截断不完整行，原子写 meta，参考 mistake-agent storage 基建），
  `sl-agent` 默认启用（`SL_AGENT_DATA_DIR`，默认 `./data`）；
  **内核插件目录（✅ 已落地）**：`src/plugin/` + build.rs 自动发现（ADR-0036，参考
  mistake-agent），首个内核插件 `storage`（纯服务提供者，JSONL 会话存储）；
- **热重载（rune 热重载，✅ 已落地）**：`ScriptPluginLoader`（目录轮询 + 变更检测 +
  摘旧重挂 + 失败回滚）+ `Registry::remove_namespace`（可逆副作用摘除原语）+
  `ScriptPluginHandle::reload`（线程重编译）；脚本变更热生效、语法错误回滚保留旧版、
  目录删除卸载；manifest（requires 白名单）变更 = 重新加载；**执行超时（B2）**：
  单次脚本调用 30s 默认超时（`with_call_timeout` 可配），死循环不卡死执行线程；
- 事件 / 审计 / RPC 桥：脚本插件经宿主函数触发 `Event::Custom`、读审计、调通用 RPC；
- **事件决策分离（✅ 已落地，调研报告路线 1）**：保留 `EventSink` 播报（观察），
  新增 `LoopHook` 决策链（`before_tool` / `after_tool` / `before_model_request` /
  `turn_stopping`）——`before_tool` 可**改写参数或拒绝**（错误回喂模型），其余观察式；
  内核插件/使用方实现 trait，经 `KernelBuilder::loop_hook` 按序注入；
  Rune 脚本不直接实现（脚本无具体类型），观察需求经 `Event::Custom` 上浮；
- 不可信插件加固（后续项）：`session_read` 越权收窄（多会话场景）、emit_event/log
  洪泛配额——单机单用户场景可接受，多用户/多租户由下游在服务层控制；
- **GUI（✅ 已落地：前后端分离 + React 参考实现，ADR-0010）**：sl-agent 纯 API
  服务（/ws + /healthz，无静态服务）；`frontend/` React + TS 参考前端——聊天流式
  （message_delta/reasoning_delta）、会话列表（list_sessions/read_session）、工具面板
  （ToolStart/Progress/End 聚合）、断线重连；`VITE_SL_AGENT_WS` 注入后端地址；
  协议契约在 `frontend/src/protocol.ts`；fork/自建前端连同一 WS 协议、技术栈自选；
- 插件手册 Rune 路径与 examples 脚本插件示例已在 P1 落地。

## P3：分发形态评估（评估项）

- **workspace 化（ADR-0008，✅ 决策已留痕）**：仓库改为 Cargo workspace——内核插件
  独立 crate（`crates/plugin-*/`），整个 workspace 编译成一个二进制（`sl-agent`）；
  engine crate 保持业务无关；build.rs 自动发现改扫 workspace 级插件目录；P3 执行迁移；
- web 打磨：会话持久化落盘（已具备）、模型/凭据配置界面、错误与日志的用户面；
- 配置驱动组合评估：重议 ADR-0005（profile/patch 类似物）——只有明确需求才做；
- 会话事实日志词汇扩展评估：turn/step、raw chunk、tool 生命周期、compaction 锁、
  fork（由 mistake 迁移需求反推，ADR-0007 第三步）；
- mistake-agent 非特权业务迁移为 Rune 用户插件评估（其内核级业务留在自己二进制）；
- **crates.io 上架：不评估（ADR-0009，✅ 决策已留痕）**——定位已转为可执行文件
  项目，不发布 crates.io；mistake-agent 切换评估保留（原 M5 内容）。
