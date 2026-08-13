//! 会话事实日志（ADR-0007）：append-only 事件 + 遮蔽投影（参考 DeepSeek Harness
//! `SessionEventMap` / `SurfaceOp` / 持久化契约）。
//!
//! 会话真相 = 每会话一条 append-only 不可变事件日志（lossless JSON、seq 连续、
//! 落盘后不可修改）；消息历史由投影得到，可回放/恢复/审计。编辑、重新生成、
//! 压缩统一为「追加新事件 + replace 遮蔽旧事件 + 投影」，用户可见的编辑/分支
//! UX 全部保留。
//!
//! 事件词汇表（P2 子集，对齐本 crate 现有语义；P3 再扩展 turn/step、raw chunk、
//! tool 生命周期、compaction 锁）：
//!
//! | 事件（由 `message.kind` 判别） | surface_op | 说明 |
//! |---|---|---|
//! | `User`（user/message） | append | 用户消息 |
//! | `Assistant`（assistant/message） | append / replace | 助手消息；重新生成/编辑 = 新事件 replace 遮蔽旧 assistant |
//! | `Reasoning`（assistant/reasoning） | append | 推理消息 |
//! | `ToolCall`（tool/result） | append | 工具调用（P2 合并 call/result） |
//! | `System`（compaction/summary） | replace | 压缩摘要 + replace 遮蔽被压段 |
//!
//! 投影 = 活跃链回溯：每个 surface 事件进入链时记录前驱（prev），replace 遮蔽
//! 区间并把区间后首节点重定向到新节点；从任意末端沿 prev 回溯即得候选链
//! （多条 replace 遮蔽同一段 → 多条候选链，`active_path` 选一条末端）。

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::session::{Goal, SessionKey, SessionMeta, SessionStatus};
use crate::message::{Message, MessageId, MessageKind};

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("会话不存在：{0}")]
    SessionNotFound(SessionKey),
    #[error("已存在：{0}")]
    AlreadyExists(String),
    #[error("数据损坏：{0}")]
    Corrupt(String),
    #[error("IO 错误：{0}")]
    Io(String),
    #[error("事件非法：{0}")]
    InvalidEvent(String),
    #[error("内部错误：{0}")]
    Internal(String),
}

/// SurfaceOp（参考 DSH）：事件进入有序 surface 的方式。
/// - `Append`：追加到链尾（user/assistant/reasoning/tool 的常规路径）；
/// - `Replace { start, end }`：遮蔽链上从 `start`（含）到 `end`（含）的区间，
///   以本事件节点顶替（编辑/重新生成/压缩统一走 replace）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceOp {
    Append,
    Replace { start: u64, end: u64 },
}

/// 一条会话事实日志事件：不可变、seq 连续、lossless JSON。
///
/// `message.kind` 即事件判别（User → user/message 等，见模块文档词汇表）；
/// `surface_op` 决定投影如何进入链；`source_event_seqs` 记录被遮蔽的旧 seq 全集
/// （replace 必填，append 通常为空）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEvent {
    pub seq: u64,
    pub message: Message,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface_op: Option<SurfaceOp>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_event_seqs: Vec<u64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl SessionEvent {
    /// 构造待追加事件（seq/created_at 由 store 分配）。
    pub fn new(message: Message, surface_op: SurfaceOp) -> Self {
        Self {
            seq: 0,
            message,
            surface_op: Some(surface_op),
            source_event_seqs: Vec::new(),
            created_at: chrono::Utc::now(),
        }
    }

    /// 事件类型名（词汇表判别；P3 扩展时此处同步）。
    pub fn kind_name(&self) -> &'static str {
        match &self.message.kind {
            MessageKind::User { .. } => "user/message",
            MessageKind::Assistant { .. } => "assistant/message",
            MessageKind::Reasoning { .. } => "assistant/reasoning",
            MessageKind::ToolCall { .. } => "tool/result",
            MessageKind::System { .. } => "compaction/summary",
        }
    }
}

/// Capability seam（ADR-0006）：session 能力的 Service Definition（会话事实日志契约）。
/// 会话持久化：kernel 内部（Session scheduler / loop / 压缩）使用。
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), SessionError>;
    async fn get_session(&self, key: &SessionKey) -> Result<Option<SessionMeta>, SessionError>;
    /// 追加一条事件：seq 自动分配（= 当前日志长度，连续）；校验 data 可 JSON
    /// 序列化、surface 元数据合法（坏事件 fail-fast，参考 DSH `Session.append`）。
    /// 返回落库后的完整事件（含 seq / created_at）。
    async fn append_event(
        &self,
        key: &SessionKey,
        event: SessionEvent,
    ) -> Result<SessionEvent, SessionError>;
    /// 全量事件日志（append 顺序；含被遮蔽事件——人读 transcript / 审计 / 回放）。
    async fn read_events(&self, key: &SessionKey) -> Result<Vec<SessionEvent>, SessionError>;
    /// 活跃链投影：从活跃末端（`active_path`，缺省 = 最新 surface 事件）沿遮蔽链
    /// 回溯到根，返回模型可见消息（parent_id 按链内相邻重算）。
    async fn read_path(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError>;
    /// 从任意末端投影（switch_branch / 编辑后取新链）。
    async fn read_path_from(
        &self,
        key: &SessionKey,
        end_seq: u64,
    ) -> Result<Vec<Message>, SessionError>;
    /// 设置活跃末端（消息树分支切换；None = 退化为最新 surface 事件）。
    async fn set_active_path(
        &self,
        key: &SessionKey,
        message_id: Option<MessageId>,
    ) -> Result<(), SessionError>;
    /// 全量日志 → 消息（append 顺序，含被遮蔽；人读 transcript 用）。
    async fn read_all(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError>;
    /// 消息 id → 事件 seq（编辑 / 切换分支前定位）。
    async fn resolve_seq(
        &self,
        key: &SessionKey,
        message_id: MessageId,
    ) -> Result<u64, SessionError>;
    async fn set_goal(&self, key: &SessionKey, goal: &Goal) -> Result<(), SessionError>;
    async fn archive(&self, key: &SessionKey) -> Result<(), SessionError>;
    async fn list_sessions(&self) -> Result<Vec<SessionMeta>, SessionError>;
    async fn set_last_activity(
        &self,
        key: &SessionKey,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), SessionError>;
}

/// 会话服务句柄：内核插件 / kernel 持有的完整视图。
pub type SessionHandle = Arc<dyn SessionStore>;

// ---------------------------------------------------------------------------
// 投影：surface 折叠 + 遮蔽链回溯（参考 DSH `foldSurface` / `SurfaceManager`）
// ---------------------------------------------------------------------------

/// 折叠结果：每个 surface 事件的链前驱 + 当前活跃链 seq 列表。
#[derive(Debug, Clone)]
pub struct SurfaceFold {
    /// 每个 surface 事件的链前驱（根节点为 None）。
    pub prev_of: HashMap<u64, Option<u64>>,
    /// 当前活跃链 seq 列表（append 顺序；replace 已顶替遮蔽区间）。
    pub chain: Vec<u64>,
}

/// 校验一条事件的 surface 元数据（不落库，纯检查）。
/// - 消息事件必须带 surface_op；
/// - replace 的 start/end 必须是当前 surface 节点；
/// - source_event_seqs 必须覆盖全部被遮蔽节点（参考 DSH `assertProvenance`）；
/// - source 必须引用更早事件。
fn validate_surface(event: &SessionEvent, fold: &SurfaceFold) -> Result<(), SessionError> {
    let Some(op) = event.surface_op else {
        return Err(SessionError::InvalidEvent(format!(
            "事件 {}（{}）缺 surface_op",
            event.seq,
            event.kind_name()
        )));
    };
    let SurfaceOp::Replace { start, end } = op else {
        return Ok(());
    };
    let start_idx = fold.chain.iter().position(|s| *s == start).ok_or_else(|| {
        SessionError::InvalidEvent(format!(
            "replace start {start} 不在当前 surface（seq {}）",
            event.seq
        ))
    })?;
    let end_idx = fold.chain.iter().position(|s| *s == end).ok_or_else(|| {
        SessionError::InvalidEvent(format!(
            "replace end {end} 不在当前 surface（seq {}）",
            event.seq
        ))
    })?;
    if start_idx > end_idx {
        return Err(SessionError::InvalidEvent(format!(
            "replace start {start} 在 end {end} 之后（seq {}）",
            event.seq
        )));
    }
    let shadowed: Vec<u64> = fold.chain[start_idx..=end_idx].to_vec();
    for s in &shadowed {
        if !event.source_event_seqs.contains(s) {
            return Err(SessionError::InvalidEvent(format!(
                "replace（seq {}）source_event_seqs 必须包含被遮蔽节点 {s}",
                event.seq
            )));
        }
    }
    for s in &event.source_event_seqs {
        if *s >= event.seq {
            return Err(SessionError::InvalidEvent(format!(
                "source_event_seqs 必须引用更早事件：{s} >= {}",
                event.seq
            )));
        }
    }
    Ok(())
}

/// 将一条事件应用到折叠状态（变更 `prev_of` 与 `chain`）。
/// replace 会把遮蔽区间后首个节点的 prev 重定向到新节点（保持保留段接续）。
pub(crate) fn apply_event(
    fold: &mut SurfaceFold,
    event: &SessionEvent,
) -> Result<(), SessionError> {
    validate_surface(event, fold)?;
    let Some(op) = event.surface_op else {
        return Ok(());
    };
    match op {
        SurfaceOp::Append => {
            fold.prev_of.insert(event.seq, fold.chain.last().copied());
            fold.chain.push(event.seq);
        }
        SurfaceOp::Replace { start, end } => {
            let start_idx = fold
                .chain
                .iter()
                .position(|s| *s == start)
                .expect("validate_surface 已校验 start 存在");
            let end_idx = fold
                .chain
                .iter()
                .position(|s| *s == end)
                .expect("validate_surface 已校验 end 存在");
            let prev = if start_idx > 0 {
                fold.chain.get(start_idx - 1).copied()
            } else {
                None
            };
            fold.prev_of.insert(event.seq, prev);
            // 遮蔽区间后首个节点重定向到本事件（保留段接续到摘要/新消息）。
            if end_idx + 1 < fold.chain.len() {
                let next = fold.chain[end_idx + 1];
                fold.prev_of.insert(next, Some(event.seq));
            }
            fold.chain.splice(start_idx..=end_idx, [event.seq]);
        }
    }
    Ok(())
}

/// 全量折叠事件日志 → surface 状态（纯函数，参考 DSH `foldSurface`）。
/// 任何坏事件（seq 不连续 / surface 非法）都会报错（fail-fast，坏日志拒绝重建）。
pub fn fold_surface(events: &[SessionEvent]) -> Result<SurfaceFold, SessionError> {
    let mut fold = SurfaceFold {
        prev_of: HashMap::new(),
        chain: Vec::new(),
    };
    for (index, event) in events.iter().enumerate() {
        if event.seq != index as u64 {
            return Err(SessionError::Corrupt(format!(
                "事件 seq {} 不连续；期望 {}",
                event.seq, index
            )));
        }
        apply_event(&mut fold, event)?;
    }
    Ok(fold)
}

/// 从任意末端沿遮蔽链回溯到根（分支切换的候选链）。
pub fn chain_from(fold: &SurfaceFold, end_seq: u64) -> Vec<u64> {
    let mut chain = Vec::new();
    let mut cur = Some(end_seq);
    let mut guard = 0usize;
    while let Some(seq) = cur {
        if guard > fold.prev_of.len() {
            break; // 防御：环保护（正常日志不可能成环）
        }
        guard += 1;
        chain.push(seq);
        cur = fold.prev_of.get(&seq).copied().flatten();
    }
    chain.reverse();
    chain
}

/// 将 seq 列表投影为消息：parent_id 按链内相邻重算（树形语义观感兼容）。
pub fn project_messages(
    events: &[SessionEvent],
    seqs: &[u64],
) -> Result<Vec<Message>, SessionError> {
    let by_seq: HashMap<u64, &SessionEvent> = events.iter().map(|e| (e.seq, e)).collect();
    let mut out: Vec<Message> = Vec::with_capacity(seqs.len());
    for (i, seq) in seqs.iter().enumerate() {
        let event = by_seq
            .get(seq)
            .ok_or_else(|| SessionError::Internal(format!("投影引用未知 seq {seq}")))?;
        let mut msg = event.message.clone();
        msg.parent_id = if i > 0 { Some(out[i - 1].id) } else { None };
        out.push(msg);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// InMemory 默认实现：事件日志 + 投影（重写自 M2 消息树版）
// ---------------------------------------------------------------------------

#[derive(Debug, Default)]
struct Inner {
    sessions: HashMap<SessionKey, SessionMeta>,
    events: HashMap<SessionKey, Vec<SessionEvent>>,
}

/// M2 默认会话存储：全内存事件日志，重启即失。文件实现由使用方提供
/// （crate 内置 JSONL 实现见 `crate::plugin::storage`）。
#[derive(Clone)]
pub struct InMemorySessionStore {
    inner: Arc<Mutex<Inner>>,
}

impl Default for InMemorySessionStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemorySessionStore {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner::default())),
        }
    }
}

impl InMemorySessionStore {
    /// 追加事件：seq 由 store 分配（= 当前日志长度，连续）；lossless JSON 校验 +
    /// surface 校验（fail-fast，参考 DSH `Session.append`）。
    fn append_event_inner(
        &self,
        key: &SessionKey,
        event: SessionEvent,
    ) -> Result<SessionEvent, SessionError> {
        let mut inner = self.inner.lock().expect("store poisoned");
        let log = inner
            .events
            .get_mut(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        let seq = log.len() as u64;
        // lossless JSON 校验：坏事件不进日志（ADR-0007 持久化契约）。
        serde_json::to_value(&event)
            .map_err(|e| SessionError::InvalidEvent(format!("事件不可 JSON 序列化：{e}")))?;
        // surface 校验：在当前日志折叠状态下应用（参考 DSH append 时 validateNext）。
        let mut probe = event.clone();
        probe.seq = seq;
        probe.created_at = chrono::Utc::now();
        let mut fold = fold_surface(log)?;
        apply_event(&mut fold, &probe)?;
        let stored = SessionEvent {
            seq,
            created_at: chrono::Utc::now(),
            ..event
        };
        log.push(stored.clone());
        Ok(stored)
    }

    fn active_end_seq(&self, key: &SessionKey) -> Result<Option<u64>, SessionError> {
        let inner = self.inner.lock().expect("store poisoned");
        let meta = inner
            .sessions
            .get(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        let events = inner.events.get(key).cloned().unwrap_or_default();
        let fold = fold_surface(&events)?;
        match meta.active_path {
            Some(message_id) => {
                let by_id: HashMap<MessageId, u64> =
                    events.iter().map(|e| (e.message.id, e.seq)).collect();
                match by_id.get(&message_id) {
                    Some(seq) => Ok(Some(*seq)),
                    None => Ok(fold.chain.last().copied()),
                }
            }
            None => Ok(fold.chain.last().copied()),
        }
    }
}

#[async_trait]
impl SessionStore for InMemorySessionStore {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), SessionError> {
        let mut inner = self.inner.lock().expect("store poisoned");
        if inner.sessions.contains_key(key) {
            return Err(SessionError::AlreadyExists(key.to_string()));
        }
        inner.sessions.insert(*key, meta.clone());
        inner.events.insert(*key, Vec::new());
        Ok(())
    }

    async fn get_session(&self, key: &SessionKey) -> Result<Option<SessionMeta>, SessionError> {
        Ok(self
            .inner
            .lock()
            .expect("store poisoned")
            .sessions
            .get(key)
            .cloned())
    }

    async fn append_event(
        &self,
        key: &SessionKey,
        event: SessionEvent,
    ) -> Result<SessionEvent, SessionError> {
        self.append_event_inner(key, event)
    }

    async fn read_events(&self, key: &SessionKey) -> Result<Vec<SessionEvent>, SessionError> {
        Ok(self
            .inner
            .lock()
            .expect("store poisoned")
            .events
            .get(key)
            .cloned()
            .ok_or(SessionError::SessionNotFound(*key))?)
    }

    async fn read_path(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError> {
        let events = self.read_events(key).await?;
        let fold = fold_surface(&events)?;
        let end = self.active_end_seq(key)?;
        let chain = match end {
            Some(seq) => chain_from(&fold, seq),
            None => fold.chain.clone(),
        };
        project_messages(&events, &chain)
    }

    async fn read_path_from(
        &self,
        key: &SessionKey,
        end_seq: u64,
    ) -> Result<Vec<Message>, SessionError> {
        let events = self.read_events(key).await?;
        let fold = fold_surface(&events)?;
        if !fold.prev_of.contains_key(&end_seq) {
            return Err(SessionError::InvalidEvent(format!(
                "末端 seq {end_seq} 不是 surface 事件"
            )));
        }
        let chain = chain_from(&fold, end_seq);
        project_messages(&events, &chain)
    }

    async fn set_active_path(
        &self,
        key: &SessionKey,
        message_id: Option<MessageId>,
    ) -> Result<(), SessionError> {
        let mut inner = self.inner.lock().expect("store poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        meta.active_path = message_id;
        Ok(())
    }

    async fn read_all(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError> {
        Ok(self
            .read_events(key)
            .await?
            .into_iter()
            .map(|e| e.message)
            .collect())
    }

    async fn resolve_seq(
        &self,
        key: &SessionKey,
        message_id: MessageId,
    ) -> Result<u64, SessionError> {
        let events = self.read_events(key).await?;
        events
            .iter()
            .rev()
            .find(|e| e.message.id == message_id)
            .map(|e| e.seq)
            .ok_or(SessionError::Internal(format!(
                "消息 {message_id} 不在事件日志"
            )))
    }

    async fn set_goal(&self, key: &SessionKey, goal: &Goal) -> Result<(), SessionError> {
        let mut inner = self.inner.lock().expect("store poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        meta.goal = Some(goal.clone());
        Ok(())
    }

    async fn archive(&self, key: &SessionKey) -> Result<(), SessionError> {
        let mut inner = self.inner.lock().expect("store poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        meta.status = SessionStatus::Archived;
        meta.archived_at = Some(chrono::Utc::now());
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<SessionMeta>, SessionError> {
        Ok(self
            .inner
            .lock()
            .expect("store poisoned")
            .sessions
            .values()
            .cloned()
            .collect())
    }

    async fn set_last_activity(
        &self,
        key: &SessionKey,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), SessionError> {
        let mut inner = self.inner.lock().expect("store poisoned");
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        meta.last_activity_at = at;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 测试：投影语义（遮蔽链 / 分支 / 压缩 / 校验）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::Attachment;

    async fn store_with(key: SessionKey) -> (InMemorySessionStore, SessionKey) {
        let store = InMemorySessionStore::new();
        let meta = SessionMeta::new(key);
        store.create_session(&key, &meta).await.unwrap();
        (store, key)
    }

    fn user(text: &str) -> Message {
        Message::user(text)
    }

    fn assistant(text: &str) -> Message {
        Message::assistant(text)
    }

    async fn append(
        store: &InMemorySessionStore,
        key: &SessionKey,
        msg: Message,
        op: SurfaceOp,
    ) -> SessionEvent {
        let event = SessionEvent::new(msg, op);
        store.append_event(key, event).await.unwrap()
    }

    #[tokio::test]
    async fn append_chain_projects_in_order() {
        let (store, key) = store_with(SessionKey::new()).await;
        append(&store, &key, user("你好"), SurfaceOp::Append).await;
        append(&store, &key, assistant("你好！"), SurfaceOp::Append).await;
        let msgs = store.read_path(&key).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(
            msgs[0].id,
            msgs[1].parent_id.unwrap(),
            "链内 parent 应指向前驱"
        );
        assert_eq!(msgs[1].parent_id, Some(msgs[0].id));
    }

    #[tokio::test]
    async fn edit_shadows_old_assistant_and_keeps_branch() {
        let (store, key) = store_with(SessionKey::new()).await;
        let u = append(&store, &key, user("问题"), SurfaceOp::Append).await;
        let a = append(&store, &key, assistant("旧回答"), SurfaceOp::Append).await;
        let a_seq = a.seq;

        // 编辑：新 assistant 事件 replace 遮蔽旧 assistant。
        let mut edit = SessionEvent::new(
            assistant("新回答"),
            SurfaceOp::Replace {
                start: a_seq,
                end: a_seq,
            },
        );
        edit.source_event_seqs = vec![a_seq];
        let stored = store.append_event(&key, edit).await.unwrap();
        store
            .set_active_path(&key, Some(stored.message.id))
            .await
            .unwrap();

        let msgs = store.read_path(&key).await.unwrap();
        assert_eq!(msgs.len(), 2, "编辑后活跃链 = [user, 新回答]");
        assert!(matches!(
            &msgs[1].kind,
            MessageKind::Assistant { text } if text == "新回答"
        ));

        // 切回旧分支：旧 assistant 仍是候选末端。
        let old = store.read_path_from(&key, a_seq).await.unwrap();
        assert_eq!(old.len(), 2);
        assert!(matches!(
            &old[1].kind,
            MessageKind::Assistant { text } if text == "旧回答"
        ));
        // 全量日志含被遮蔽消息（人读 transcript）。
        let all = store.read_all(&key).await.unwrap();
        assert_eq!(all.len(), 3);
        let _ = u;
    }

    #[tokio::test]
    async fn compaction_summary_shadows_prefix_and_keeps_tail() {
        let (store, key) = store_with(SessionKey::new()).await;
        let mut seqs = Vec::new();
        for i in 0..4 {
            let e = append(&store, &key, user(&format!("m{i}")), SurfaceOp::Append).await;
            seqs.push(e.seq);
        }
        // 压缩前两条 → summary replace [seqs[0]..=seqs[1]]，保留后两条。
        let mut summary = SessionEvent::new(
            Message::system("上下文压缩摘要：前两条"),
            SurfaceOp::Replace {
                start: seqs[0],
                end: seqs[1],
            },
        );
        summary.source_event_seqs = vec![seqs[0], seqs[1]];
        let stored = store.append_event(&key, summary).await.unwrap();
        // 活跃末端 = 保留段末端（m3 的消息 id），对齐 Kernel 压缩后 set_active_path(tail_end)。
        store
            .set_active_path(&key, Some(stored.message.id))
            .await
            .unwrap();
        // 以摘要为末端的链 = [摘要]（压缩点分支）。
        let at_summary = store.read_path(&key).await.unwrap();
        assert_eq!(at_summary.len(), 1);
        // 默认链（无 active_path）= fold.chain = [摘要, m2, m3]。
        store.set_active_path(&key, None).await.unwrap();
        let msgs = store.read_path(&key).await.unwrap();
        assert_eq!(msgs.len(), 3, "活跃链 = [摘要, m2, m3]");
        assert!(matches!(
            &msgs[0].kind,
            MessageKind::System { text } if text.contains("上下文压缩摘要")
        ));
        assert!(matches!(&msgs[1].kind, MessageKind::User { text, .. } if text == "m2"));
        assert!(matches!(&msgs[2].kind, MessageKind::User { text, .. } if text == "m3"));

        // 被压消息仍在全量日志（transcript 可读）。
        let all = store.read_all(&key).await.unwrap();
        assert_eq!(all.len(), 5);
    }

    #[tokio::test]
    async fn replace_requires_source_coverage() {
        let (store, key) = store_with(SessionKey::new()).await;
        let a = append(&store, &key, assistant("回答"), SurfaceOp::Append).await;
        let mut bad = SessionEvent::new(
            assistant("重写"),
            SurfaceOp::Replace {
                start: a.seq,
                end: a.seq,
            },
        );
        bad.source_event_seqs = Vec::new(); // 缺被遮蔽节点 → 拒绝
        let err = store.append_event(&key, bad).await.unwrap_err();
        assert!(matches!(err, SessionError::InvalidEvent(_)), "{err}");
    }

    #[tokio::test]
    async fn seq_gap_rejected_on_fold() {
        let (store, key) = store_with(SessionKey::new()).await;
        append(&store, &key, user("a"), SurfaceOp::Append).await;
        let events = store.read_events(&key).await.unwrap();
        let mut broken = events;
        broken[0].seq = 9;
        assert!(fold_surface(&broken).is_err());
    }

    #[tokio::test]
    async fn user_message_carries_attachments() {
        let (store, key) = store_with(SessionKey::new()).await;
        let mut msg = user("看图");
        if let MessageKind::User { attachments, .. } = &mut msg.kind {
            attachments.push(Attachment {
                path: "/tmp/x.png".into(),
                name: Some("x.png".into()),
                mime: None,
                data_base64: None,
            });
        }
        append(&store, &key, msg, SurfaceOp::Append).await;
        let msgs = store.read_path(&key).await.unwrap();
        match &msgs[0].kind {
            MessageKind::User { attachments, .. } => {
                assert_eq!(attachments.len(), 1);
            }
            _ => panic!("应为 user 消息"),
        }
    }
}
