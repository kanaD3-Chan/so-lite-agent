# so-lite-agent API 参考

> 面向**使用方（第三方开发者）**：`cargo add so-lite-agent` 后需要知道的装配、Kernel
> 直连 API、通用 RPC、事件流、模型 Provider 与审计。本文档只讲接口，不讲内核设计；
> 想理解内核再读 [docs/kernel-dev.md](kernel-dev.md)。
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
| `max_tool_calls(usize)` | 单回合工具调用上限 | 25 |
| `max_consecutive_failures(usize)` | 同码错误连续停止阈值 | 3 |
| `context_limit_tokens(usize)` | 上下文窗口（压缩阈值 = 75%） | 131072 |
| `compaction_keep_last(usize)` | 压缩保留的最近消息数 | 15 |
| `default_tool_timeout(Duration)` | 工具默认超时 | 30s |
| `turn_budget(Duration)` | 单回合总预算 | 10min |
| `loop_engine(Arc<dyn AgentLoop>)` | 注入可替换的 agent loop（Capability seam，ADR-0006）；缺省为内置默认实现 | 内置 `DefaultAgentLoop` |
| `rpc_extension(Arc<dyn RpcExtension>)` | 业务 RPC 方法扩展 | 无 |

## 3. Kernel 直连 API

| 方法 | 说明 |
|---|---|
| `send_user_message(key, text)` | 开新回合；会话不存在自动创建，新增消息落回 `SessionStore` |
| `send_user_message_with_attachments(key, text, attachments)` | 带附件（中性 path+name，数据由使用方填充） |
| `call_command(entry, params)` | 用户触发入口（trigger_command 等价）；找不到 Command 回退同名 Tool |
| `abort()` | 取消当前回合（无活动回合时静默） |
| `get_state()` | `{running: bool}` |
| `edit_message(key, message_id, text)` | 消息树编辑：只可编辑 assistant 消息，从编辑点派生新分支 |
| `switch_branch(key, message_id)` | 切换活跃路径，返回新路径消息 |
| `list_sessions()` / `read_session(key)` | 会话元数据 / 活跃路径消息 |
| `list_tools()` | 模型可见工具列表（wire name + JSON Schema） |
| `handle_rpc(RpcRequest)` | 通用 RPC 入口，返回带 id 的响应帧 |
| `registry()` / `dispatch()` / `auditor()` / `events()` / `interrupt_bus()` | 深度集成访问点（一般用不到） |

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
| `send_user_message` | `session_key?`, `text`, `attachments?` | 开新回合（None = 默认会话） |
| `trigger_command` | `entry`, `params?` | 用户触发入口 |
| `edit_message` | `session_key`, `message_id`, `text` | 派生新分支 |
| `switch_branch` | `session_key`, `message_id` | 切活跃路径 |
| `abort` | — | 停止当前回合 |
| `get_state` | — | 运行状态 |
| `list_sessions` | — | 会话列表 |
| `read_session` | `session_key` | 活跃路径消息 |
| `list_tools` | — | 模型工具列表 |
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

## 8. 会话与消息树

`SessionStore` 契约（异步 trait）：

- 元数据：`create_session` / `get_session` / `set_goal` / `archive` / `list_sessions` / `set_last_activity`；
- 消息：`append_message` / `read_path`（活跃路径）/ `read_all`（全量）/ `set_active_path`；
- 分支：`derive_branch`（编辑派生）/ `switch_branch` / `splice_compaction`（压缩接入）。

`Message` 为消息树节点（id/parent_id + kind）；`MessageKind`：`user`（含 `display_text`
与 `attachments`）/ `assistant` / `tool_call`（含 `call_id`，Responses 回传用）/
`reasoning` / `system`。`InMemorySessionStore` 重启即失，文件实现由使用方提供。

## 9. 插件契约

写插件（用户/内核）不读本文档的其余部分：见 [docs/plugin-dev.md](plugin-dev.md) 与
`examples/`（hello、plugins、folder_plugins）。参考模板在
`docs/plugin-dev/reference/`（复制即开工）。

## 10. 参考

- [examples/hello.rs](../examples/hello.rs)：最小内核 + hello 回合；
- [examples/plugins.rs](../examples/plugins.rs)：自定义服务 + 内核插件 + 用户插件端到端；
- [examples/folder_plugins](../examples/folder_plugins/main.rs)：一插件一目录编排；
- [tests/rpc_round.rs](../tests/rpc_round.rs)：RPC 子集往返测试。
