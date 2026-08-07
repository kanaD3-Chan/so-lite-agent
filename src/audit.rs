//! 审计（默认全覆盖）：通用记录集 + 可替换 sink。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::agent::dispatch::Caller;
use crate::agent::session::SessionKey;
use crate::message::MessageId;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
pub enum AuditRecord {
    EntryPointCall {
        entry: String,
        caller: Caller,
        ok: bool,
        error: Option<String>,
    },
    MessageCompleted {
        message_id: MessageId,
    },
    MessageEdited {
        message_id: MessageId,
        branch_id: MessageId,
    },
    BranchSwitched {
        message_id: MessageId,
    },
    SessionSwitched {
        from: SessionKey,
        to: SessionKey,
        reason: String,
    },
    LlmCall {
        provider: String,
        model: String,
        kind: String,
        tokens_in: Option<u64>,
        tokens_out: Option<u64>,
        duration_ms: u64,
        ok: bool,
    },
    Lifecycle {
        phase: String,
    },
    AccessDenied {
        entry: String,
        caller: Caller,
    },
    TurnEnded {
        stop_reason: String,
        tool_calls: usize,
    },
    Interrupt {
        name: String,
        reason: String,
    },
    Retry {
        entry: String,
        attempt: u32,
    },
    Compaction {
        session: String,
        summarized: usize,
    },
}

/// 审计落盘接口（M2 内存实现；文件 JSONL 由使用方提供）。
pub trait AuditSink: Send + Sync {
    fn append(&self, record: AuditRecord);
}

/// 审计器：内核组件统一入口。
#[derive(Clone)]
pub struct Auditor {
    sink: Arc<dyn AuditSink>,
}

impl Auditor {
    pub fn new(sink: Arc<dyn AuditSink>) -> Self {
        Self { sink }
    }

    pub fn record(&self, record: AuditRecord) {
        self.sink.append(record);
    }
}

/// M2 内存 sink：测试可断言。
#[derive(Default)]
pub struct MemoryAuditSink {
    records: std::sync::Mutex<Vec<AuditRecord>>,
}

impl MemoryAuditSink {
    pub fn take(&self) -> Vec<AuditRecord> {
        std::mem::take(&mut *self.records.lock().expect("audit poisoned"))
    }
}

impl AuditSink for MemoryAuditSink {
    fn append(&self, record: AuditRecord) {
        self.records.lock().expect("audit poisoned").push(record);
    }
}
