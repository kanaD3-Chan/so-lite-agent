//! OpenAI 兼容适配器（M3）：Responses API 与 Chat Completions 流式实现，
//! 覆盖 DeepSeek / SiliconFlow / Ollama 等 OpenAI 兼容端点。

use std::error::Error;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio_stream::wrappers::ReceiverStream;

use crate::contract::full_to_wire;
use crate::message::{Message, MessageKind};
use crate::model::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelRequest, ModelService, ModelStream,
    ProviderRegistry, ResponseFormat, TokenUsage, ToolChoice, ToolSchema,
};

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
fn summarize_input(input: &Value) -> String {
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

fn parse_usage(v: &Value) -> TokenUsage {
    let input_tokens = v["input_tokens"].as_u64();
    let cached_tokens = v["input_tokens_details"]["cached_tokens"].as_u64();
    TokenUsage {
        input_tokens,
        output_tokens: v["output_tokens"].as_u64(),
        cached_tokens,
        cache_miss_tokens: match (input_tokens, cached_tokens) {
            (Some(i), Some(c)) => Some(i.saturating_sub(c)),
            _ => None,
        },
        reasoning_tokens: v["output_tokens_details"]["reasoning_tokens"].as_u64(),
    }
}

// ---------- Responses API ----------

/// DeepSeek Responses API 适配器（SSE 语义事件，无状态全量历史）。
pub struct ResponsesModelService {
    client: Client,
    api_url: String,
    api_key: String,
    model: String,
    timeout: Duration,
}

impl ResponsesModelService {
    pub fn new(api_url: String, api_key: String, model: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client 构建失败"),
            api_url,
            api_key,
            model,
            timeout: Duration::from_secs(60),
        }
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn build_body(&self, request: &ModelRequest) -> Result<Value, ModelError> {
        let mut body = json!({
            "model": self.model,
            "input": messages_to_responses_input(&request.messages)?,
            "stream": true,
        });
        if let Some(tools) = &request.tools {
            body["tools"] = json!(tools.iter().map(tool_to_function).collect::<Vec<_>>());
        }
        if let Some(effort) = &request.reasoning_effort {
            body["reasoning"] = json!({"effort": effort});
        }
        if let Some(fmt) = &request.response_format {
            body["text"] = json!({"format": text_format(fmt)});
        }
        if let Some(choice) = &request.tool_choice {
            body["tool_choice"] = match choice {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Function { name } => json!({
                    "type": "function",
                    "name": name,
                }),
            };
            // API 限制：thinking 模式不支持 tool_choice，强制调用时关闭思考。
            body["reasoning"] = json!({"effort": "none"});
        }
        Ok(body)
    }

    /// 兜底请求体：剥离全部 reasoning + thinking 关闭。
    fn build_body_no_reasoning(&self, request: &ModelRequest) -> Result<Value, ModelError> {
        let mut body = json!({
            "model": self.model,
            "input": messages_to_responses_input_no_reasoning(&request.messages)?,
            "stream": true,
            "reasoning": {"effort": "none"},
        });
        if let Some(tools) = &request.tools {
            body["tools"] = json!(tools.iter().map(tool_to_function).collect::<Vec<_>>());
        }
        if let Some(fmt) = &request.response_format {
            body["text"] = json!({"format": text_format(fmt)});
        }
        Ok(body)
    }

    async fn post(&self, url: &str, body: &Value) -> Result<reqwest::Response, ModelError> {
        tokio::time::timeout(
            self.timeout,
            self.client
                .post(url)
                .bearer_auth(&self.api_key)
                .json(body)
                .send(),
        )
        .await
        .map_err(|_| ModelError::Timeout)?
        .map_err(|e| ModelError::Transport(reqwest_chain(&e)))
    }
}

#[async_trait::async_trait]
impl ModelService for ResponsesModelService {
    async fn stream(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let body = self.build_body(request)?;
        let url = responses_endpoint(&self.api_url);
        let mut response = self.post(&url, &body).await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            log::warn!(
                "Responses 请求被拒（{status}）：{} tools={}",
                summarize_input(&body["input"]),
                body["tools"].as_array().map(|a| a.len()).unwrap_or(0),
            );
            let err = map_status_error(status, &text);
            // reasoning 回放校验失败时兜底重试一次：剥离 reasoning + effort=none。
            if let ModelError::Protocol(msg) = &err
                && msg.contains("reasoning_text")
            {
                log::warn!("reasoning 回放被拒，兜底重试：剥离 reasoning + reasoning.effort=none");
                let fallback = self.build_body_no_reasoning(request)?;
                response = self.post(&url, &fallback).await?;
                if !response.status().is_success() {
                    let status2 = response.status();
                    let text2 = response.text().await.unwrap_or_default();
                    log::warn!(
                        "兜底重试仍被拒（{status2}）：{} tools={}",
                        summarize_input(&fallback["input"]),
                        fallback["tools"].as_array().map(|a| a.len()).unwrap_or(0),
                    );
                    return Err(map_status_error(status2, &text2));
                }
            } else {
                return Err(err);
            }
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ModelChunk, ModelError>>(128);
        let mut byte_stream = response.bytes_stream();
        let cancel = signal.cancelled();
        tokio::spawn(async move {
            let mut parser = SseParser::default();
            let mut last_tool_index = 0usize;
            let mut done = false;
            loop {
                let next = tokio::select! {
                    chunk = byte_stream.next() => chunk,
                    _ = cancel.cancelled() => None,
                };
                let Some(chunk) = next else { break };
                match chunk {
                    Ok(bytes) => {
                        for ev in parser.push_chunk(&bytes) {
                            match ev.name.as_str() {
                                "response.output_item.added" => {
                                    if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                        let item = &v["item"];
                                        if item["type"] == "reasoning" {
                                            let id =
                                                item["id"].as_str().unwrap_or_default().to_string();
                                            let _ = tx
                                                .send(Ok(ModelChunk::ReasoningItemStart { id }))
                                                .await;
                                        } else if item["type"] == "function_call" {
                                            last_tool_index += 1;
                                            let index = v["output_index"]
                                                .as_u64()
                                                .map(|i| i as usize)
                                                .unwrap_or(last_tool_index);
                                            let call_id = item["call_id"]
                                                .as_str()
                                                .unwrap_or_default()
                                                .to_string();
                                            let name = item["name"]
                                                .as_str()
                                                .unwrap_or_default()
                                                .to_string();
                                            let _ = tx
                                                .send(Ok(ModelChunk::ToolCallStart {
                                                    index,
                                                    call_id,
                                                    name,
                                                }))
                                                .await;
                                        }
                                    }
                                }
                                "response.output_item.done" => {
                                    if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                        match v["item"]["type"].as_str() {
                                            Some("message") => {
                                                let _ = tx
                                                    .send(Ok(ModelChunk::ItemDone {
                                                        kind: ItemKind::Message,
                                                    }))
                                                    .await;
                                            }
                                            Some("function_call") => {
                                                let _ = tx
                                                    .send(Ok(ModelChunk::ItemDone {
                                                        kind: ItemKind::FunctionCall,
                                                    }))
                                                    .await;
                                            }
                                            Some("reasoning") => {
                                                let _ = tx
                                                    .send(Ok(ModelChunk::ItemDone {
                                                        kind: ItemKind::Reasoning,
                                                    }))
                                                    .await;
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                                "response.reasoning_text.delta" => {
                                    let _ = tx
                                        .send(Ok(ModelChunk::ReasoningDelta(parse_delta(&ev.data))))
                                        .await;
                                }
                                "response.output_text.delta" => {
                                    let _ = tx
                                        .send(Ok(ModelChunk::TextDelta(parse_delta(&ev.data))))
                                        .await;
                                }
                                "response.function_call_arguments.delta" => {
                                    if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                        let index = v["output_index"]
                                            .as_u64()
                                            .map(|i| i as usize)
                                            .unwrap_or(last_tool_index);
                                        let data =
                                            v["delta"].as_str().unwrap_or_default().to_string();
                                        let _ = tx
                                            .send(Ok(ModelChunk::ToolCallDelta { index, data }))
                                            .await;
                                    }
                                }
                                "response.completed" | "response.incomplete" => {
                                    if !done {
                                        if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                            let usage_src = if v["response"]["usage"].is_object() {
                                                &v["response"]["usage"]
                                            } else {
                                                &v["usage"]
                                            };
                                            let _ = tx
                                                .send(Ok(ModelChunk::Usage(parse_usage(usage_src))))
                                                .await;
                                        }
                                        let _ = tx.send(Ok(ModelChunk::Done)).await;
                                        done = true;
                                    }
                                }
                                "response.failed" if !done => {
                                    let message = serde_json::from_str::<Value>(&ev.data)
                                        .ok()
                                        .and_then(|v| {
                                            v["error"]["message"].as_str().map(|s| s.to_string())
                                        })
                                        .unwrap_or_else(|| "响应失败".into());
                                    let _ = tx.send(Err(ModelError::Protocol(message))).await;
                                    done = true;
                                }
                                _ => {}
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(ModelError::Transport(e.to_string()))).await;
                        break;
                    }
                }
            }
            if !done {
                let _ = tx.send(Ok(ModelChunk::Done)).await;
            }
        });

        Ok(Box::new(ReceiverStream::new(rx)))
    }
}

// ---------- Chat Completions ----------

/// OpenAI 兼容 Chat Completions 流式适配器（视觉模型 / Ollama 等兼容端）。
pub struct ChatCompletionsModelService {
    client: Client,
    api_url: String,
    api_key: String,
    model: String,
    max_tokens: u32,
    timeout: Duration,
}

impl ChatCompletionsModelService {
    pub fn new(api_url: String, api_key: String, model: String, max_tokens: u32) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client 构建失败"),
            api_url,
            api_key,
            model,
            max_tokens,
            timeout: Duration::from_secs(60),
        }
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn build_body(&self, request: &ModelRequest) -> Value {
        let mut body = json!({
            "model": self.model,
            "messages": messages_to_cc(&request.messages),
            "max_tokens": self.max_tokens,
            "stream": true,
        });
        if let Some(tools) = &request.tools {
            body["tools"] = json!(tools.iter().map(tool_to_function).collect::<Vec<_>>());
        }
        if let Some(effort) = &request.reasoning_effort {
            body["reasoning_effort"] = json!(effort);
        }
        if let Some(fmt) = &request.response_format {
            body["response_format"] = match fmt {
                ResponseFormat::JsonObject => json!({"type": "json_object"}),
                // json_schema 降级为 json_object（部分兼容端不支持）。
                ResponseFormat::JsonSchema { .. } => json!({"type": "json_object"}),
            };
        }
        if let Some(choice) = &request.tool_choice {
            body["tool_choice"] = match choice {
                ToolChoice::Auto => json!("auto"),
                ToolChoice::Required => json!("required"),
                ToolChoice::Function { name } => json!({
                    "type": "function",
                    "function": {"name": name},
                }),
            };
        }
        body
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response, ModelError> {
        let url = format!("{}/chat/completions", self.api_url.trim_end_matches('/'));
        tokio::time::timeout(
            self.timeout,
            self.client
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(body)
                .send(),
        )
        .await
        .map_err(|_| ModelError::Timeout)?
        .map_err(|e| ModelError::Transport(reqwest_chain(&e)))
    }
}

#[async_trait::async_trait]
impl ModelService for ChatCompletionsModelService {
    async fn stream(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        let body = self.build_body(request);
        let response = self.post(&body).await?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(map_status_error(status, &text));
        }

        let (tx, rx) = tokio::sync::mpsc::channel::<Result<ModelChunk, ModelError>>(128);
        let mut byte_stream = response.bytes_stream();
        let cancel = signal.cancelled();
        tokio::spawn(async move {
            let mut parser = SseParser::default();
            let mut calls: std::collections::BTreeMap<usize, (String, String, String)> =
                std::collections::BTreeMap::new();
            let mut text_done = false;
            let mut done = false;
            loop {
                let next = tokio::select! {
                    chunk = byte_stream.next() => chunk,
                    _ = cancel.cancelled() => None,
                };
                let Some(chunk) = next else { break };
                match chunk {
                    Ok(bytes) => {
                        for ev in parser.push_chunk(&bytes) {
                            if ev.name == "data" || ev.name.is_empty() {
                                let data = ev.data.trim();
                                if data == "[DONE]" {
                                    for (index, (call_id, name, _args)) in calls.iter() {
                                        let _ = tx
                                            .send(Ok(ModelChunk::ToolCallStart {
                                                index: *index,
                                                call_id: call_id.clone(),
                                                name: name.clone(),
                                            }))
                                            .await;
                                        let _ = tx
                                            .send(Ok(ModelChunk::ToolCallDelta {
                                                index: *index,
                                                data: String::new(),
                                            }))
                                            .await;
                                        let _ = tx
                                            .send(Ok(ModelChunk::ItemDone {
                                                kind: ItemKind::FunctionCall,
                                            }))
                                            .await;
                                    }
                                    if text_done {
                                        let _ = tx
                                            .send(Ok(ModelChunk::ItemDone {
                                                kind: ItemKind::Message,
                                            }))
                                            .await;
                                    }
                                    let _ = tx.send(Ok(ModelChunk::Done)).await;
                                    done = true;
                                    break;
                                }
                                if let Ok(v) = serde_json::from_str::<Value>(data) {
                                    if let Some(usage) = v["usage"].as_object() {
                                        let u = TokenUsage {
                                            input_tokens: usage["prompt_tokens"].as_u64(),
                                            output_tokens: usage["completion_tokens"].as_u64(),
                                            cached_tokens: usage["prompt_cache_hit_tokens"]
                                                .as_u64(),
                                            cache_miss_tokens: usage["prompt_cache_miss_tokens"]
                                                .as_u64(),
                                            ..Default::default()
                                        };
                                        let _ = tx.send(Ok(ModelChunk::Usage(u))).await;
                                    }
                                    let delta = &v["choices"][0]["delta"];
                                    if let Some(t) = delta["content"].as_str() {
                                        text_done = true;
                                        let _ =
                                            tx.send(Ok(ModelChunk::TextDelta(t.to_string()))).await;
                                    }
                                    if let Some(r) = delta["reasoning_content"].as_str() {
                                        let _ = tx
                                            .send(Ok(ModelChunk::ReasoningDelta(r.to_string())))
                                            .await;
                                    }
                                    if let Some(calls_arr) = delta["tool_calls"].as_array() {
                                        for call in calls_arr {
                                            let index =
                                                call["index"].as_u64().unwrap_or(0) as usize;
                                            let entry = calls.entry(index).or_insert_with(|| {
                                                (String::new(), String::new(), String::new())
                                            });
                                            if let Some(id) = call["id"].as_str() {
                                                entry.0 = id.to_string();
                                            }
                                            if let Some(name) = call["function"]["name"].as_str() {
                                                entry.1 = name.to_string();
                                            }
                                            if let Some(args) =
                                                call["function"]["arguments"].as_str()
                                            {
                                                entry.2.push_str(args);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(ModelError::Transport(e.to_string()))).await;
                        break;
                    }
                }
            }
            if !done {
                let _ = tx.send(Ok(ModelChunk::Done)).await;
            }
        });

        Ok(Box::new(ReceiverStream::new(rx)))
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

    #[test]
    fn usage_parses_cached_tokens() {
        let v = json!({
            "input_tokens": 100,
            "output_tokens": 20,
            "input_tokens_details": {"cached_tokens": 60},
            "output_tokens_details": {"reasoning_tokens": 5},
        });
        let u = parse_usage(&v);
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.cached_tokens, Some(60));
        assert_eq!(u.cache_miss_tokens, Some(40));
        assert_eq!(u.reasoning_tokens, Some(5));
    }
}
