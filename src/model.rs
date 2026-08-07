//! 模型 Provider 层（pi-ai 等价物）：ModelService 抽象、流式事件归一化、
//! Provider 注册表（M2 骨架）与 Mock 桩。

use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

use crate::audit::{AuditRecord, Auditor};
use crate::message::Message;

// ---------- 取消信号（SIGTERM 通道；SIGKILL 由 dispatch 任务 abort 承担） ----------

#[derive(Clone)]
pub struct AbortSignal {
    token: CancellationToken,
}

impl AbortSignal {
    pub fn new() -> Self {
        Self {
            token: CancellationToken::new(),
        }
    }

    pub fn from_token(token: CancellationToken) -> Self {
        Self { token }
    }

    pub fn cancel(&self) {
        self.token.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub fn cancelled(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Default for AbortSignal {
    fn default() -> Self {
        Self::new()
    }
}

// ---------- Model 契约 ----------

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

/// 模型可见工具（wire name + JSON Schema）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchema {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
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

/// 注入插件的模型受控句柄：只暴露带超时 + abort + 审计的 complete。
#[derive(Clone)]
pub struct ModelHandle {
    inner: Arc<dyn ModelService>,
    timeout: Duration,
    auditor: Auditor,
}

impl ModelHandle {
    pub fn new(inner: Arc<dyn ModelService>, timeout: Duration, auditor: Auditor) -> Self {
        Self {
            inner,
            timeout,
            auditor,
        }
    }

    /// 内核装配用：取回底层服务（KernelBuilder / 内核插件）。
    pub fn inner(&self) -> Arc<dyn ModelService> {
        self.inner.clone()
    }

    pub async fn complete(
        &self,
        request: &ModelRequest,
        signal: &AbortSignal,
    ) -> Result<ModelResponse, ModelError> {
        let started = std::time::Instant::now();
        let result = tokio::time::timeout(self.timeout, self.inner.complete(request, signal)).await;
        let duration_ms = started.elapsed().as_millis() as u64;
        match result {
            Ok(Ok(resp)) => {
                self.auditor.record(AuditRecord::LlmCall {
                    provider: "handle".into(),
                    model: match request.model {
                        ModelKind::Main => "main".into(),
                        ModelKind::Vision => "vision".into(),
                    },
                    kind: "complete".into(),
                    tokens_in: resp.usage.as_ref().and_then(|u| u.input_tokens),
                    tokens_out: resp.usage.as_ref().and_then(|u| u.output_tokens),
                    duration_ms,
                    ok: true,
                });
                Ok(resp)
            }
            Ok(Err(e)) => {
                self.auditor.record(AuditRecord::LlmCall {
                    provider: "handle".into(),
                    model: match request.model {
                        ModelKind::Main => "main".into(),
                        ModelKind::Vision => "vision".into(),
                    },
                    kind: "complete".into(),
                    tokens_in: None,
                    tokens_out: None,
                    duration_ms,
                    ok: false,
                });
                Err(e)
            }
            Err(_) => Err(ModelError::Timeout),
        }
    }
}

// ---------- Provider 注册表（M2 骨架：登记与查询；HTTP 适配器 M3） ----------

/// Provider 注册表：使用方注册具名模型服务，供 KernelBuilder / 插件按名取用。
/// 不做全局可变状态，实例由使用方持有。
#[derive(Default, Clone)]
pub struct ProviderRegistry {
    providers: Arc<RwLock<HashMap<String, Arc<dyn ModelService>>>>,
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// 注册具名 provider；重名拒绝（fail-fast）。
    pub fn register(
        &self,
        name: impl Into<String>,
        provider: Arc<dyn ModelService>,
    ) -> Result<(), String> {
        let name = name.into();
        let mut providers = self.providers.write().expect("registry poisoned");
        if providers.contains_key(&name) {
            return Err(format!("provider 已存在：{name}"));
        }
        providers.insert(name, provider);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<Arc<dyn ModelService>> {
        self.providers
            .read()
            .expect("registry poisoned")
            .get(name)
            .cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.providers
            .read()
            .expect("registry poisoned")
            .keys()
            .cloned()
            .collect()
    }
}

/// 便捷函数：等价 `registry.register(name, provider)`。
pub fn register_provider(
    registry: &ProviderRegistry,
    name: &str,
    provider: Arc<dyn ModelService>,
) -> Result<(), String> {
    registry.register(name, provider)
}

// ---------- Mock（M2 默认服务） ----------

/// 固定文本模型桩：链路自检 / 测试；可脚本化 chunk 流模拟工具调用。
#[derive(Debug, Clone)]
pub struct MockModelService {
    chunks: Vec<Result<ModelChunk, ModelError>>,
}

impl MockModelService {
    pub fn new(reply: impl Into<String>) -> Self {
        Self {
            chunks: vec![
                Ok(ModelChunk::TextDelta(reply.into())),
                Ok(ModelChunk::ItemDone {
                    kind: ItemKind::Message,
                }),
                Ok(ModelChunk::Done),
            ],
        }
    }

    /// 脚本化响应：每次 stream 调用重放同一批 chunks。
    pub fn scripted(chunks: Vec<Result<ModelChunk, ModelError>>) -> Self {
        Self { chunks }
    }
}

#[async_trait]
impl ModelService for MockModelService {
    async fn stream(
        &self,
        _request: &ModelRequest,
        _signal: &AbortSignal,
    ) -> Result<ModelStream, ModelError> {
        Ok(Box::new(futures_util::stream::iter(self.chunks.clone())))
    }
}
