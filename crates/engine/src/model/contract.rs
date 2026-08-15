//! 模型通用契约：ModelService 抽象、请求/响应类型与流式事件归一化（ModelChunk）。

use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};

use crate::message::Message;

use super::handle::AbortSignal;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    Main,
    Vision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseFormat {
    JsonObject,
    /// OpenAI 兼容端 `text.format` 的 json_schema 模式（服务端强制结构）。
    JsonSchema {
        name: String,
        schema: serde_json::Value,
    },
}

#[derive(Debug, Clone)]
pub struct ModelRequest {
    pub model: ModelKind,
    pub messages: Vec<Message>,
    pub tools: Option<Vec<ToolSchema>>,
    /// 思考模式 effort（none/minimal/low/medium/high/xhigh/max）。
    pub reasoning_effort: Option<String>,
    pub response_format: Option<ResponseFormat>,
    /// 工具选择策略：强制调用指定工具时用 Function{name}（API 要求关闭思考模式）。
    pub tool_choice: Option<ToolChoice>,
}

impl ModelRequest {
    pub fn chat(model: ModelKind, messages: Vec<Message>) -> Self {
        Self {
            model,
            messages,
            tools: None,
            reasoning_effort: None,
            response_format: None,
            tool_choice: None,
        }
    }
}

/// 工具选择策略（OpenAI Responses 兼容）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChoice {
    Auto,
    Required,
    /// 强制调用指定工具（wire name）。
    Function {
        name: String,
    },
}

/// 模型可见工具（wire name + JSON Schema + GUI 展示元数据）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
    /// 用户友好显示名（GUI 工具面板展示；缺省回退 name）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Iconify 图标名（如 "mdi:lightbulb-on-outline"，GUI 展示用）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// 所属插件的显示名（GUI 按命名空间分组展示；缺省回退 namespace）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace_title: Option<String>,
    /// 所属插件的 Iconify 图标名（GUI 分组展示）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace_icon: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ItemKind {
    Message,
    FunctionCall,
    Reasoning,
}

#[derive(Debug, Clone)]
pub enum ModelChunk {
    TextDelta(String),
    ReasoningDelta(String),
    /// 推理 item 开始（携带 id，后续轮次必须按 id 回传给 API）。
    ReasoningItemStart {
        id: String,
    },
    ToolCallStart {
        index: usize,
        call_id: String,
        name: String,
    },
    ToolCallDelta {
        index: usize,
        data: String,
    },
    ItemDone {
        kind: ItemKind,
    },
    /// 完整响应中的 token 用量。
    Usage(TokenUsage),
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallSpec {
    pub call_id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cached_tokens: Option<u64>,
    pub cache_miss_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ModelResponse {
    pub text: String,
    pub tool_calls: Vec<ToolCallSpec>,
    pub usage: Option<TokenUsage>,
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum ModelError {
    #[error("鉴权失败：{0}")]
    AuthFailed(String),
    #[error("余额或配额不足：{0}")]
    QuotaExceeded(String),
    #[error("模型不存在或已下架：{0}")]
    ModelNotFound(String),
    #[error("请求超时")]
    Timeout,
    #[error("被取消")]
    Cancelled,
    #[error("限流：{0}")]
    RateLimited(String),
    #[error("传输错误：{0}")]
    Transport(String),
    #[error("协议错误：{0}")]
    Protocol(String),
    #[error("配置缺失：{0}")]
    Config(String),
}

impl ModelError {
    /// 系统性错误：重试/换参数无意义，应中断回合。
    pub fn is_systemic(&self) -> bool {
        matches!(
            self,
            ModelError::AuthFailed(_) | ModelError::QuotaExceeded(_) | ModelError::ModelNotFound(_)
        )
    }
}

pub type ModelStream = Box<dyn Stream<Item = Result<ModelChunk, ModelError>> + Send + Unpin>;

/// Capability seam（ADR-0006）：model 能力的 Service Definition。
/// 纯净 provider 抽象（不管超时/审计；护栏在包装层与 loop）。
#[async_trait]
pub trait ModelService: Send + Sync {
    async fn stream(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError>;

    async fn complete(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        use futures_util::StreamExt;

        let mut stream = self.stream(request, signal).await?;
        let mut text = String::new();
        let mut calls: Vec<(usize, ToolCallSpec)> = Vec::new();
        let mut usage_holder: Option<TokenUsage> = None;
        while let Some(chunk) = stream.next().await {
            match chunk? {
                ModelChunk::TextDelta(d) => text.push_str(&d),
                ModelChunk::ToolCallStart {
                    index,
                    call_id,
                    name,
                } => {
                    calls.push((
                        index,
                        ToolCallSpec {
                            call_id,
                            name,
                            arguments: String::new(),
                        },
                    ));
                }
                ModelChunk::ToolCallDelta { index, data } => {
                    if let Some((_, spec)) = calls.iter_mut().find(|(i, _)| *i == index) {
                        spec.arguments.push_str(&data);
                    }
                }
                ModelChunk::Usage(usage) => {
                    usage_holder = Some(usage);
                }
                _ => {}
            }
        }
        Ok(ModelResponse {
            text,
            tool_calls: calls.into_iter().map(|(_, spec)| spec).collect(),
            usage: usage_holder,
        })
    }
}
