//! JSONL 会话事实日志落盘（ADR-0007 第二步）：每会话一个 `<key>.jsonl`，
//! 首行会话元数据（SessionMeta），后续每行一条事件（lossless JSON）。
//!
//! 参考 DeepSeek Harness 持久化契约 + mistake-agent storage 文件后端（原子写 /
//! 崩溃恢复）：
//! - append 强制事件 JSON 可序列化（契约层已校验），追加即落盘（append_line）；
//! - 崩溃尾部修复：加载时跳过/截断最后不完整行（崩溃可能留下半行）；
//! - 元数据更新（goal/archive/last_activity/active_path）重写首行（原子写：
//!   tmp + rename，参考 mistake-agent `atomic_write_str`）；
//! - 会话目录由使用方创建（`sl-agent` 数据根），本实现不做懒创建。

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::agent::session::{Goal, SessionKey, SessionMeta, SessionStatus};
use crate::message::{Message, MessageId};
use crate::services::session::{SessionError, SessionEvent};

use super::session::{SessionStore, chain_from, fold_surface, project_messages};

/// JSONL 会话存储：事件日志 + 遮蔽投影（与 [`super::session::InMemorySessionStore`]
/// 相同的投影语义，只是落盘为 JSONL）。
#[derive(Clone)]
pub struct JsonlSessionStore {
    root: PathBuf,
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    sessions: HashMap<SessionKey, SessionMeta>,
    events: HashMap<SessionKey, Vec<SessionEvent>>,
}

impl JsonlSessionStore {
    /// 打开数据根目录（`<root>/sessions/<key>.jsonl`），加载全部会话。
    /// 崩溃尾部修复：最后不完整行被截断丢弃；中间坏行报 Corrupt（fail-fast，
    /// 参考 DSH 坏日志拒绝重建）。
    pub fn open(root: &Path) -> Result<Self, SessionError> {
        let sessions_dir = root.join("sessions");
        let mut inner = Inner::default();
        if sessions_dir.is_dir() {
            for entry in std::fs::read_dir(&sessions_dir)
                .map_err(|e| SessionError::Io(format!("读会话目录失败：{e}")))?
            {
                let entry = entry.map_err(|e| SessionError::Io(format!("读目录项失败：{e}")))?;
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                    continue;
                }
                let (key, meta, events) = load_session_file(&path)?;
                inner.sessions.insert(key, meta);
                inner.events.insert(key, events);
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            inner: Arc::new(Mutex::new(inner)),
        })
    }

    fn session_path(&self, key: &SessionKey) -> PathBuf {
        self.root.join("sessions").join(format!("{key}.jsonl"))
    }

    /// 整文件重写（meta 首行 + 全部事件行）。
    fn persist(&self, key: &SessionKey) -> Result<(), SessionError> {
        let (meta, events) = {
            let inner = self.inner.lock().expect("store poisoned");
            let meta = inner
                .sessions
                .get(key)
                .ok_or(SessionError::SessionNotFound(*key))?
                .clone();
            let events = inner.events.get(key).cloned().unwrap_or_default();
            (meta, events)
        };
        let path = self.session_path(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SessionError::Io(format!("创建目录失败 {parent:?}：{e}")))?;
        }
        let mut out = String::new();
        out.push_str(
            &serde_json::to_string(&meta)
                .map_err(|e| SessionError::Io(format!("会话元数据序列化失败：{e}")))?,
        );
        out.push('\n');
        for ev in &events {
            out.push_str(
                &serde_json::to_string(ev)
                    .map_err(|e| SessionError::Io(format!("事件序列化失败：{e}")))?,
            );
            out.push('\n');
        }
        atomic_write_str(&path, &out)
    }

    /// 追加单条事件行（append-only，与整写分离——只追加不重写）。
    fn append_line(&self, key: &SessionKey, event: &SessionEvent) -> Result<(), SessionError> {
        let path = self.session_path(key);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| SessionError::Io(format!("创建目录失败 {parent:?}：{e}")))?;
        }
        let mut line = serde_json::to_string(event)
            .map_err(|e| SessionError::Io(format!("事件序列化失败：{e}")))?;
        line.push('\n');
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| SessionError::Io(format!("打开失败 {path:?}：{e}")))?;
        file.write_all(line.as_bytes())
            .map_err(|e| SessionError::Io(format!("追加失败：{e}")))?;
        Ok(())
    }

    /// 追加事件（内存 + JSONL），复用投影校验。
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
        // 校验当前日志折叠 + 新事件 surface 合法（与 InMemory 同规则）。
        let mut probe = event.clone();
        probe.seq = seq;
        probe.created_at = chrono::Utc::now();
        let mut fold = fold_surface(log)?;
        super::session::apply_event(&mut fold, &probe)?;
        let stored = SessionEvent {
            seq,
            created_at: chrono::Utc::now(),
            ..event
        };
        log.push(stored.clone());
        // 落盘：追加行（崩溃只丢尾部半行，加载时修复）。
        self.append_line(key, &stored)?;
        Ok(stored)
    }
}

/// 加载单个会话文件：首行 meta + 事件行；最后不完整行截断（崩溃尾部修复）。
fn load_session_file(
    path: &Path,
) -> Result<(SessionKey, SessionMeta, Vec<SessionEvent>), SessionError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| SessionError::Io(format!("读会话文件失败 {path:?}：{e}")))?;
    let mut lines = text.lines().peekable();
    let Some(first) = lines.next() else {
        return Err(SessionError::Corrupt(format!("会话文件为空：{path:?}")));
    };
    let meta: SessionMeta = serde_json::from_str(first)
        .map_err(|e| SessionError::Corrupt(format!("会话元数据解析失败 {path:?}：{e}")))?;
    let key = meta.key;
    let mut events = Vec::new();
    let line_count = text.lines().count();
    for (idx, line) in text.lines().enumerate().skip(1) {
        match serde_json::from_str::<SessionEvent>(line) {
            Ok(ev) => events.push(ev),
            Err(e) => {
                // 崩溃尾部修复：只有最后一行允许不完整（截断）；中间坏行 = Corrupt。
                let is_last = idx == line_count - 1;
                if is_last {
                    log::warn!("会话尾部不完整行被截断：{path:?}（{e}）");
                    break;
                }
                return Err(SessionError::Corrupt(format!(
                    "会话文件 {path:?} 第 {} 行解析失败：{e}",
                    idx + 1
                )));
            }
        }
    }
    // seq 连续校验（坏日志拒绝重建，参考 DSH）。
    for (i, ev) in events.iter().enumerate() {
        if ev.seq != i as u64 {
            return Err(SessionError::Corrupt(format!(
                "会话文件 {path:?} seq {} 不连续；期望 {}",
                ev.seq, i
            )));
        }
    }
    Ok((key, meta, events))
}

/// 原子写：tmp + rename（参考 mistake-agent `atomic_write_str`；tmp 名带随机后缀
/// 防并发写踩踏）。
fn atomic_write_str(path: &Path, text: &str) -> Result<(), SessionError> {
    let tmp = path.with_extension(format!("jsonl.tmp.{}", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, text)
        .map_err(|e| SessionError::Io(format!("写临时文件失败 {tmp:?}：{e}")))?;
    std::fs::rename(&tmp, path)
        .map_err(|e| SessionError::Io(format!("原子改名失败 {path:?}：{e}")))?;
    Ok(())
}

#[async_trait]
impl SessionStore for JsonlSessionStore {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), SessionError> {
        {
            let mut inner = self.inner.lock().expect("store poisoned");
            if inner.sessions.contains_key(key) {
                return Err(SessionError::AlreadyExists(key.to_string()));
            }
            inner.sessions.insert(*key, meta.clone());
            inner.events.insert(*key, Vec::new());
        }
        self.persist(key)
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
        {
            let mut inner = self.inner.lock().expect("store poisoned");
            let meta = inner
                .sessions
                .get_mut(key)
                .ok_or(SessionError::SessionNotFound(*key))?;
            meta.active_path = message_id;
        }
        self.persist(key)
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
        {
            let mut inner = self.inner.lock().expect("store poisoned");
            let meta = inner
                .sessions
                .get_mut(key)
                .ok_or(SessionError::SessionNotFound(*key))?;
            meta.goal = Some(goal.clone());
        }
        self.persist(key)
    }

    async fn archive(&self, key: &SessionKey) -> Result<(), SessionError> {
        {
            let mut inner = self.inner.lock().expect("store poisoned");
            let meta = inner
                .sessions
                .get_mut(key)
                .ok_or(SessionError::SessionNotFound(*key))?;
            meta.status = SessionStatus::Archived;
            meta.archived_at = Some(chrono::Utc::now());
        }
        self.persist(key)
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
        {
            let mut inner = self.inner.lock().expect("store poisoned");
            let meta = inner
                .sessions
                .get_mut(key)
                .ok_or(SessionError::SessionNotFound(*key))?;
            meta.last_activity_at = at;
        }
        self.persist(key)
    }
}

impl JsonlSessionStore {
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

// ---------------------------------------------------------------------------
// 测试：JSONL 落盘 + 崩溃尾部修复 + 投影与 InMemory 一致
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::MessageKind;
    use tempfile::TempDir;

    async fn append(
        store: &JsonlSessionStore,
        key: &SessionKey,
        msg: Message,
        op: super::super::session::SurfaceOp,
    ) -> SessionEvent {
        let event = SessionEvent::new(msg, op);
        store.append_event(key, event).await.unwrap()
    }

    #[tokio::test]
    async fn roundtrip_persists_and_reloads() {
        let dir = TempDir::new().unwrap();
        let key = SessionKey::new();
        let store = JsonlSessionStore::open(dir.path()).unwrap();
        store
            .create_session(&key, &SessionMeta::new(key))
            .await
            .unwrap();
        let a = append(
            &store,
            &key,
            Message::user("你好"),
            super::super::session::SurfaceOp::Append,
        )
        .await;
        let _ = a;
        append(
            &store,
            &key,
            Message::assistant("你好！"),
            super::super::session::SurfaceOp::Append,
        )
        .await;

        // 重开（模拟重启）：事件与投影完整恢复。
        let reloaded = JsonlSessionStore::open(dir.path()).unwrap();
        let metas = reloaded.list_sessions().await.unwrap();
        assert_eq!(metas.len(), 1);
        let msgs = reloaded.read_path(&key).await.unwrap();
        assert_eq!(msgs.len(), 2);
        assert!(matches!(&msgs[1].kind, MessageKind::Assistant { text } if text == "你好！"));
    }

    #[tokio::test]
    async fn crash_tail_truncated_on_reload() {
        let dir = TempDir::new().unwrap();
        let key = SessionKey::new();
        let store = JsonlSessionStore::open(dir.path()).unwrap();
        store
            .create_session(&key, &SessionMeta::new(key))
            .await
            .unwrap();
        append(
            &store,
            &key,
            Message::user("a"),
            super::super::session::SurfaceOp::Append,
        )
        .await;
        append(
            &store,
            &key,
            Message::assistant("b"),
            super::super::session::SurfaceOp::Append,
        )
        .await;

        // 模拟崩溃：在文件尾追加半行（不完整 JSON）。
        let path = store.session_path(&key);
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        f.write_all(br#"{"seq":2,"message":"{"#).unwrap();
        drop(f);

        let reloaded = JsonlSessionStore::open(dir.path()).unwrap();
        let events = reloaded.read_events(&key).await.unwrap();
        assert_eq!(events.len(), 2, "崩溃尾部半行应被截断");
    }

    #[tokio::test]
    async fn mid_file_corruption_rejected() {
        let dir = TempDir::new().unwrap();
        let key = SessionKey::new();
        let store = JsonlSessionStore::open(dir.path()).unwrap();
        store
            .create_session(&key, &SessionMeta::new(key))
            .await
            .unwrap();
        append(
            &store,
            &key,
            Message::user("a"),
            super::super::session::SurfaceOp::Append,
        )
        .await;
        append(
            &store,
            &key,
            Message::assistant("b"),
            super::super::session::SurfaceOp::Append,
        )
        .await;

        // 中间插入坏行（后面还有合法行，坏行不在尾部）。
        let path = store.session_path(&key);
        let text = std::fs::read_to_string(&path).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.insert(2, "not-json");
        std::fs::write(&path, lines.join("\n") + "\n").unwrap();

        assert!(JsonlSessionStore::open(dir.path()).is_err());
    }
}
