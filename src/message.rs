//! 消息树：气泡 = 一个输出 item，完成即落盘。

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::contract::ToolError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct MessageId(pub Uuid);

impl MessageId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for MessageId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for MessageId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// 附件（图片 base64；上传链路使用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub mime: String,
    pub data_base64: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageKind {
    User {
        text: String,
        /// 前端展示文本（force_tool 场景：原始输入 / 工具标题+参数），重开会话后仍友好；
        /// 缺省时前端回退渲染 `text`。模型上下文始终使用 `text`（拼好的指令）。
        #[serde(default)]
        display_text: Option<String>,
        #[serde(default)]
        attachments: Vec<Attachment>,
    },
    Assistant {
        text: String,
    },
    ToolCall {
        entry: String,
        params: serde_json::Value,
        result: Result<serde_json::Value, ToolError>,
        /// 第一轮模型返回的真实 call_id（Responses API 要求按原值回传；
        /// 旧数据/手动构造缺省为空，回传时回退消息 id）。
        #[serde(default)]
        call_id: String,
    },
    /// 模型推理（思维链）：id 用于后续轮次回传，text 供前端展示。
    Reasoning {
        id: String,
        text: String,
    },
    System {
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: MessageId,
    pub parent_id: Option<MessageId>,
    pub kind: MessageKind,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl Message {
    /// 会话切换控制调用：只作调度动作，不落会话树、不随历史携带。
    pub fn is_switch_tool_call(&self) -> bool {
        matches!(
            &self.kind,
            MessageKind::ToolCall { entry, .. } if entry == "session::switch"
        )
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::user_with_display(text, None)
    }

    pub fn user_with_display(text: impl Into<String>, display_text: Option<String>) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            kind: MessageKind::User {
                text: text.into(),
                display_text,
                attachments: Vec::new(),
            },
            created_at: chrono::Utc::now(),
        }
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            kind: MessageKind::Assistant { text: text.into() },
            created_at: chrono::Utc::now(),
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            kind: MessageKind::System { text: text.into() },
            created_at: chrono::Utc::now(),
        }
    }

    pub fn tool_call(
        entry: impl Into<String>,
        params: serde_json::Value,
        result: Result<serde_json::Value, ToolError>,
    ) -> Self {
        Self::tool_call_with_id(entry, params, result, String::new())
    }

    /// 带真实 call_id 的工具调用消息（agent loop 使用）。
    pub fn tool_call_with_id(
        entry: impl Into<String>,
        params: serde_json::Value,
        result: Result<serde_json::Value, ToolError>,
        call_id: String,
    ) -> Self {
        Self {
            id: MessageId::new(),
            parent_id: None,
            kind: MessageKind::ToolCall {
                entry: entry.into(),
                params,
                result,
                call_id,
            },
            created_at: chrono::Utc::now(),
        }
    }
}

/// 把消息挂到链尾：parent = 最后一条消息 id。
pub fn append_to_path(messages: &mut Vec<Message>, mut msg: Message) {
    msg.parent_id = messages.last().map(|m| m.id);
    messages.push(msg);
}
