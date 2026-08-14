//! ServiceId 与 ServiceHandles：混合式服务标识与类型化句柄容器（ADR-0002）。

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::model::ModelHandle;

use super::dynamic::DynamicService;
use super::session::SessionHandle;

/// Capability seam（ADR-0006）：Service Definition 的能力标识。
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

/// Capability seam（ADR-0006）：Service Provider 容器。
/// 服务句柄容器：会话/模型两个内置服务走类型化槽位，业务服务走类型擦除包，
/// 插件侧用 [`ServiceHandles::get_custom`] 做运行时 downcast 取回；
/// 另设 dynamic 槽位：实现 [`DynamicService`] 的业务服务可被 Rune 脚本插件
/// 按 method + JSON 参数调用（脚本无具体类型，见 `services::dynamic`）。
#[derive(Default, Clone)]
pub struct ServiceHandles {
    session: Option<SessionHandle>,
    model: Option<ModelHandle>,
    custom: HashMap<ServiceId, Arc<dyn Any + Send + Sync>>,
    dynamic: HashMap<ServiceId, Arc<dyn DynamicService>>,
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

    /// 取动态调用句柄（Rune 脚本桥用；无 dynamic 实现时返回 None）。
    pub fn get_dynamic(&self, id: &ServiceId) -> Option<Arc<dyn DynamicService>> {
        self.dynamic.get(id).cloned()
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

    /// 注入动态调用实现（供脚本插件按 method + JSON 调用；可与
    /// `with_custom` 同 id 并存——Rust 插件仍走 downcast，脚本走 dynamic）。
    pub fn with_dynamic(mut self, id: ServiceId, h: Arc<dyn DynamicService>) -> Self {
        self.dynamic.insert(id, h);
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
        set.extend(self.dynamic.keys().cloned());
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
            } else if let Some(v) = self.dynamic.get(id) {
                out.dynamic.insert(id.clone(), v.clone());
            }
        }
        out
    }
}
