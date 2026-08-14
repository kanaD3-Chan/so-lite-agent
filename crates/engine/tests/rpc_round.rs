//! M4 验收：通用 RPC 子集 + custom 兜底 + RpcExtension。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::message::MessageKind;
use so_lite_agent::rpc::{Method, RpcError, RpcExtension, RpcFrame, RpcRequest};

struct PingExtension;

#[async_trait]
impl RpcExtension for PingExtension {
    async fn handle(&self, method: &str, _params: Value) -> Result<Value, RpcError> {
        match method {
            "ping" => Ok(json!({"pong": true})),
            _ => Err(RpcError::not_handled(method)),
        }
    }
}

#[tokio::test]
async fn rpc_method_subset_round_trip() {
    let kernel = KernelBuilder::new()
        .rpc_extension(Arc::new(PingExtension))
        .build()
        .unwrap();

    // ListTools
    let frame = kernel
        .handle_rpc(RpcRequest {
            id: 1,
            method: Method::ListTools,
        })
        .await;
    assert!(matches!(
        frame,
        RpcFrame::Response {
            result: Some(_),
            ..
        }
    ));

    // GetState
    let frame = kernel
        .handle_rpc(RpcRequest {
            id: 2,
            method: Method::GetState,
        })
        .await;
    let RpcFrame::Response { result, .. } = frame else {
        panic!("应返回响应帧");
    };
    assert_eq!(result.unwrap()["running"], false);

    // SendUserMessage（默认会话）
    let frame = kernel
        .handle_rpc(RpcRequest {
            id: 3,
            method: Method::SendUserMessage {
                session_key: None,
                text: "你好".into(),
                attachments: Vec::new(),
                force_tool: None,
            },
        })
        .await;
    let RpcFrame::Response { result, .. } = frame else {
        panic!("应返回响应帧");
    };
    let result = result.unwrap();
    assert_eq!(result["stop_reason"], "natural");
    assert_eq!(result["messages"].as_array().unwrap().len(), 1);

    // ListSessions / ReadSession
    let frame = kernel
        .handle_rpc(RpcRequest {
            id: 4,
            method: Method::ListSessions,
        })
        .await;
    let RpcFrame::Response { result, .. } = frame else {
        panic!("应返回响应帧");
    };
    let sessions = result.unwrap();
    assert_eq!(sessions.as_array().unwrap().len(), 1);

    // custom 兜底
    let frame = kernel
        .handle_rpc(RpcRequest {
            id: 5,
            method: Method::Custom {
                method: "ping".into(),
                params: Value::Null,
            },
        })
        .await;
    let RpcFrame::Response { result, .. } = frame else {
        panic!("应返回响应帧");
    };
    assert_eq!(result.unwrap()["pong"], true);

    // 未处理的方法 → not_handled
    let frame = kernel
        .handle_rpc(RpcRequest {
            id: 6,
            method: Method::Custom {
                method: "nope".into(),
                params: Value::Null,
            },
        })
        .await;
    let RpcFrame::Response { error, .. } = frame else {
        panic!("应返回响应帧");
    };
    assert_eq!(error.unwrap().code, "not_handled");

    // Abort（无活动回合，静默成功）
    let frame = kernel
        .handle_rpc(RpcRequest {
            id: 7,
            method: Method::Abort,
        })
        .await;
    assert!(matches!(frame, RpcFrame::Response { error: None, .. }));
}

#[tokio::test]
async fn edit_message_derives_branch() {
    let kernel = KernelBuilder::new().build().unwrap();
    let key = Default::default();
    kernel.send_user_message(key, "你好").await.unwrap();

    let messages = kernel.read_session(&key).await.unwrap();
    let user = messages
        .iter()
        .find(|m| matches!(m.kind, MessageKind::User { .. }))
        .expect("应有 user 消息");
    let assistant = messages
        .iter()
        .find(|m| matches!(m.kind, MessageKind::Assistant { .. }))
        .expect("应有 assistant 消息");

    // 编辑 user（"改完重发"）：遮蔽到链尾，新链 = [改后的问题]。
    let new_path = kernel
        .edit_message(key, user.id, "改后的问题")
        .await
        .unwrap();
    assert_eq!(new_path.len(), 1); // user 编辑遮蔽到链尾
    assert!(matches!(
        &new_path[0].kind,
        MessageKind::User { text, .. } if text == "改后的问题"
    ));

    // 重新生成已禁用：编辑 assistant 被拒绝。
    assert!(
        kernel
            .edit_message(key, assistant.id, "改写")
            .await
            .is_err()
    );

    let all = kernel.read_session(&key).await.unwrap();
    // read_session = 全量消息树（mistake-agent 同款）：旧分支（原 user + assistant）
    // 与被遮蔽历史仍在（< / > 可切回），活跃链 = 改后的问题。
    assert_eq!(all.len(), 3, "全量树：原 user + assistant + 编辑后的 user");
    assert!(
        all.iter()
            .any(|m| matches!(&m.kind, MessageKind::User { text, .. } if text == "改后的问题")),
        "编辑后的 user 在全量树中"
    );
}
