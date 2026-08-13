//! `sl-agent`：业务无关的通用 Agent 可执行文件（浏览器 Web 应用形态，ADR-0006）。
//!
//! HTTP/WS 服务 + 内嵌前端（`web/`，rust-embed），单二进制分发；模型默认 mock
//! （零配置 hello 回合），经环境变量接真实 OpenAI 兼容端点；Rune 脚本用户插件
//! 从 `--plugins` 目录加载（一插件一目录：manifest.json + plugin.rn）。
//!
//! 运行：`cargo run --bin sl-agent --features server,rune-plugins`
//!
//! 环境变量：`SL_AGENT_PORT`（默认 8080）、`SL_AGENT_PLUGINS_DIR`（默认 `./plugins`）、
//! `SL_AGENT_API_URL` / `SL_AGENT_API_KEY` / `SL_AGENT_MODEL`（可选，配了接真实模型）。

#![cfg(feature = "server")]

mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use rust_embed::RustEmbed;
use so_lite_agent::audit::{Auditor, MemoryAuditSink};
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::model::{
    ModelHandle, OpenAiCompatibleConfig, OpenAiTransport, ProviderRegistry,
    register_openai_compatible,
};
use so_lite_agent::services::ServiceHandles;

use self::ws::{AppState, WsHub};

/// 内嵌前端资源（`web/`，构建期打包进二进制）。
#[derive(RustEmbed)]
#[folder = "web/"]
struct WebAssets;

fn main() {
    // env 配置读取失败只告警不退出：mock 模型 + 默认端口也能跑 hello。
    match tokio::runtime::Runtime::new() {
        Ok(rt) => {
            if let Err(e) = rt.block_on(run()) {
                eprintln!("sl-agent 启动失败：{e}");
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("tokio runtime 构建失败：{e}");
            std::process::exit(1);
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("SL_AGENT_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080);
    let plugins_dir = std::env::var("SL_AGENT_PLUGINS_DIR").unwrap_or_else(|_| "./plugins".into());

    // ---- 装配 kernel（模型：env 有则真实，否则默认 mock）----
    let (hub_tx, hub_rx) = tokio::sync::broadcast::channel(256);
    let events = Arc::new(WsHub { tx: hub_tx.clone() });
    let mut builder = KernelBuilder::new().event_sink(events);

    if let (Ok(api_url), Ok(api_key), Ok(model)) = (
        std::env::var("SL_AGENT_API_URL"),
        std::env::var("SL_AGENT_API_KEY"),
        std::env::var("SL_AGENT_MODEL"),
    ) {
        log::info!("使用真实模型端点：{api_url}（{model}）");
        let registry = ProviderRegistry::new();
        let service = register_openai_compatible(
            &registry,
            "sl",
            OpenAiCompatibleConfig {
                api_url,
                api_key,
                model,
                transport: OpenAiTransport::Responses,
                ..Default::default()
            },
        )?;
        let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
        builder = builder.service_handles(ServiceHandles::default().with_model(ModelHandle::new(
            service,
            Duration::from_secs(30),
            auditor,
        )));
    }

    // ---- Rune 脚本用户插件目录（一插件一目录，失败只告警单个插件）----
    #[cfg(feature = "rune-plugins")]
    {
        let dir = PathBuf::from(&plugins_dir);
        if dir.is_dir() {
            for entry in std::fs::read_dir(&dir)? {
                let path = entry?.path();
                if !path.is_dir() {
                    continue;
                }
                match so_lite_agent::rune::ScriptPlugin::from_dir(&path) {
                    Ok(plugin) => {
                        log::info!("加载脚本插件：{}", plugin.manifest.namespace);
                        builder = builder.script_plugin(plugin);
                    }
                    Err(e) => log::warn!("跳过插件目录 {}：{e}", path.display()),
                }
            }
        } else {
            log::info!("插件目录不存在，跳过：{plugins_dir}");
        }
    }

    let kernel = Arc::new(builder.build()?);
    let state = Arc::new(AppState {
        kernel,
        hub: hub_tx,
    });
    let _ = hub_rx;

    // ---- HTTP 路由：静态（内嵌前端）+ /ws + /healthz ----
    let app = Router::new()
        .route("/ws", get(ws::ws_upgrade))
        .route("/healthz", get(healthz))
        .fallback(static_handler)
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::info!("sl-agent 已启动：http://{addr}（打开浏览器即可对话）");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true }))
}

/// 静态资源：内嵌前端（rust-embed），按扩展名给 MIME。
async fn static_handler(State(_state): State<Arc<AppState>>, uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match WebAssets::get(path) {
        Some(content) => {
            let mime = mime_for(path);
            ([(header::CONTENT_TYPE, mime)], content.data).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

fn mime_for(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "json" => "application/json",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use serde_json::json;
    use so_lite_agent::events::Event;
    use so_lite_agent::rpc::RpcFrame;

    /// 起一个真实服务（随机端口），返回 base URL。
    async fn serve() -> String {
        let (hub_tx, _hub_rx) = tokio::sync::broadcast::channel(256);
        let events = Arc::new(WsHub { tx: hub_tx.clone() });
        let kernel = Arc::new(
            KernelBuilder::new()
                .event_sink(events)
                .build()
                .expect("默认装配应成功"),
        );
        let state = Arc::new(AppState {
            kernel,
            hub: hub_tx,
        });
        let app = Router::new()
            .route("/ws", get(ws::ws_upgrade))
            .route("/healthz", get(healthz))
            .fallback(static_handler)
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn healthz_ok() {
        let base = serve().await;
        let body = reqwest::get(format!("{base}/healthz"))
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(body.contains("\"ok\":true"), "{body}");
    }

    #[tokio::test]
    async fn index_served() {
        let base = serve().await;
        let resp = reqwest::get(&base).await.unwrap();
        assert!(resp.status().is_success());
        let body = resp.text().await.unwrap();
        assert!(body.contains("sl-agent"), "首页应包含 sl-agent：{body}");
    }

    #[tokio::test]
    async fn ws_hello_round_gets_response_and_events() {
        let base = serve().await;
        let url = base.replace("http", "ws") + "/ws";
        let (mut socket, _) = tokio_tungstenite::connect_async(&url).await.unwrap();

        // 发 send_user_message（默认 mock 模型）。
        let request = json!({
            "id": 1,
            "method": { "type": "send_user_message", "text": "你好" }
        });
        socket
            .send(tokio_tungstenite::tungstenite::Message::Text(
                request.to_string().into(),
            ))
            .await
            .unwrap();

        // 事件帧与 Response 帧可能乱序（写任务 select 汇流）——收齐两者再断言。
        let mut got_event = false;
        let mut got_response = false;
        for _ in 0..40 {
            if got_event && got_response {
                break;
            }
            let msg = socket.next().await.unwrap().unwrap();
            let tokio_tungstenite::tungstenite::Message::Text(text) = msg else {
                continue;
            };
            let frame: RpcFrame = serde_json::from_str(&text).unwrap();
            match frame {
                RpcFrame::Event { event } => {
                    if matches!(event, Event::MessageDelta { .. } | Event::TurnEnd { .. }) {
                        got_event = true;
                    }
                }
                RpcFrame::Response { id, result, error } => {
                    assert_eq!(id, 1, "回执应带原 id");
                    assert!(error.is_none(), "默认 mock 回合不应报错：{error:?}");
                    assert!(result.is_some());
                    got_response = true;
                }
            }
        }
        assert!(got_event, "应至少收到一个事件帧");
        assert!(got_response, "应收到 Response 回执");
    }
}
