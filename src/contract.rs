//! 公开契约：入口点元数据、调用方策略、结构化错误。

use schemars::JsonSchema;
use schemars::Schema;
use serde::{Deserialize, Serialize};

use crate::services::ServiceId;

/// 调用方策略：决定 EntryPoint 谁能调用。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum CallerPolicy {
    /// 模型可调，用户必可调（用户经 trigger_command 触发）。
    UserAndModel,
    /// 仅用户可调，不出现在模型工具列表。
    UserOnly,
}

/// 加载策略：插件何时执行 register。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LoadPolicy {
    /// 读取即加载（启动时注册）。
    Eager,
    /// 首次使用才加载（默认）。
    #[default]
    Lazy,
}

/// 插件静态元数据（两段式契约第一阶段）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Info {
    pub namespace: String,
    /// 启用标记：**默认 false**——插件必须显式 `enabled: true` 才会注册；
    /// 禁用插件可保留在聚合点/代码中，注册表静默跳过（不做 fail-fast）。
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub requires: Vec<ServiceId>,
    /// 内核插件声明的服务提供：每个 ServiceId 至多由一个内核插件提供；
    /// 用户插件不得声明（注册表 fail-fast 拒绝）。
    #[serde(default)]
    pub provides: Vec<ServiceId>,
    #[serde(default)]
    pub load: LoadPolicy,
    #[serde(default)]
    pub tools: Vec<ToolDef>,
    #[serde(default)]
    pub commands: Vec<CommandDef>,
    #[serde(default)]
    pub events: Vec<EventDef>,
}

/// 工具定义：短名 + 描述 + 参数 schema + 调用方策略。
/// handler 不在 info 中，register 阶段绑定（两段式契约）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    /// 是否对用户可见（功能中心展示）；false = 仅模型可调，不出现在用户面板。
    #[serde(default = "default_true")]
    pub user_visible: bool,
    /// 用户友好显示名（功能中心展示；缺省回退 name）。
    #[serde(default)]
    pub title: Option<String>,
    /// 用户功能分组（缺省归"其它"）。
    #[serde(default)]
    pub group: Option<String>,
    pub description: String,
    pub params: Schema,
    pub policy: CallerPolicy,
    /// 工具级超时（秒）；None = 使用内核默认值。
    #[serde(default)]
    pub timeout: Option<u64>,
    /// Iconify 图标名（如 "mdi:upload"），供 GUI 展示。
    #[serde(default)]
    pub icon: Option<String>,
}

/// 命令定义：GUI/用户触发，恒为 UserOnly。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandDef {
    pub name: String,
    #[serde(default = "default_true")]
    pub user_visible: bool,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
    pub description: String,
    pub params: Schema,
    /// Iconify 图标名（如 "mdi:file-upload"），供 GUI 命令面板展示。
    #[serde(default)]
    pub icon: Option<String>,
}

/// 事件定义：kernel 生命周期回调，不对外暴露。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventDef {
    pub name: String,
}

fn default_true() -> bool {
    true
}

/// 工具错误码：驱动 loop 护栏（同码连续失败计数等）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ToolErrorCode {
    UnknownTool,
    InvalidParams,
    HandlerError,
    Timeout,
    Aborted,
    Forbidden,
    ModelUnavailable,
    Internal,
}

/// 工具调用失败时回喂给模型的结构化错误。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub code: ToolErrorCode,
    pub message: String,
    pub retryable: bool,
}

impl ToolError {
    pub fn new(code: ToolErrorCode, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }

    pub fn unknown_tool(name: &str) -> Self {
        Self::new(
            ToolErrorCode::UnknownTool,
            format!("未知工具：{name}"),
            false,
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::InvalidParams, message, true)
    }

    pub fn handler(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::HandlerError, message, false)
    }

    pub fn timeout() -> Self {
        Self::new(
            ToolErrorCode::Timeout,
            "工具执行超时，已被内核强制终止",
            true,
        )
    }

    pub fn aborted() -> Self {
        Self::new(ToolErrorCode::Aborted, "执行被取消", false)
    }

    pub fn forbidden() -> Self {
        Self::new(
            ToolErrorCode::Forbidden,
            "该入口点不允许当前调用方调用",
            false,
        )
    }

    pub fn model_unavailable(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::ModelUnavailable, message, false)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ToolErrorCode::Internal, message, false)
    }
}

/// 注册期错误（与运行期 ToolError 分开）。
#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    #[error("namespace 已被占用：{0}")]
    NamespaceTaken(String),
    #[error("声明的服务不可用：{0:?}")]
    CapabilityUnavailable(Vec<ServiceId>),
    #[error("服务已被内核插件提供：{0:?}")]
    ServiceTaken(ServiceId),
    #[error("只有内核插件能声明 provides：{0:?}")]
    ProvisionNotAllowed(Vec<ServiceId>),
    #[error("入口点已存在：{0}")]
    DuplicateEntry(String),
    #[error("未声明的入口点：{0}")]
    UndeclaredEntry(String),
    #[error("wire name 冲突：{0}")]
    WireNameCollision(String),
    #[error("注册失败：{0}")]
    Internal(String),
}

/// 空参数 schema 辅助：`schema_for!(EmptyParams)`。
#[derive(JsonSchema)]
#[allow(dead_code)]
struct EmptyParams {}

pub fn empty_params() -> Schema {
    schemars::json_schema!({"type": "object"})
}

/// 全名 → wire name（`::` → `__`）：
/// 双下划线让 `a::b_c`（→ `a__b_c`）与 `a_b::c`（→ `a_b__c`）不再撞名，
/// 插件内部的下划线得以保留；真正撞名只剩 `a::b__c` vs `a__b::c` 这类
/// 病态组合，仍由注册期全局唯一校验兜底。
pub fn full_to_wire(full: &str) -> String {
    full.replace("::", "__")
}

/// 内部全名：`namespace::short`。
pub fn full_name(namespace: &str, short: &str) -> String {
    format!("{namespace}::{short}")
}
