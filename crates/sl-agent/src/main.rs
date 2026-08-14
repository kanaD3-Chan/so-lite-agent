//! `sl-agent`：业务无关的通用 Agent 可执行文件（API 服务形态，ADR-0006/0010）。
//!
//! **前后端分离（ADR-0010）**：sl-agent 只提供 HTTP/WS API（无静态服务、无内嵌前端）；
//! 前端是独立 React 工程（`frontend/`），自己起 dev server 或静态托管，经 WS 连本服务。
//! 模型默认 mock（零配置 hello 回合），经环境变量接真实 OpenAI 兼容端点；
//! Rune 脚本用户插件从 `--plugins` 目录加载（一插件一目录：manifest.json + plugin.rn）。
//! 内置内核插件（crates/plugin-*，ADR-0008）由 build.rs 自动发现并逐条注册。
//!
//! 运行：`cargo run -p sl-agent`
//!
//! 环境变量：`SL_AGENT_PORT`（默认 8080）、`SL_AGENT_PLUGINS_DIR`（默认 `./plugins`）、
//! `SL_AGENT_API_URL` / `SL_AGENT_API_KEY` / `SL_AGENT_MODEL`（可选，配了接真实模型）。
//! 前端连 `ws://127.0.0.1:8080/ws`（Vite dev 见 `frontend/README`）。

mod builtin;
mod ws;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::routing::get;
use so_lite_agent::audit::{Auditor, MemoryAuditSink};
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::model::{
    ModelHandle, OpenAiCompatibleConfig, OpenAiTransport, ProviderRegistry,
    register_openai_compatible,
};
use so_lite_agent::services::ServiceHandles;

use self::ws::{AppState, WsHub};

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
    let data_dir = std::env::var("SL_AGENT_DATA_DIR").unwrap_or_else(|_| "./data".into());

    // ---- 会话持久化：JSONL 事件日志落盘（ADR-0007 第二步，sl-agent 默认启用）----
    let session_store = Arc::new(so_lite_agent::services::JsonlSessionStore::open(
        std::path::Path::new(&data_dir),
    )?);

    // ---- 装配 kernel（模型：env 有则真实，否则默认 mock）----
    let (hub_tx, hub_rx) = tokio::sync::broadcast::channel(256);
    let events = Arc::new(WsHub { tx: hub_tx.clone() });
    let mut builder = KernelBuilder::new().event_sink(events);
    // 会话存储注入（JSONL 落盘；类型擦除为 SessionStore 句柄）。
    let session_handle: std::sync::Arc<dyn so_lite_agent::services::SessionStore> = session_store;
    builder =
        builder.service_handles(ServiceHandles::default().with_session(session_handle.clone()));
    // 内核插件注册（Linus 模式，ADR-0036 构建期自动发现 crates/plugin-*）。
    for desc in builtin::builtin_kernel_plugins() {
        builder = builder.register_kernel_plugin(desc);
    }

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
        // 保留已注入的 session 存储，只补 model。
        let mut handles = ServiceHandles::default().with_session(session_handle.clone());
        handles = handles.with_model(ModelHandle::new(service, Duration::from_secs(30), auditor));
        builder = builder.service_handles(handles);
    }

    // ---- Rune 脚本用户插件目录（一插件一目录，失败只告警单个插件）----
    {
        let plugins_dir =
            std::env::var("SL_AGENT_PLUGINS_DIR").unwrap_or_else(|_| "./plugins".into());
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

    // ---- HTTP 路由：仅 API（/ws + /healthz）；前端独立部署（ADR-0010）----
    let app = Router::new()
        .route("/ws", get(ws::ws_upgrade))
        .route("/healthz", get(healthz))
        .with_state(state);

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    log::info!("sl-agent API 已启动：http://{addr}（WS: /ws；前端独立运行，见 frontend/README）");
    axum::serve(listener, app).await?;
    Ok(())
}

async fn healthz() -> axum::Json<serde_json::Value> {
    axum::Json(serde_json::json!({ "ok": true }))
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
    async fn root_is_api_only() {
        // 前后端分离（ADR-0010）：sl-agent 不提供静态页，根路径应 404。
        let base = serve().await;
        let resp = reqwest::get(&base).await.unwrap();
        assert!(
            resp.status().is_client_error() || resp.status().is_server_error(),
            "API 服务不应提供静态页：{}",
            resp.status()
        );
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
