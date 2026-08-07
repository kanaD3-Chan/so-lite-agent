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
