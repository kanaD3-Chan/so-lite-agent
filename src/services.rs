//! 服务契约与受控句柄：ServiceId、SessionStore 通用契约、InMemory 默认实现、
//! ServiceHandles 类型化容器。

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::agent::session::{Goal, SessionKey, SessionMeta, SessionStatus};
use crate::message::{Message, MessageId, MessageKind};
use crate::model::ModelHandle;

/// 服务标识：字符串背书的 newtype，内置会话/模型，业务服务用 [`ServiceId::custom`]。
/// 注册表按 `provides` 全局唯一（ADR-0002 混合式设计）。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ServiceId(String);

impl ServiceId {
    pub fn session() -> Self {
        Self("session".into())
    }

    pub fn model() -> Self {
        Self("model".into())
    }

    pub fn custom(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ServiceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

// ---------- Session 契约 ----------

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
    #[error("内部错误：{0}")]
    Internal(String),
}

/// 会话持久化：kernel 内部（Session scheduler / loop / 压缩）使用。
#[async_trait]
pub trait SessionStore: Send + Sync {
    async fn create_session(
        &self,
        key: &SessionKey,
        meta: &SessionMeta,
    ) -> Result<(), SessionError>;
    async fn get_session(&self, key: &SessionKey) -> Result<Option<SessionMeta>, SessionError>;
    async fn append_message(&self, key: &SessionKey, msg: &Message) -> Result<(), SessionError>;
    async fn read_path(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError>;
    async fn read_all(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError>;
    /// 设置活跃路径末端（消息树分支切换；None = 退化为线性全链）。
    async fn set_active_path(
        &self,
        key: &SessionKey,
        message_id: Option<MessageId>,
    ) -> Result<(), SessionError>;
    /// 在 message_id 处派生新分支：消息复制新 id（parent 不变、文本替换），
    /// 编辑点之后的旧消息保留但不属于活跃路径（历史不截断）。返回新活跃路径。
    async fn derive_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
        text: &str,
    ) -> Result<Vec<Message>, SessionError>;
    /// 切换到以 message_id 为末端的活跃路径（沿 parent 链回溯）。
    async fn switch_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
    ) -> Result<Vec<Message>, SessionError>;
    /// 压缩接入：把摘要消息追加进会话，并把 tail_start（保留段首条）的 parent
    /// 改挂到摘要下，使活跃路径变为 `摘要 → 保留段 → …`。
    async fn splice_compaction(
        &self,
        key: &SessionKey,
        summary: &Message,
        tail_start: MessageId,
    ) -> Result<(), SessionError>;
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

// ---------- InMemory 默认实现 ----------

#[derive(Debug, Default)]
struct Inner {
    sessions: HashMap<SessionKey, SessionMeta>,
    messages: HashMap<SessionKey, Vec<Message>>,
}

/// M2 默认会话存储：全内存，重启即失。文件实现由使用方提供。
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
        inner.messages.insert(*key, Vec::new());
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

    async fn append_message(&self, key: &SessionKey, msg: &Message) -> Result<(), SessionError> {
        let mut inner = self.inner.lock().expect("store poisoned");
        let path = inner
            .messages
            .get_mut(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        path.push(msg.clone());
        Ok(())
    }

    async fn read_path(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError> {
        let (messages, active_path) = {
            let inner = self.inner.lock().expect("store poisoned");
            let messages = inner
                .messages
                .get(key)
                .cloned()
                .ok_or(SessionError::SessionNotFound(*key))?;
            let active_path = inner.sessions.get(key).and_then(|m| m.active_path);
            (messages, active_path)
        };
        Ok(active_chain(&messages, active_path))
    }

    async fn read_all(&self, key: &SessionKey) -> Result<Vec<Message>, SessionError> {
        Ok(self
            .inner
            .lock()
            .expect("store poisoned")
            .messages
            .get(key)
            .cloned()
            .unwrap_or_default())
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

    async fn derive_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
        text: &str,
    ) -> Result<Vec<Message>, SessionError> {
        let (messages, active_path) = {
            let inner = self.inner.lock().expect("store poisoned");
            let messages = inner
                .messages
                .get(key)
                .cloned()
                .ok_or(SessionError::SessionNotFound(*key))?;
            let active_path = inner.sessions.get(key).and_then(|m| m.active_path);
            (messages, active_path)
        };
        let chain = active_chain(&messages, active_path);
        let idx = chain
            .iter()
            .position(|m| m.id == message_id)
            .ok_or(SessionError::Internal("消息不在活跃路径".into()))?;
        let original = &chain[idx];
        if !matches!(original.kind, MessageKind::Assistant { .. }) {
            return Err(SessionError::Internal("只能编辑 assistant 消息".into()));
        }
        let mut new_msg = original.clone();
        new_msg.id = MessageId::new();
        new_msg.parent_id = original.parent_id;
        new_msg.kind = MessageKind::Assistant {
            text: text.to_string(),
        };
        new_msg.created_at = chrono::Utc::now();

        let mut new_path = chain[..idx].to_vec();
        new_path.push(new_msg.clone());
        let mut inner = self.inner.lock().expect("store poisoned");
        let path = inner
            .messages
            .get_mut(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        path.push(new_msg);
        let meta = inner
            .sessions
            .get_mut(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        meta.active_path = new_path.last().map(|m| m.id);
        Ok(new_path)
    }

    async fn switch_branch(
        &self,
        key: &SessionKey,
        message_id: MessageId,
    ) -> Result<Vec<Message>, SessionError> {
        let messages = self.read_all(key).await?;
        if !messages.iter().any(|m| m.id == message_id) {
            return Err(SessionError::Internal("消息不存在".into()));
        }
        let chain = active_chain(&messages, Some(message_id));
        self.set_active_path(key, Some(message_id)).await?;
        Ok(chain)
    }

    async fn splice_compaction(
        &self,
        key: &SessionKey,
        summary: &Message,
        tail_start: MessageId,
    ) -> Result<(), SessionError> {
        let mut inner = self.inner.lock().expect("store poisoned");
        let path = inner
            .messages
            .get_mut(key)
            .ok_or(SessionError::SessionNotFound(*key))?;
        let tail = path
            .iter_mut()
            .find(|m| m.id == tail_start)
            .ok_or(SessionError::Internal("保留段首条不存在".into()))?;
        tail.parent_id = Some(summary.id);
        path.push(summary.clone());
        Ok(())
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

/// 活跃路径回溯：从末端沿 parent 链还原（消息树分支语义）。
pub fn active_chain(messages: &[Message], active_path: Option<MessageId>) -> Vec<Message> {
    let Some(end) = active_path else {
        return messages.to_vec();
    };
    let by_id: HashMap<MessageId, Message> = messages.iter().map(|m| (m.id, m.clone())).collect();
    if !by_id.contains_key(&end) {
        return messages.to_vec();
    }
    let mut chain = Vec::new();
    let mut cur = Some(end);
    while let Some(id) = cur {
        match by_id.get(&id) {
            Some(m) => {
                cur = m.parent_id;
                chain.push(m.clone());
            }
            None => break,
        }
    }
    chain.reverse();
    chain
}

// ---------- ServiceHandles：类型化封闭容器（ADR-0002） ----------

/// 服务句柄容器：会话/模型两个内置服务走类型化槽位，业务服务走类型擦除包，
/// 插件侧用 [`ServiceHandles::get_custom`] 做运行时 downcast 取回。
#[derive(Default, Clone)]
pub struct ServiceHandles {
    session: Option<SessionHandle>,
    model: Option<ModelHandle>,
    custom: HashMap<ServiceId, Arc<dyn Any + Send + Sync>>,
}

impl ServiceHandles {
    pub fn session(&self) -> Option<&SessionHandle> {
        self.session.as_ref()
    }

    pub fn model(&self) -> Option<&ModelHandle> {
        self.model.as_ref()
    }

    pub fn get_custom<T: Any + Send + Sync>(&self, id: &ServiceId) -> Option<Arc<T>> {
        self.custom
            .get(id)
            .and_then(|v| v.clone().downcast::<T>().ok())
    }

    pub fn with_session(mut self, h: SessionHandle) -> Self {
        self.session = Some(h);
        self
    }

    pub fn with_model(mut self, h: ModelHandle) -> Self {
        self.model = Some(h);
        self
    }

    pub fn with_custom<T: Any + Send + Sync>(mut self, id: ServiceId, h: Arc<T>) -> Self {
        self.custom.insert(id, h);
        self
    }

    pub fn available(&self) -> HashSet<ServiceId> {
        let mut set = HashSet::new();
        if self.session.is_some() {
            set.insert(ServiceId::session());
        }
        if self.model.is_some() {
            set.insert(ServiceId::model());
        }
        set.extend(self.custom.keys().cloned());
        set
    }

    /// 按能力声明过滤：插件只拿到声明过的服务（结构上受限）。
    pub fn filter(&self, requires: &[ServiceId]) -> ServiceHandles {
        let mut out = ServiceHandles::default();
        for id in requires {
            if id == &ServiceId::session() {
                out.session = self.session.clone();
            } else if id == &ServiceId::model() {
                out.model = self.model.clone();
            } else if let Some(v) = self.custom.get(id) {
                out.custom.insert(id.clone(), v.clone());
            }
        }
        out
    }
}
