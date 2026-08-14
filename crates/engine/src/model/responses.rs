//! Responses API 适配器（M3）：DeepSeek / OpenAI 兼容端流式实现（SSE 语义事件，无状态全量历史）。

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{Value, json};
use tokio_stream::wrappers::ReceiverStream;

use crate::model::{
    AbortSignal, ItemKind, ModelChunk, ModelError, ModelRequest, ModelService, ModelStream,
    TokenUsage, ToolChoice,
};

use super::openai::{
    SseParser, map_status_error, messages_to_responses_input,
    messages_to_responses_input_no_reasoning, parse_delta, reqwest_chain, responses_endpoint,
    summarize_input, text_format, tool_to_function,
};
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

#[cfg(test)]
mod tests {
    use super::*;

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
