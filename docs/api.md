# so-lite-agent API 参考

> 面向 **fork 定制者**（ADR-0009 不发布 crates.io）：在本仓库源码内装配 Kernel、
> 写内核/用户插件时需要知道的直连 API、通用 RPC、事件流、模型 Provider 与审计。
> 本文档只讲接口，不讲内核设计；想理解内核再读 [docs/kernel-dev.md](kernel-dev.md)；
> 从零开发自己的 Agent 先读 [docs/agent-dev-guide.md](agent-dev-guide.md)。
> 由 mistake-agent 的 `docs/api.md` 按通用语义改编（AGPL-3.0 同源），业务方法一律不在此列。

## 1. 十行跑通（不需要懂内核）

```rust
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::events::MemoryEventSink;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events)
        .system_prompt(|| "你是我的 Agent。".to_string())
        .build()?;
    let outcome = kernel.send_user_message(Default::default(), "你好").await?;
    println!("{outcome:?}");
    Ok(())
}
```

默认装配：`InMemorySessionStore` + `MockModelService`（固定文本桩）+ `MemoryEventSink` +
`MemoryAuditSink` + 空系统提示 + 计数摘要器（ADR-0003）。不配任何东西也能跑 hello 回合；
显式传过的一律优先。

## 2. KernelBuilder 装配入口

| 方法 | 说明 | 默认 |
|---|---|---|
| `event_sink(Arc<dyn EventSink>)` | 事件流 sink | `MemoryEventSink` |
| `audit_sink(Arc<dyn AuditSink>)` | 审计 sink | `MemoryAuditSink` |
| `service_handles(ServiceHandles)` | 会话/模型/自定义服务 | 自动补 `InMemorySessionStore` + `MockModelService` |
| `register_kernel_plugin(KernelDescriptor)` | 内核插件（特权入口） | 无 |
| `register_plugin(PluginDescriptor)` | 用户插件（业务入口） | 无 |
| `system_prompt(fn() -> String)` | 人格注入，每轮调用 | 空字符串 |
| `summarizer(Arc<dyn Summarizer>)` | 压缩/交接摘要器 | `StubSummarizer` |
| `session_switch(Arc<dyn SessionSwitch>)` | 回合内 `session::switch` 钩子 | 无（工具返回"不可用"） |
| `session_decision(Arc<dyn SessionDecision>)` | 会话调度决策器（新消息前置决策 + 回合末决策；注入后 `send_user_message*` 委托决策器追加/分叉/切换，不再自行 append） | 无（默认 create + append） |
| `max_tool_calls(usize)` | 单回合工具调用上限 | 25 |
| `max_consecutive_failures(usize)` | 同码错误连续停止阈值 | 3 |
| `context_limit_tokens(usize)` | 上下文窗口（压缩阈值 = 75%） | 131072 |
| `compaction_keep_last(usize)` | 压缩保留的最近消息数 | 15 |
| `default_tool_timeout(Duration)` | 工具默认超时 | 30s |
| `turn_budget(Duration)` | 单回合总预算 | 10min |
| `loop_engine(Arc<dyn AgentLoop>)` | 注入可替换的 agent loop（Capability seam，ADR-0006）；缺省为内置默认实现 | 内置 `DefaultAgentLoop` |
| `loop_hook(Arc<dyn LoopHook>)` | 注入决策 hook（事件决策分离，P2）：before_tool 可改写/拒绝，其余观察式；按序链式执行（仅默认 loop） | 无 |
| `script_plugin(ScriptPlugin)` | 注册 Rune 脚本用户插件（feature `rune-plugins`；目录形态经 `ScriptPlugin::from_dir` 加载） | 无 |
| `rpc_extension(Arc<dyn RpcExtension>)` | 业务 RPC 方法扩展 | 无 |

## 2.5 Rune 脚本插件热重载（`rune::ScriptPluginLoader`，feature `rune-plugins`）

热重载原语（下游 fork 定制者直接继承，见 [plugin-dev.md](plugin-dev.md) §热重载）：

- `ScriptPluginLoader::new(dir, registry, services, events, logger)`：持有插件目录，
  轮询变更（manifest.json / plugin.rn）；
- `load_all()`：首次全量加载（懒登记，首次命中工具才绑定）；
- `poll()`：检测变更并热重载——脚本变更 = 摘旧条目 + 线程重编译 + 重新登记；
  语法错误回滚保留旧版；manifest（requires）变更 = 整体重新加载；目录删除 = 卸载；
- `run_loop(interval)`：后台轮询循环（`tokio::spawn`）；
- 配套：`Registry::remove_namespace(ns)`（摘除插件全部注册痕迹）、
  `ScriptPluginHandle::reload(script)`（线程重编译，失败保留旧 VM）；
- **执行超时**：单次脚本调用默认 30s（`ScriptPluginLoader::with_call_timeout` 可配），
  死循环不卡死执行线程（B2 不可信插件防护）。

## 3. Kernel 直连 API

| 方法 | 说明 |
|---|---|
| `send_user_message(key, text)` | 开新回合；会话不存在自动创建，新增消息落回 `SessionStore` |
| `send_user_message_with_attachments(key, text, attachments)` | 带附件（中性 path+name，数据由使用方填充） |
| `send_user_message_forced(key, text, display_text, attachments, forced_wire)` | 显式工具调用：开回合强制模型首轮调用指定工具（wire name），工具结果回填后由模型继续生成回复——不绕过 LLM（唯一决策者不变）；`text` = 模型指令文本、`display_text` = 落盘展示文本（两者分离） |
| `call_command(entry, params)` | 用户触发入口（trigger_command 等价）；找不到 Command 回退同名 Tool |
| `abort()` | 取消当前回合（无活动回合时静默） |
| `get_state()` | `{running: bool}` |
| `edit_message(key, message_id, text)` | 消息树编辑（user "改完重发"：只可编辑 user 消息，从编辑点派生新分支；**重新生成（改写 assistant）禁用**） |
| `switch_branch(key, message_id)` | 切换活跃路径，返回新路径消息 |
| `list_sessions()` | 会话元数据列表 |
| `read_session(key)` | 会话**全量消息树**（逻辑时间线：压缩摘要节点前插到压缩点，被遮蔽分支/历史完整保留——前端树视图 + `< / >` 分支导航数据源；模型可见活跃链另经 `read_path`） |
| `list_tools()` | 用户可见工具目录（GUI 工具面板数据源：UserAndModel **且** `user_visible=true`，wire name + JSON Schema + title/icon，每工具附所属插件的 `namespace_title`/`namespace_icon`（组标题/组图标，缺省回退 namespace）；`session::switch` 等仅模型工具不出现；模型可见工具全量列表另经 `model_tools()`） |
| `model_tools()` | 模型可见工具全量（wire name；含 `user_visible=false` 的仅模型工具，如 `session::switch`；不带 GUI 分组元数据） |
| `tool_meta()` | 全量工具展示元数据（RPC `list_tool_meta`：全部 UserAndModel 工具的 name/title/icon，**含 `user_visible=false` 的仅模型工具**——隐藏工具的事件气泡仍需 title/icon 渲染；不含 params/policy/description，展示与候选分离） |
| `handle_rpc(RpcRequest)` | 通用 RPC 入口，返回带 id 的响应帧 |
| `registry()` / `dispatch()` / `auditor()` / `events()` / `interrupt_bus()` / `registry_arc()` | 深度集成访问点（一般用不到；`registry_arc()` 返回与 kernel 同一注册表实例的 Arc，供 `ScriptPluginLoader` 等外部装配共享） |

## 4. 通用 RPC

### 4.1 帧格式

```json
{"id": 1, "method": "send_user_message", "text": "你好"}
{"type": "response", "id": 1, "result": {"stop_reason": "natural", "tool_calls": 0, "messages": []}}
{"type": "event", "event": {"event": "message_delta", "message_id": "...", "delta": "你"}}
```

`Method` 带 `#[serde(tag = "type")]`，参数平铺在请求帧顶层。`result` 与 `error` 二选一。

### 4.2 通用方法子集

| method | 参数 | 说明 |
|---|---|---|
| `send_user_message` | `session_key?`, `text`, `attachments?`, `force_tool?` | 开新回合（None = 默认会话）；`force_tool = {entry, hint?, display?}` 显式工具调用（entry 为内部全名 `namespace::tool`，UserOnly 工具拒绝） |
| `trigger_command` | `entry`, `params?` | 用户触发入口 |
| `edit_message` | `session_key`, `message_id`, `text` | 派生新分支（user "改完重发"） |
| `switch_branch` | `session_key`, `message_id` | 切活跃路径 |
| `abort` | — | 停止当前回合 |
| `get_state` | — | 运行状态 |
| `list_sessions` | — | 会话列表 |
| `read_session` | `session_key` | 全量消息树（逻辑时间线，含被遮蔽分支与压缩摘要节点） |
| `list_tools` | — | 用户可见工具目录（`user_visible=true`） |
| `custom` | `method`, `params?` | 业务方法兜底（走 `RpcExtension`） |

### 4.3 业务方法扩展

```rust
#[async_trait]
impl RpcExtension for MyExt {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "my.balance" => Ok(json!({"balance": 100})),
            _ => Err(RpcError::not_handled(method)),  // 继续问下一个扩展
        }
    }
}

KernelBuilder::new().rpc_extension(Arc::new(MyExt)).build()?;
```

错误码约定：`not_handled`（kernel 继续询问下一个扩展）、`tool_error`、`internal`；
使用方可自定义业务错误码。

## 5. 事件流

| event | 负载 | 说明 |
|---|---|---|
| `message_delta` | `message_id`, `delta` | 打字机增量 |
| `reasoning_delta` | `delta` | 思维链增量 |
| `tool_start` / `tool_end` | `entry`, `icon?` / `entry`, `ok` | 工具生命周期 |
| `tool_progress` | `entry`, `message`, `icon?` | 长任务进度 |
| `turn_end` | `stop_reason` | `natural` / `tool_call_limit` / `consecutive_failures` / `turn_timeout` / `user_aborted` / `failed` / `internal_abort` |
| `session_switched` | `from`, `to` | 会话切换（内部键） |
| `compaction` | `session` | 上下文压缩 |
| `error` | `message` | 错误播报 |
| `custom` | `name`, `payload?` | 业务事件扩展口（kernel 只运输不解析） |

实现 `EventSink::emit` 即可消费；业务事件一律走 `Event::Custom`，不进通用变体（ADR-0004）。

## 6. 模型 Provider

### 6.1 契约

- `ModelService`：`stream(&ModelRequest, &AbortSignal) -> Result<ModelStream>`，
  附默认 `complete()`（消费流聚合）。适配器只做协议转换，不做超时/审计（护栏在
  `ModelHandle` 与 loop）。
- `ModelRequest`：`model`（Main/Vision）、`messages`、`tools?`、`reasoning_effort?`、
  `response_format?`、`tool_choice?`（`auto` / `required` / `function{name}`）。
  `ToolSchema`（模型可见工具）= wire name + description + JSON Schema + **GUI 展示元数据**
  （`title` 用户友好显示名、`icon` Iconify 图标名，`list_tools`/`user_entries` 目录用）。
- `ModelChunk`：`TextDelta` / `ReasoningDelta` / `ReasoningItemStart` / `ToolCallStart` /
  `ToolCallDelta` / `ItemDone` / `Usage` / `Done` —— 流式事件归一化。
- `ModelHandle`：注入插件的受限句柄，只暴露带超时 + abort + 审计的 `complete`；
  凭据与适配器永远不离开 kernel。

### 6.2 内置适配器

```rust
let registry = ProviderRegistry::new();
let service = register_openai_compatible(&registry, "deepseek", OpenAiCompatibleConfig {
    api_url: "https://api.deepseek.com".into(),
    api_key: key.into(),
    model: "deepseek-v4-flash".into(),
    transport: OpenAiTransport::Responses,   // 或 ChatCompletions
    max_tokens: 4096,
    request_timeout: Duration::from_secs(300),
})?;
```

- `ResponsesModelService`：DeepSeek/OpenAI Responses API（SSE 语义事件，无状态全量历史）；
- `ChatCompletionsModelService`：OpenAI 兼容 Chat Completions（视觉模型 / Ollama 等）；
- `AnthropicModelService`：Anthropic Messages API（`with_version` 可调版本头）；
- `MockModelService`：固定文本桩 / `scripted(chunks)` 脚本化工具调用序列（测试与示例用）。

`ProviderRegistry` 不做全局可变状态，实例由使用方持有；重名注册 fail-fast。

## 7. 审计

`AuditRecord`（`#[serde(tag = "record")]`）覆盖：`entry_point_call`、`message_completed`、
`message_edited`、`branch_switched`、`session_switched`、`llm_call`、`lifecycle`、
`access_denied`、`turn_ended`、`interrupt`、`retry`、`compaction`。

实现 `AuditSink::append` 落盘（JSONL 文件、轮转、脱敏由使用方决定）；业务审计
（记忆/验算/settings 等）不在通用变体内，使用方可经自定义 sink 附加应用字段或
`Event::Custom` 上浮。

## 8. 会话事实日志（ADR-0007）

`SessionStore` 契约（异步 trait）：会话真相 = per-session **append-only 事件日志**
（lossless JSON、seq 连续、落盘后不可修改），消息历史由遮蔽投影派生。

- 元数据：`create_session` / `get_session` / `set_goal` / `archive` / `list_sessions` / `set_last_activity`；
- 事件日志：`append_event`（追加，seq 自动分配，校验 JSON 可序列化 + surface 合法）
  / `read_events`（全量日志，含被遮蔽事件）/ `resolve_seq`（消息 id → 事件 seq）；
- 投影：`read_path`（活跃链，默认末端 = `active_path` 或最新 surface 事件）/
  `read_path_from(end_seq)`（任意末端，switch_branch）/ `set_active_path` /
  `read_all`（全量日志 → 消息，人读 transcript）/
  `read_timeline`（**逻辑顺序全量时间线**：压缩摘要节点前插到其被压缩段之后，
  遮蔽不删除、历史不因压缩消失——前端完整渲染用；`timeline_messages` /
  `is_compaction_summary` 纯函数从 `services` 导出，缺省 = 日志顺序）。

事件 = `{ seq, message, surface_op?, source_event_seqs?, created_at }`；`message.kind`
即事件判别（User → user/message、Assistant → assistant/message、Reasoning →
assistant/reasoning、ToolCall → tool/result、System → compaction/summary）；
`SurfaceOp`：`Append` 或 `Replace { start, end }`（编辑（user "改完重发"）/压缩统一走 replace
遮蔽旧事件，`source_event_seqs` 记录被遮蔽 seq 全集；**重新生成（改写 assistant）禁用**）。投影纯函数
`fold_surface` / `chain_from` / `project_messages` 从 `services` 导出，供内核/测试复用。

`Message` 为投影视图节点（id/parent_id + kind）；`MessageKind`：`user`（含
`display_text` 与 `attachments`）/ `assistant` / `tool_call`（含 `call_id`，Responses
回传用）/ `reasoning` / `system`。

默认实现：

- `InMemorySessionStore`：全内存事件日志，重启即失；
- `JsonlSessionStore`（`services::JsonlSessionStore`）：JSONL 落盘，每会话
  `<key>.jsonl`（首行 meta + 事件行），崩溃尾部修复（截断不完整行）、原子写 meta、
  seq 连续校验（坏日志拒绝重建）；`sl-agent` 默认启用（`SL_AGENT_DATA_DIR`，
  默认 `./data`）。

## 9. 插件契约

写插件（用户/内核）不读本文档的其余部分：见 [docs/plugin-dev.md](plugin-dev.md) 与
`crates/engine/examples/`（hello、plugins、folder_plugins）。参考模板在
`docs/plugin-dev/reference/`（复制即开工）。

## 10. `sl-agent` 服务端（二进制 crate，ADR-0006/0010）

`sl-agent` 是业务无关的通用 Agent **API 服务**（ADR-0006/0010 前后端分离）：
只提供 `/ws` + `/healthz`，不内嵌页面。`cargo run -p sl-agent`
启动后 API 在 `http://127.0.0.1:8080`（`SL_AGENT_PORT` 改端口，`SL_AGENT_DATA_DIR`
改会话数据目录（默认 `./data`，JSONL 落盘），`SL_AGENT_API_URL/API_KEY/MODEL` 接
真实 OpenAI 兼容端点，缺省 mock 模型）。前端独立运行：`frontend/`（React 参考实现，
`npm run dev` → http://localhost:5173，`VITE_SL_AGENT_WS` 指后端 WS）。

WS 协议复用 §4 的帧格式：浏览器发 `RpcRequest` JSON 文本帧，服务端回
`RpcFrame::Response`（带 id 回执）；kernel 事件流经广播推 `RpcFrame::Event`
（`message_delta` / `reasoning_delta` / `tool_start` / `tool_end` / `turn_end` /
`tool_progress` / `custom`…）。事件先于回执（单一有序广播路径）。HTTP 侧：
`GET /healthz` 探活（根路径与其它路径不提供静态页，返回 404——前后端分离
ADR-0010，静态资源由前端自己托管）。

## 10. 参考

- [crates/engine/examples/hello.rs](../crates/engine/examples/hello.rs)：最小内核 + hello 回合；
- [crates/engine/examples/plugins.rs](../crates/engine/examples/plugins.rs)：自定义服务 + 内核插件 + 用户插件端到端；
- [crates/engine/examples/folder_plugins](../crates/engine/examples/folder_plugins/main.rs)：一插件一目录编排；
- [crates/engine/tests/rpc_round.rs](../crates/engine/tests/rpc_round.rs)：RPC 子集往返测试。
