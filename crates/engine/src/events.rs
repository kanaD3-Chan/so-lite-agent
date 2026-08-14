//! kernel → 使用方的事件流（通用子集）。
//!
//! 业务事件（记忆变更、验算请求、余额/缓存统计等）不属于通用运行时，
//! 由使用方在自己的协议层扩展（见 ADR-0004）。

use serde::{Deserialize, Serialize};

use crate::agent::r#loop::StopReason;
use crate::agent::session::SessionKey;
use crate::message::MessageId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    MessageDelta {
        message_id: MessageId,
        delta: String,
    },
    ReasoningDelta {
        delta: String,
    },
    ToolStart {
        entry: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    ToolEnd {
        entry: String,
        ok: bool,
    },
    ToolProgress {
        entry: String,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        icon: Option<String>,
    },
    TurnEnd {
        stop_reason: StopReason,
    },
    SessionSwitched {
        from: SessionKey,
        to: SessionKey,
    },
    Compaction {
        session: SessionKey,
    },
    Error {
        message: String,
    },
    /// 业务事件扩展口：内核插件/使用方上浮自定义事件（与 [`crate::rpc::Method::Custom`] 对称）。
    /// kernel 不解析 name/payload，只负责运输；前端按 name 匹配处理。
    Custom {
        name: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
}

/// 事件消费者：非 async、fire-and-forget。
pub trait EventSink: Send + Sync {
    fn emit(&self, event: Event);
}

/// 测试/控制台 sink：内存收集。
#[derive(Default)]
pub struct MemoryEventSink {
    events: std::sync::Mutex<Vec<Event>>,
}

impl MemoryEventSink {
    pub fn take(&self) -> Vec<Event> {
        std::mem::take(&mut *self.events.lock().expect("sink poisoned"))
    }
}

impl EventSink for MemoryEventSink {
    fn emit(&self, event: Event) {
        self.events.lock().expect("sink poisoned").push(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_event_round_trip() {
        let event = Event::Custom {
            name: "iot.alert".into(),
            payload: serde_json::json!({"device": "env_sensor", "value": 33.0}),
        };
        let json = serde_json::to_string(&event).unwrap();
        let back: Event = serde_json::from_str(&json).unwrap();
        match back {
            Event::Custom { name, payload } => {
                assert_eq!(name, "iot.alert");
                assert_eq!(payload["value"], 33.0);
            }
            other => panic!("应反序列化为 Custom：{other:?}"),
        }
    }
}
