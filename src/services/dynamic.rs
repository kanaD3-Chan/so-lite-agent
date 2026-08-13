//! DynamicService：自定义服务的动态调用接口（Rune 脚本插件访问通道）。

use async_trait::async_trait;
use serde_json::Value;

use crate::contract::ToolError;

/// 动态调用接口：自定义服务若要被 **Rune 脚本插件**访问，需实现本 trait。
///
/// 脚本没有具体类型（无法走 `get_custom::<T>()` downcast），只能按
/// `method + JSON 参数` 调用；实现方可把方法名映射到自己的业务方法。
/// 未实现本 trait 的自定义服务对脚本**不可见**（脚本 requires 声明了但服务
/// 无动态实现时注册 fail-fast，报清晰错误）——结构性白名单的一部分
/// （ADR-0006 Rune 用户插件 eBPF 模型）。
///
/// Rust 插件路径不受影响：仍按具体类型 `get_custom::<T>()` downcast（ADR-0002）。
#[async_trait]
pub trait DynamicService: Send + Sync {
    /// 按方法名 + JSON 参数调用；错误经 [`ToolError`] 结构化返回（回喂模型）。
    async fn call(&self, method: &str, params: Value) -> Result<Value, ToolError>;
}
