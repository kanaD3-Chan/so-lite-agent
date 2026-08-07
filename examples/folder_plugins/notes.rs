//! 业务服务（共享模块）：trait + 具体实现 + 服务标识。

use std::sync::Mutex;

use async_trait::async_trait;
use so_lite_agent::services::ServiceId;

#[async_trait]
pub trait NoteService: Send + Sync {
    async fn count(&self) -> Result<usize, String>;
}

#[derive(Default)]
pub struct MemoryNoteService {
    notes: Mutex<Vec<String>>,
}

#[async_trait]
impl NoteService for MemoryNoteService {
    async fn count(&self) -> Result<usize, String> {
        Ok(self.notes.lock().expect("notes poisoned").len())
    }
}

pub fn notes_service_id() -> ServiceId {
    ServiceId::custom("notes")
}
