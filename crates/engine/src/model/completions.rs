//! OpenAI 兼容 Chat Completions 流式适配器（M3）：视觉模型 / Ollama 等兼容端。

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio_stream::wrappers::ReceiverStream;

use crate::model::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelRequest, ModelService, ModelStream,
    ResponseFormat, TokenUsage, ToolChoice,
};

use super::openai::{SseParser, map_status_error, messages_to_cc, reqwest_chain};

// ---------- Chat Completions ----------

/// Chat Completions 工具定义：`{"type":"function","function":{...}}` 包装
/// （与 Responses API 的平铺格式不同——标准 Chat Completions 要求 function 键）。
fn tool_to_chat_function(t: &crate::model::ToolSchema) -> Value {
    json!({
        "type": "function",
        "function": {
            "name": t.name,
            "description": t.description,
            "parameters": t.input_schema,
        }
    })
}

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
            body["tools"] = json!(tools.iter().map(tool_to_chat_function).collect::<Vec<_>>());
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
            // thinking 模式：reasoning 文本流要带 item 边界（Start/End），
            // 否则 reasoning 进不了消息历史，下一轮无法回传（opencode 端点要求）。
            let mut reasoning_started = false;
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
                                    if reasoning_started {
                                        let _ = tx
                                            .send(Ok(ModelChunk::ItemDone {
                                                kind: ItemKind::Reasoning,
                                            }))
                                            .await;
                                    }
                                    for (index, (call_id, name, args)) in calls.iter() {
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
                                                // 修复：发送拼接好的 arguments（此前误发空串导致工具参数丢失）
                                                data: args.clone(),
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
                                    // OpenAI 兼容聚合端（opencode-go / 部分本地代理）按 OpenAI 标准
                                    // 透传 usage，常缺 prompt_cache_hit/miss_tokens；必须按 &Value 索引
                                    // （Value::Index 对缺失键返回 Null），不能窄化为 Map 后用 []——
                                    // Map::Index 委派到 BTreeMap，缺键触发 expect("no entry found for key")。
                                    if v["usage"].is_object() {
                                        let u = parse_usage(&v["usage"]);
                                        let _ = tx.send(Ok(ModelChunk::Usage(u))).await;
                                    }
                                    let delta = &v["choices"][0]["delta"];
                                    if let Some(t) = delta["content"].as_str() {
                                        text_done = true;
                                        let _ =
                                            tx.send(Ok(ModelChunk::TextDelta(t.to_string()))).await;
                                    }
                                    if let Some(r) = delta["reasoning_content"].as_str() {
                                        if !reasoning_started {
                                            reasoning_started = true;
                                            let _ = tx
                                                .send(Ok(ModelChunk::ReasoningItemStart {
                                                    id: format!("r{}", calls.len()),
                                                }))
                                                .await;
                                        }
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

/// Chat Completions usage 解析：缺字段返回 `None`，不 panic。
///
/// 设计契约：
/// - 入参是 `&Value`（不是 `&Map`），用 `Value::Index`（缺失键返回 `Null`）。
/// - 标准 OpenAI usage 字段（`prompt_tokens` / `completion_tokens`）与
///   DeepSeek/自建端扩展字段（`prompt_cache_hit_tokens` / `prompt_cache_miss_tokens`）
///   同等对待：缺哪个都不应让 SSE 解析线程崩溃。
fn parse_usage(usage: &Value) -> TokenUsage {
    TokenUsage {
        input_tokens: usage["prompt_tokens"].as_u64(),
        output_tokens: usage["completion_tokens"].as_u64(),
        cached_tokens: usage["prompt_cache_hit_tokens"].as_u64(),
        cache_miss_tokens: usage["prompt_cache_miss_tokens"].as_u64(),
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 回归：opencode-go / 标准 OpenAI 兼容聚合端常省略 cache 字段——
    /// 此前 `usage["prompt_cache_hit_tokens"]` 在 `Map` 上索引触发
    /// `BTreeMap::Index::expect("no entry found for key")` panic，整条 SSE 流断。
    #[test]
    fn parse_usage_without_cache_fields_does_not_panic() {
        let v = json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "total_tokens": 150,
        });
        let u = parse_usage(&v);
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.output_tokens, Some(50));
        assert_eq!(u.cached_tokens, None);
        assert_eq!(u.cache_miss_tokens, None);
    }

    #[test]
    fn parse_usage_full() {
        let v = json!({
            "prompt_tokens": 100,
            "completion_tokens": 50,
            "prompt_cache_hit_tokens": 30,
            "prompt_cache_miss_tokens": 70,
        });
        let u = parse_usage(&v);
        assert_eq!(u.input_tokens, Some(100));
        assert_eq!(u.output_tokens, Some(50));
        assert_eq!(u.cached_tokens, Some(30));
        assert_eq!(u.cache_miss_tokens, Some(70));
    }

    #[test]
    fn parse_usage_minimal() {
        // 极端：只有 prompt_tokens，其它全缺。
        let v = json!({"prompt_tokens": 10});
        let u = parse_usage(&v);
        assert_eq!(u.input_tokens, Some(10));
        assert_eq!(u.output_tokens, None);
        assert_eq!(u.cached_tokens, None);
        assert_eq!(u.cache_miss_tokens, None);
    }
}
