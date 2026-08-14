//! Anthropic Messages API 适配器（M3）：流式 text/tool_use，SSE 归一化为 ModelChunk。

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio_stream::wrappers::ReceiverStream;

use crate::contract::full_to_wire;
use crate::message::{Message, MessageKind};
use crate::model::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelRequest, ModelService, ModelStream,
    TokenUsage, ToolChoice,
};

use super::openai::{SseParser, map_status_error, reqwest_chain};

/// Anthropic Messages API 适配器。
pub struct AnthropicModelService {
    client: Client,
    api_url: String,
    api_key: String,
    model: String,
    version: String,
    max_tokens: u32,
    timeout: Duration,
}

impl AnthropicModelService {
    pub fn new(api_url: String, api_key: String, model: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(Duration::from_secs(300))
                .build()
                .expect("reqwest client 构建失败"),
            api_url,
            api_key,
            model,
            version: "2023-06-01".into(),
            max_tokens: 4096,
            timeout: Duration::from_secs(60),
        }
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.version = version.into();
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    fn build_body(&self, request: &ModelRequest) -> Value {
        let payload = messages_to_anthropic(&request.messages);
        let mut body = json!({
            "model": self.model,
            "max_tokens": self.max_tokens,
            "system": payload.system,
            "messages": payload.messages,
            "stream": true,
        });
        if let Some(tools) = &request.tools {
            body["tools"] = json!(
                tools
                    .iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "description": t.description,
                            "input_schema": t.input_schema,
                        })
                    })
                    .collect::<Vec<_>>()
            );
        }
        if let Some(choice) = &request.tool_choice {
            body["tool_choice"] = match choice {
                ToolChoice::Auto => json!({"type": "auto"}),
                ToolChoice::Required => json!({"type": "any"}),
                ToolChoice::Function { name } => json!({"type": "tool", "name": name}),
            };
        }
        body
    }

    async fn post(&self, body: &Value) -> Result<reqwest::Response, ModelError> {
        let url = format!("{}/v1/messages", self.api_url.trim_end_matches('/'));
        tokio::time::timeout(
            self.timeout,
            self.client
                .post(&url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", &self.version)
                .json(body)
                .send(),
        )
        .await
        .map_err(|_| ModelError::Timeout)?
        .map_err(|e| ModelError::Transport(reqwest_chain(&e)))
    }
}

/// 转换结果：system 内容 + user/assistant 消息（相邻同角色已合并）。
pub(crate) struct AnthropicPayload {
    pub system: Vec<Value>,
    pub messages: Vec<Value>,
}

/// 内部 Message 树 → Anthropic Messages 请求体。
pub(crate) fn messages_to_anthropic(messages: &[Message]) -> AnthropicPayload {
    let mut system: Vec<Value> = Vec::new();
    let mut flat: Vec<(String, Vec<Value>)> = Vec::new();

    for msg in messages {
        match &msg.kind {
            MessageKind::System { text } => {
                system.push(json!({"type": "text", "text": text}));
            }
            MessageKind::User { text, .. } => {
                if !text.is_empty() {
                    flat.push(("user".into(), vec![json!({"type": "text", "text": text})]));
                }
            }
            MessageKind::Assistant { text } => {
                if !text.is_empty() {
                    flat.push((
                        "assistant".into(),
                        vec![json!({"type": "text", "text": text})],
                    ));
                }
            }
            // Anthropic thinking 块未接入：跳过（不影响主链路）。
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
                flat.push((
                    "assistant".into(),
                    vec![json!({
                        "type": "tool_use",
                        "id": call_id,
                        "name": full_to_wire(entry),
                        "input": params,
                    })],
                ));
                let output = match result {
                    Ok(v) => v.clone(),
                    Err(e) => json!({"error": e}),
                };
                flat.push((
                    "user".into(),
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": serde_json::to_string(&output).unwrap_or_default(),
                    })],
                ));
            }
        }
    }

    // Anthropic 要求相邻消息角色不能相同：合并。
    let mut messages: Vec<Value> = Vec::new();
    for (role, content) in flat {
        if let Some(last) = messages.last_mut()
            && last["role"] == role
        {
            if let Some(arr) = last["content"].as_array_mut() {
                arr.extend(content);
            }
        } else {
            messages.push(json!({"role": role, "content": content}));
        }
    }
    AnthropicPayload { system, messages }
}

#[async_trait::async_trait]
impl ModelService for AnthropicModelService {
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
            let mut block_kinds: std::collections::BTreeMap<usize, String> =
                std::collections::BTreeMap::new();
            let mut input_tokens: Option<u64> = None;
            let mut output_tokens: Option<u64> = None;
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
                            if ev.name == "ping" {
                                continue;
                            }
                            if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                                match ev.name.as_str() {
                                    "message_start" => {
                                        input_tokens =
                                            v["message"]["usage"]["input_tokens"].as_u64();
                                    }
                                    "content_block_start" => {
                                        let index = v["index"].as_u64().unwrap_or(0) as usize;
                                        let block = &v["content_block"];
                                        let kind = block["type"].as_str().unwrap_or("").to_string();
                                        if kind == "tool_use" {
                                            let call_id = block["id"]
                                                .as_str()
                                                .unwrap_or_default()
                                                .to_string();
                                            let name = block["name"]
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
                                        block_kinds.insert(index, kind);
                                    }
                                    "content_block_delta" => {
                                        let index = v["index"].as_u64().unwrap_or(0) as usize;
                                        let delta = &v["delta"];
                                        match delta["type"].as_str() {
                                            Some("text_delta") => {
                                                let text =
                                                    delta["text"].as_str().unwrap_or_default();
                                                let _ = tx
                                                    .send(Ok(ModelChunk::TextDelta(
                                                        text.to_string(),
                                                    )))
                                                    .await;
                                            }
                                            Some("input_json_delta") => {
                                                let data = delta["partial_json"]
                                                    .as_str()
                                                    .unwrap_or_default();
                                                let _ = tx
                                                    .send(Ok(ModelChunk::ToolCallDelta {
                                                        index,
                                                        data: data.to_string(),
                                                    }))
                                                    .await;
                                            }
                                            _ => {}
                                        }
                                    }
                                    "content_block_stop" => {
                                        let index = v["index"].as_u64().unwrap_or(0) as usize;
                                        match block_kinds.get(&index).map(|s| s.as_str()) {
                                            Some("text") => {
                                                let _ = tx
                                                    .send(Ok(ModelChunk::ItemDone {
                                                        kind: ItemKind::Message,
                                                    }))
                                                    .await;
                                            }
                                            Some("tool_use") => {
                                                let _ = tx
                                                    .send(Ok(ModelChunk::ItemDone {
                                                        kind: ItemKind::FunctionCall,
                                                    }))
                                                    .await;
                                            }
                                            _ => {}
                                        }
                                    }
                                    "message_delta" => {
                                        output_tokens = v["usage"]["output_tokens"].as_u64();
                                    }
                                    "message_stop" => {
                                        let _ = tx
                                            .send(Ok(ModelChunk::Usage(TokenUsage {
                                                input_tokens,
                                                output_tokens,
                                                ..Default::default()
                                            })))
                                            .await;
                                        let _ = tx.send(Ok(ModelChunk::Done)).await;
                                        done = true;
                                    }
                                    "error" => {
                                        let message = v["error"]["message"]
                                            .as_str()
                                            .unwrap_or("响应失败")
                                            .to_string();
                                        let _ = tx.send(Err(ModelError::Protocol(message))).await;
                                        done = true;
                                    }
                                    _ => {}
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
    fn merges_adjacent_same_role_and_maps_tool_calls() {
        let messages = vec![
            Message::system("你是助手"),
            Message::user("你好"),
            Message::user("继续"),
            Message::tool_call_with_id(
                "demo::echo",
                json!({"x": 1}),
                Ok(json!({"ok": true})),
                "call_1".into(),
            ),
            Message::assistant("完成"),
        ];
        let payload = messages_to_anthropic(&messages);
        assert_eq!(payload.system.len(), 1);
        assert_eq!(payload.messages.len(), 4);
        // 相邻两个 user 合并成一条。
        assert_eq!(payload.messages[0]["role"], "user");
        assert_eq!(payload.messages[0]["content"].as_array().unwrap().len(), 2);
        // 工具调用拆成 assistant(tool_use) + user(tool_result)。
        assert_eq!(payload.messages[1]["role"], "assistant");
        assert_eq!(payload.messages[1]["content"][0]["type"], "tool_use");
        assert_eq!(payload.messages[1]["content"][0]["name"], "demo__echo");
        assert_eq!(payload.messages[2]["role"], "user");
        assert_eq!(payload.messages[2]["content"][0]["type"], "tool_result");
        assert_eq!(payload.messages[3]["role"], "assistant");
    }

    #[test]
    fn empty_text_user_skipped() {
        let msg = Message::user("");
        let payload = messages_to_anthropic(&[msg]);
        assert!(payload.messages.is_empty());
    }
}
