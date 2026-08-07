//! OpenAI 兼容适配器（M3）：共享工具 + 端点配置 + 注册入口。
//! 流式实现见 [`responses`](crate::model::responses)（Responses API）与
//! [`completions`](crate::model::completions)（Chat Completions）。

use std::error::Error;
use std::time::Duration;

use serde_json::{Value, json};

use crate::contract::full_to_wire;
use crate::message::{Message, MessageKind};
use crate::model::{ModelError, ModelService, ProviderRegistry, ResponseFormat, ToolSchema};

use super::completions::ChatCompletionsModelService;
use super::responses::ResponsesModelService;

/// 传输协议：Responses API（DeepSeek 主模型）或 Chat Completions（视觉/通用兼容端）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenAiTransport {
    Responses,
    ChatCompletions,
}

/// OpenAI 兼容端点配置。
#[derive(Debug, Clone)]
pub struct OpenAiCompatibleConfig {
    pub api_url: String,
    pub api_key: String,
    pub model: String,
    pub transport: OpenAiTransport,
    pub max_tokens: u32,
    pub request_timeout: Duration,
}

impl Default for OpenAiCompatibleConfig {
    fn default() -> Self {
        Self {
            api_url: "https://api.deepseek.com".into(),
            api_key: String::new(),
            model: "deepseek-v4-flash".into(),
            transport: OpenAiTransport::Responses,
            max_tokens: 4096,
            request_timeout: Duration::from_secs(300),
        }
    }
}

/// 按配置构造适配器并注册到 [`ProviderRegistry`]；返回可立即使用的服务。
pub fn register_openai_compatible(
    registry: &ProviderRegistry,
    name: &str,
    config: OpenAiCompatibleConfig,
) -> Result<std::sync::Arc<dyn ModelService>, String> {
    let service: std::sync::Arc<dyn ModelService> = match config.transport {
        OpenAiTransport::Responses => std::sync::Arc::new(ResponsesModelService::new(
            config.api_url,
            config.api_key,
            config.model,
        )),
        OpenAiTransport::ChatCompletions => std::sync::Arc::new(ChatCompletionsModelService::new(
            config.api_url,
            config.api_key,
            config.model,
            config.max_tokens,
        )),
    };
    registry.register(name, service.clone())?;
    Ok(service)
}

// ---------- 消息转换 ----------

pub(crate) fn responses_endpoint(api_url: &str) -> String {
    let base = api_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}/responses")
}

pub(crate) fn text_format(fmt: &ResponseFormat) -> Value {
    match fmt {
        ResponseFormat::JsonObject => json!({"type": "json_object"}),
        ResponseFormat::JsonSchema { name, schema } => json!({
            "type": "json_schema",
            "name": name,
            "schema": schema,
        }),
    }
}

pub(crate) fn parse_delta(data: &str) -> String {
    serde_json::from_str::<Value>(data)
        .ok()
        .and_then(|v| v["delta"].as_str().map(|s| s.to_string()))
        .unwrap_or_default()
}

pub(crate) fn tool_to_function(t: &ToolSchema) -> Value {
    json!({
        "type": "function",
        "name": t.name,
        "description": t.description,
        "parameters": t.input_schema,
    })
}

/// 内部 Message 树 → Responses API input items。
/// ToolCall 一条消息展开为 function_call + function_call_output。
pub(crate) fn messages_to_responses_input(messages: &[Message]) -> Result<Vec<Value>, ModelError> {
    messages_to_responses_input_impl(messages, true)
}

/// 兜底（reasoning_text 回传校验失败时）：剥离全部 reasoning item，
/// 请求方同时把 thinking 关掉（reasoning.effort=none）。
pub(crate) fn messages_to_responses_input_no_reasoning(
    messages: &[Message],
) -> Result<Vec<Value>, ModelError> {
    messages_to_responses_input_impl(messages, false)
}

fn messages_to_responses_input_impl(
    messages: &[Message],
    include_reasoning: bool,
) -> Result<Vec<Value>, ModelError> {
    let mut items = Vec::new();
    // 回放校验：thinking 开启时，输入里每个 function_call 前都必须紧跟 reasoning item；
    // 并行调用按调用复制该 reasoning（DeepSeek 实测必要）。
    let mut pending_reasoning: Option<(String, String)> = None;
    let mut calls_since_reasoning = 0usize;
    for msg in messages {
        match &msg.kind {
            MessageKind::User { text, .. } => {
                pending_reasoning = None;
                calls_since_reasoning = 0;
                items.push(json!({
                    "type": "message",
                    "role": "user",
                    "content": [{"type": "input_text", "text": text}],
                }));
            }
            MessageKind::Assistant { text } => items.push(json!({
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}],
            })),
            MessageKind::System { text } => {
                pending_reasoning = None;
                calls_since_reasoning = 0;
                items.push(json!({
                    "type": "message",
                    "role": "system",
                    "content": [{"type": "input_text", "text": text}],
                }));
            }
            MessageKind::Reasoning { id, text } => {
                pending_reasoning = Some((id.clone(), text.clone()));
                calls_since_reasoning = 0;
                if include_reasoning {
                    items.push(reasoning_item(id, text));
                }
            }
            MessageKind::ToolCall {
                entry,
                params,
                result,
                call_id,
            } => {
                if include_reasoning
                    && let Some((rid, rtext)) = &pending_reasoning
                    && calls_since_reasoning > 0
                {
                    items.push(reasoning_item(rid, rtext));
                }
                calls_since_reasoning += 1;
                let call_id = if call_id.is_empty() {
                    msg.id.to_string()
                } else {
                    call_id.clone()
                };
                let arguments = serde_json::to_string(params)
                    .map_err(|e| ModelError::Protocol(format!("参数序列化失败：{e}")))?;
                items.push(json!({
                    "type": "function_call",
                    "call_id": call_id,
                    "name": full_to_wire(entry),
                    "arguments": arguments,
                }));
                let output = match result {
                    Ok(v) => serde_json::to_string(v)
                        .map_err(|e| ModelError::Protocol(format!("结果序列化失败：{e}")))?,
                    Err(e) => serde_json::to_string(&json!({"error": e}))
                        .map_err(|e| ModelError::Protocol(format!("错误序列化失败：{e}")))?,
                };
                items.push(json!({
                    "type": "function_call_output",
                    "call_id": call_id,
                    "output": output,
                }));
            }
        }
    }
    Ok(items)
}

fn reasoning_item(id: &str, text: &str) -> Value {
    json!({
        "type": "reasoning",
        "id": id,
        "summary": [{"type": "summary_text", "text": text}],
        "content": [{"type": "reasoning_text", "text": text}],
    })
}

/// 内部 Message 树 → Chat Completions messages（附件内联数据时转 image_url base64）。
pub(crate) fn messages_to_cc(messages: &[Message]) -> Vec<Value> {
    let mut out = Vec::new();
    for msg in messages {
        match &msg.kind {
            MessageKind::User {
                text, attachments, ..
            } => {
                let mut content: Vec<Value> = Vec::new();
                for att in attachments {
                    if let Some(data) = &att.data_base64 {
                        let mime = att.mime.as_deref().unwrap_or("application/octet-stream");
                        content.push(json!({
                            "type": "image_url",
                            "image_url": {
                                "url": format!("data:{mime};base64,{data}"),
                                "detail": "high",
                            },
                        }));
                    }
                }
                if !text.is_empty() || content.is_empty() {
                    content.push(json!({"type": "text", "text": text}));
                }
                out.push(json!({"role": "user", "content": content}));
            }
            MessageKind::Assistant { text } => {
                out.push(json!({"role": "assistant", "content": text}));
            }
            MessageKind::System { text } => {
                out.push(json!({"role": "system", "content": text}));
            }
            // Chat Completions 无 reasoning 概念：忽略。
            MessageKind::Reasoning { .. } => {}
            MessageKind::ToolCall {
                entry,
                params,
                result,
                call_id,
            } => {
                let call_id = if call_id.is_empty() {
                    msg.id.to_string()
                } else {
                    call_id.clone()
                };
                let arguments = serde_json::to_string(params).unwrap_or_else(|_| "{}".into());
                out.push(json!({
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": call_id,
                        "type": "function",
                        "function": {
                            "name": full_to_wire(entry),
                            "arguments": arguments,
                        },
                    }],
                }));
                let output = match result {
                    Ok(v) => serde_json::to_string(v).unwrap_or_else(|_| "{}".into()),
                    Err(e) => {
                        serde_json::to_string(&json!({"error": e})).unwrap_or_else(|_| "{}".into())
                    }
                };
                out.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output,
                }));
            }
        }
    }
    out
}

pub(crate) fn map_status_error(status: reqwest::StatusCode, body: &str) -> ModelError {
    let body = body.chars().take(500).collect::<String>();
    match status.as_u16() {
        401 => ModelError::AuthFailed(body),
        402 => ModelError::QuotaExceeded(body),
        404 => ModelError::ModelNotFound(body),
        429 => ModelError::RateLimited(body),
        400 | 422 => ModelError::Protocol(body),
        _ => ModelError::Transport(format!("HTTP {status}: {body}")),
    }
}

pub(crate) fn reqwest_chain(e: &reqwest::Error) -> String {
    let mut out = e.to_string();
    let mut src = e.source();
    while let Some(s) = src {
        out.push_str(&format!(" <- {s}"));
        src = s.source();
    }
    out
}

/// 请求被拒时的输入摘要（调试用）。
pub(crate) fn summarize_input(input: &Value) -> String {
    let items = input.as_array().map(|a| a.len()).unwrap_or(0);
    let mut parts: Vec<String> = Vec::new();
    if let Some(arr) = input.as_array() {
        for item in arr {
            match item["type"].as_str() {
                Some("message") => {
                    let role = item["role"].as_str().unwrap_or("?");
                    let text = item["content"][0]["text"].as_str().unwrap_or_default();
                    parts.push(format!(
                        "msg({role},len={},head={:?})",
                        text.len(),
                        text.chars().take(20).collect::<String>()
                    ));
                }
                Some("reasoning") => {
                    let id = item["id"].as_str().unwrap_or("?");
                    let clen = item["content"][0]["text"]
                        .as_str()
                        .map(|s| s.len())
                        .unwrap_or(0);
                    let slen = item["summary"][0]["text"]
                        .as_str()
                        .map(|s| s.len())
                        .unwrap_or(0);
                    parts.push(format!(
                        "reasoning(id={id},content_len={clen},summary_len={slen})"
                    ));
                }
                Some("function_call") => {
                    parts.push(format!(
                        "call(name={},call_id={})",
                        item["name"].as_str().unwrap_or("?"),
                        item["call_id"].as_str().unwrap_or("?")
                    ));
                }
                Some("function_call_output") => {
                    parts.push(format!(
                        "output(call_id={})",
                        item["call_id"].as_str().unwrap_or("?")
                    ));
                }
                other => parts.push(format!("item({other:?})")),
            }
        }
    }
    format!("input_items={items} [{}]", parts.join(" "))
}

// ---------- SSE ----------

pub(crate) struct SseEvent {
    pub(crate) name: String,
    pub(crate) data: String,
}

#[derive(Default)]
pub(crate) struct SseParser {
    buffer: Vec<u8>,
    event: String,
    data: String,
}

impl SseParser {
    pub(crate) fn push_chunk(&mut self, bytes: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(bytes);
        let mut events = Vec::new();
        while let Some(pos) = self.buffer.iter().position(|&b| b == b'\n') {
            let line_bytes: Vec<u8> = self.buffer.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line_bytes[..line_bytes.len() - 1])
                .trim_end_matches('\r')
                .to_string();
            if line.is_empty() {
                if !self.event.is_empty() || !self.data.is_empty() {
                    events.push(SseEvent {
                        name: std::mem::take(&mut self.event),
                        data: std::mem::take(&mut self.data),
                    });
                }
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event = v.trim().to_string();
            } else if let Some(v) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(v.trim());
            }
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Message;

    #[test]
    fn sse_parser_extracts_named_events() {
        let mut parser = SseParser::default();
        let events = parser.push_chunk(
            b"event: response.output_text.delta\ndata: {\"delta\":\"hi\"}\n\nevent: response.completed\ndata: {\"usage\":{}}\n\n",
        );
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].name, "response.output_text.delta");
        assert_eq!(parse_delta(&events[0].data), "hi");
        assert_eq!(events[1].name, "response.completed");
    }

    #[test]
    fn reasoning_item_replays_text_with_id() {
        let mut msg = Message::system("占位");
        msg.kind = MessageKind::Reasoning {
            id: "rs_1".into(),
            text: "先计算再调用工具".into(),
        };
        let items = messages_to_responses_input(&[msg]).unwrap();
        assert_eq!(items[0]["type"], "reasoning");
        assert_eq!(items[0]["id"], "rs_1");
        assert_eq!(items[0]["content"][0]["type"], "reasoning_text");
        assert_eq!(items[0]["content"][0]["text"], "先计算再调用工具");
        assert_eq!(items[0]["summary"][0]["type"], "summary_text");
    }

    #[test]
    fn parallel_calls_repeat_reasoning_per_call() {
        let mut reasoning = Message::system("占位");
        reasoning.kind = MessageKind::Reasoning {
            id: "rs_1".into(),
            text: "并行读图".into(),
        };
        let call = |i: u32, cid: &str| {
            Message::tool_call_with_id(
                "vision::read",
                json!({"file": format!("/tmp/p{i}.png")}),
                Ok(json!({"ok": true})),
                cid.into(),
            )
        };
        let messages = vec![
            Message::user("都看看"),
            reasoning,
            call(1, "call_00"),
            call(2, "call_01"),
            call(3, "call_02"),
        ];
        let items = messages_to_responses_input(&messages).unwrap();
        let kinds: Vec<&str> = items.iter().map(|i| i["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            vec![
                "message",
                "reasoning",
                "function_call",
                "function_call_output",
                "reasoning",
                "function_call",
                "function_call_output",
                "reasoning",
                "function_call",
                "function_call_output",
            ]
        );
        assert_eq!(items[4]["id"], "rs_1");
    }
}
