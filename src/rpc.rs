//! 通用 RPC（M4）：RpcRequest/RpcFrame + 通用 Method 子集 + `custom` 兜底 + RpcExtension。
//!
//! 使用方的业务方法（settings/balance/cache/compute 等）经 [`RpcExtension`] 挂到 Kernel，
//! 不进入通用 Method 集合（ADR-0004 通用边界）。

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::session::SessionKey;
use crate::events::Event;
use crate::message::MessageId;

/// 中性附件引用（路径 + 名称）：不固化使用方的暂存/白名单语义。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcAttachment {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// 通用方法子集；业务方法走 [`Method::Custom`] + [`RpcExtension`]。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Method {
    SendUserMessage {
        /// None = 使用默认（新建）会话。
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_key: Option<SessionKey>,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<RpcAttachment>,
    },
    TriggerCommand {
        entry: String,
        #[serde(default)]
        params: Value,
    },
    EditMessage {
        session_key: SessionKey,
        message_id: MessageId,
        text: String,
    },
    SwitchBranch {
        session_key: SessionKey,
        message_id: MessageId,
    },
    Abort,
    GetState,
    ListSessions,
    ReadSession {
        session_key: SessionKey,
    },
    ListTools,
    Custom {
        method: String,
        #[serde(default)]
        params: Value,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcRequest {
    pub id: u64,
    pub method: Method,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    pub fn not_handled(method: &str) -> Self {
        Self::new("not_handled", format!("扩展未处理的方法：{method}"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcFrame {
    Response {
        id: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        result: Option<Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<RpcError>,
    },
    Event {
        event: Event,
    },
}

impl RpcFrame {
    pub fn ok(id: u64, result: Value) -> Self {
        Self::Response {
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: u64, error: RpcError) -> Self {
        Self::Response {
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// 业务方法扩展：挂到 Kernel 的 `custom` 兜底链。
/// 不认识的 method 应返回 [`RpcError::not_handled`]，kernel 会继续问下一个扩展。
#[async_trait]
pub trait RpcExtension: Send + Sync {
    async fn handle(&self, method: &str, params: Value) -> Result<Value, RpcError>;
}
