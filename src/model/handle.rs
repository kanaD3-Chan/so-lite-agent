//! AbortSignal（取消信号）与 ModelHandle（注入插件的受限模型句柄）。

use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::audit::{AuditRecord, Auditor};

use super::contract::{ModelError, ModelKind, ModelRequest, ModelResponse, ModelService};

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

    /// 内核装配用：取回底层服务（仅 KernelBuilder / 内核插件，TM-004——
    /// 不外露原始服务，防止下游绕过超时/审计包装）。
    pub(crate) fn inner(&self) -> Arc<dyn ModelService> {
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
