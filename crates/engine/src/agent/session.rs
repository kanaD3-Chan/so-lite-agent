//! 会话通用语义：SessionKey / Goal / SessionMeta / 中断总线 / 摘要器 / 会话切换钩子。
//!
//! 会话切换的**决策**（新消息先判断、回合末三动作）与默认调度器不属于通用运行时
//! （ADR-0010 由使用方实现）；crate 只提供 `SessionSwitch` 钩子供 loop 消费。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::message::{Message, MessageId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionKey(pub Uuid);

impl SessionKey {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionKey {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// 会话目标：continue / update_goal / start_new 的决策依据。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Goal {
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMeta {
    pub key: SessionKey,
    pub goal: Option<Goal>,
    pub status: SessionStatus,
    pub created_at: DateTime<Utc>,
    pub archived_at: Option<DateTime<Utc>>,
    pub last_activity_at: DateTime<Utc>,
    pub active_path: Option<MessageId>,
}

impl SessionMeta {
    pub fn new(key: SessionKey) -> Self {
        let now = Utc::now();
        Self {
            key,
            goal: None,
            status: SessionStatus::Active,
            created_at: now,
            archived_at: None,
            last_activity_at: now,
            active_path: None,
        }
    }
}

/// 会话摘要器：压缩/交接时生成摘要。真实实现（LLM）由使用方注入。
#[async_trait]
pub trait Summarizer: Send + Sync {
    async fn summarize(&self, messages: &[Message], goal: Option<&Goal>) -> String;
}

/// 默认计数摘要桩（与 MockModelService 对称，骨架可跑通）。
pub struct StubSummarizer;

#[async_trait]
impl Summarizer for StubSummarizer {
    async fn summarize(&self, messages: &[Message], goal: Option<&Goal>) -> String {
        let goal_text = goal
            .map(|g| g.text.clone())
            .unwrap_or_else(|| "（未记录目标）".into());
        format!(
            "上一个会话共 {} 条消息，会话目标：{}。",
            messages.len(),
            goal_text
        )
    }
}

/// 内部中断：内核组件向 agent loop 发出的环境变更信号，
/// 回合边界消费，不抢占当前回合。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "interrupt", rename_all = "snake_case")]
pub enum Interrupt {
    SessionSwitched {
        from: SessionKey,
        to: SessionKey,
        goal: Goal,
    },
    GoalUpdated {
        goal: Goal,
    },
    /// 通用配置变更（mistake-agent 的 SettingsChanged 通用化）。
    ConfigChanged,
    CompactionDone {
        session: SessionKey,
    },
    /// 业务自定义中断（ADR-0011）：内核组件/使用方上浮业务语义中断
    /// （如告警通知、定时提醒），kernel 不解析 name/payload，只负责运输与
    /// 审计；空闲时消费并自动开回合由使用方装配（`Kernel::run_turn` +
    /// `KernelBuilder::interrupt_bus` 注入同一总线）。
    Custom {
        name: String,
        #[serde(default)]
        payload: serde_json::Value,
    },
}

/// 中断总线：跨内核组件共享，回合边界取空。
#[derive(Clone, Default)]
pub struct InterruptBus {
    queue: Arc<Mutex<VecDeque<Interrupt>>>,
}

impl InterruptBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn send(&self, interrupt: Interrupt) {
        self.queue
            .lock()
            .expect("interrupt bus poisoned")
            .push_back(interrupt);
    }

    pub fn take_all(&self) -> Vec<Interrupt> {
        std::mem::take(&mut *self.queue.lock().expect("interrupt bus poisoned"))
            .into_iter()
            .collect()
    }
}

/// 会话切换钩子：回合内模型调用 `session::switch` 时由 loop 执行。
/// 默认调度器与工具注册由使用方实现（M4 插件手册）。
#[async_trait]
pub trait SessionSwitch: Send + Sync {
    async fn switch(&self, goal: &str) -> Result<SessionKey, String>;
}

/// 会话调度决策（mistake-agent SessionScheduler 的 kernel 侧接口；上游 ADR-0010
/// 决策与默认调度器由使用方实现）：新消息**前置决策**（先判断要不要切换上下文
/// 再回答）+ 回合末决策（continue / update_goal / start_new）。注入后
/// `Kernel::send_user_message*` 不再自行追加 user 消息，改由决策器返回进入回合
/// 的会话 key 与消息链（决策器内部负责追加 / 树内分叉 / 切换与 active_path）。
#[async_trait]
pub trait SessionDecision: Send + Sync {
    /// 新消息到达：返回 (进入回合的会话 key, 模型可见消息链)。
    /// 实现方负责：空闲超时检查、主模型决策（start_new → 树内分叉挂摘要节点）、
    /// user 消息落盘、set_active_path；决策失败默认继续当前会话（存疑即继续）。
    async fn on_new_message(
        &self,
        key: SessionKey,
        text: &str,
        display_text: Option<String>,
    ) -> Result<(SessionKey, Vec<Message>), String>;

    /// 回合结束：会话调度决策（如 start_new → 只挂摘要节点，下条消息从子树继续）。
    async fn on_turn_end(&self, key: &SessionKey, outcome: &[Message]) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_interrupt_round_trip() {
        // ADR-0011：业务自定义中断（name + payload）经总线传输可序列化往返。
        let interrupt = Interrupt::Custom {
            name: "iot.alert".into(),
            payload: serde_json::json!({"device_id": "env_sensor", "value": 33.0}),
        };
        let json = serde_json::to_string(&interrupt).unwrap();
        let back: Interrupt = serde_json::from_str(&json).unwrap();
        match back {
            Interrupt::Custom { name, payload } => {
                assert_eq!(name, "iot.alert");
                assert_eq!(payload["value"], 33.0);
            }
            other => panic!("应反序列化为 Custom：{other:?}"),
        }
    }
}
