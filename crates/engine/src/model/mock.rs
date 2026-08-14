//! MockModelService：固定文本桩 / 脚本化 chunk 流（链路自检与测试）。

use async_trait::async_trait;

use super::contract::{ItemKind, ModelChunk, ModelError, ModelRequest, ModelService, ModelStream};
use super::handle::AbortSignal;

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
