//! 模型 Provider 层（pi-ai 等价物）：ModelService 抽象、流式事件归一化、
//! Provider 注册表、内置适配器与 Mock 桩。
//!
//! - `contract`：ModelService / ModelRequest / ModelChunk 等通用契约；
//! - `handle`：AbortSignal 与带超时/审计的 ModelHandle；
//! - `providers`：ProviderRegistry 与 `register_provider`；
//! - `mock`：MockModelService（链路自检/测试）；
//! - `openai` / `responses` / `completions` / `anthropic`：内置适配器。

mod anthropic;
mod completions;
mod contract;
mod handle;
mod mock;
mod openai;
mod providers;
mod responses;

pub use anthropic::AnthropicModelService;
pub use completions::ChatCompletionsModelService;
pub use contract::{
    ItemKind, ModelChunk, ModelError, ModelKind, ModelRequest, ModelResponse, ModelService,
    ModelStream, ResponseFormat, TokenUsage, ToolCallSpec, ToolChoice, ToolSchema,
};
pub use handle::{AbortSignal, ModelHandle};
pub use mock::MockModelService;
pub use openai::{OpenAiCompatibleConfig, OpenAiTransport, register_openai_compatible};
pub use providers::{ProviderRegistry, register_provider};
pub use responses::ResponsesModelService;
