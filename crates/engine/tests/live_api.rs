//! M3 真实 API 验收（需环境变量，默认忽略）：
//!
//! ```text
//! SO_LITE_DEEPSEEK_URL=https://api.deepseek.com
//! SO_LITE_DEEPSEEK_KEY=sk-...
//! SO_LITE_DEEPSEEK_MODEL=deepseek-v4-flash
//! cargo test --test live_api -- --ignored --nocapture
//! ```

use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::events::MemoryEventSink;
use so_lite_agent::model::{
    AbortSignal, ModelKind, ModelRequest, OpenAiCompatibleConfig, OpenAiTransport,
    ProviderRegistry, register_openai_compatible,
};
use so_lite_agent::services::ServiceHandles;
use std::sync::Arc;

fn deepseek_config() -> Option<OpenAiCompatibleConfig> {
    let api_url = std::env::var("SO_LITE_DEEPSEEK_URL").ok()?;
    let api_key = std::env::var("SO_LITE_DEEPSEEK_KEY").ok()?;
    let model =
        std::env::var("SO_LITE_DEEPSEEK_MODEL").unwrap_or_else(|_| "deepseek-v4-flash".into());
    Some(OpenAiCompatibleConfig {
        api_url,
        api_key,
        model,
        transport: OpenAiTransport::Responses,
        ..Default::default()
    })
}

#[tokio::test]
#[ignore]
async fn deepseek_responses_round_trip() {
    let config = deepseek_config().expect("设置 SO_LITE_DEEPSEEK_URL/KEY");
    let registry = ProviderRegistry::new();
    let service = register_openai_compatible(&registry, "deepseek", config).expect("注册 provider");

    let request = ModelRequest::chat(
        ModelKind::Main,
        vec![so_lite_agent::message::Message::user("只回复两个字：你好")],
    );
    let response = service
        .complete(&request, &AbortSignal::new())
        .await
        .expect("真实 API 回合");
    assert!(!response.text.is_empty());
    assert!(response.usage.is_some());
    println!("reply={} usage={:?}", response.text, response.usage);
}

#[tokio::test]
#[ignore]
async fn deepseek_round_through_kernel() {
    let config = deepseek_config().expect("设置 SO_LITE_DEEPSEEK_URL/KEY");
    let registry = ProviderRegistry::new();
    let service = register_openai_compatible(&registry, "deepseek", config).expect("注册 provider");
    let kernel = KernelBuilder::new()
        .event_sink(Arc::new(MemoryEventSink::default()))
        .service_handles(ServiceHandles::default().with_model(
            so_lite_agent::model::ModelHandle::new(
                service,
                std::time::Duration::from_secs(180),
                so_lite_agent::audit::Auditor::new(Arc::new(
                    so_lite_agent::audit::MemoryAuditSink::default(),
                )),
            ),
        ))
        .system_prompt(|| "你是 so-lite-agent。".to_string())
        .build()
        .unwrap();
    let outcome = kernel
        .send_user_message(Default::default(), "你好")
        .await
        .unwrap();
    assert!(!outcome.messages.is_empty());
    println!("stop={:?} usage={:?}", outcome.stop_reason, outcome.usage);
}
