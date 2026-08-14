//! sl-agent 的 WS/RPC 桥与事件广播（二进制侧，ADR-0006）。
//!
//! 协议复用 crate 的通用帧（[`RpcRequest`] / [`RpcFrame`]，M4 定型）：
//! - 浏览器 → 服务端：`RpcRequest` JSON 文本帧；
//! - 服务端 → 浏览器：`RpcFrame::Response`（带 id 回执）与 `RpcFrame::Event`
//!   （kernel 事件流经 [`WsHub`] 广播，打字机增量/工具起止/回合结束都走它）。

use std::sync::Arc;

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use so_lite_agent::builder::Kernel;
use so_lite_agent::events::{Event, EventSink};
use so_lite_agent::rpc::{RpcFrame, RpcRequest};
use tokio_stream::wrappers::BroadcastStream;

/// 应用状态：kernel + 事件广播源。
pub struct AppState {
    pub kernel: Arc<Kernel>,
    pub hub: tokio::sync::broadcast::Sender<String>,
}

/// 事件广播桥：实现 `EventSink`，kernel 的每个事件 → JSON 帧 → 广播给所有 WS 连接。
pub struct WsHub {
    pub tx: tokio::sync::broadcast::Sender<String>,
}

impl EventSink for WsHub {
    fn emit(&self, event: Event) {
        if let Ok(frame) = serde_json::to_string(&RpcFrame::Event { event }) {
            let _ = self.tx.send(frame);
        }
    }
}

/// `GET /ws` 升级处理器：每个连接一个读循环 + 一个写任务。
///
/// 有序性：事件与 RPC 回执**都走广播通道**（单一有序路径）——回合内事件先入队、
/// 回执在 `handle_rpc` 返回后入队，保证"事件先到、回执后到"；回执带 id，多连接
/// 各取所需（P1 本地单用户，跨连接可见可接受）。
pub async fn ws_upgrade(ws: WebSocketUpgrade, State(state): State<Arc<AppState>>) -> Response {
    let resp = ws.on_upgrade(move |socket| handle(socket, state));
    // 前后端分离（ADR-0010）：前端独立部署（任意 Origin 的 WS 连接都接受）。
    // 浏览器 WS 不受同源策略限制，但显式回 ACAO 头更稳（部分环境/代理会拦）。
    let mut resp = resp;
    resp.headers_mut().insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        axum::http::HeaderValue::from_static("*"),
    );
    resp
}

async fn handle(socket: WebSocket, state: Arc<AppState>) {
    let (mut sender, mut receiver) = socket.split();
    let mut events = BroadcastStream::new(state.hub.subscribe());

    // 写任务：广播流（事件帧 + 回执帧）→ 推给浏览器。
    let write_task = tokio::spawn(async move {
        while let Some(Ok(json)) = events.next().await {
            if sender.send(Message::Text(json.into())).await.is_err() {
                break;
            }
        }
    });

    // 读循环：RpcRequest → kernel.handle_rpc → 回执帧入广播（保持与事件同序）。
    while let Some(Ok(msg)) = receiver.next().await {
        let Message::Text(text) = msg else { continue };
        match serde_json::from_str::<RpcRequest>(&text) {
            Ok(request) => {
                let frame = state.kernel.handle_rpc(request).await;
                if let Ok(json) = serde_json::to_string(&frame) {
                    let _ = state.hub.send(json);
                }
            }
            Err(e) => {
                let error = serde_json::json!({
                    "type": "response",
                    "error": { "code": "bad_request", "message": e.to_string() },
                });
                let _ = state.hub.send(error.to_string());
            }
        }
    }
    write_task.abort();
}
