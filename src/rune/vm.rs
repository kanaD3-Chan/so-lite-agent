//! ScriptVm：一次编译、按调用新建 `Vm` 执行（无共享可变状态）。

use std::sync::Arc;

use rune::runtime::{RuntimeContext, Value};
use rune::{Context, Diagnostics, Source, Sources, Unit, Vm};

/// 编译失败：收集诊断 + 原始错误（诊断含来源定位信息）。
#[derive(Debug, thiserror::Error)]
#[error("脚本编译失败：{0}")]
pub struct CompileError(pub String);

/// 调用失败：执行期 rune 错误 / 参数或返回值转换错误。
#[derive(Debug, thiserror::Error)]
#[error("脚本调用失败：{0}")]
pub struct CallError(pub String);

/// 一次编译的脚本宿主：持有 `Arc<RuntimeContext>` + `Arc<Unit>`，
/// 每次调用新建 `Vm`（构造是常量时间操作），无共享可变状态——
/// 可并发调用（本 crate 工具串行执行，天然安全）。
///
/// Capability seam 基础件（ADR-0006）：宿主函数白名单在编译期装进 context，
/// 未安装的函数 = prepare 编译失败（结构性拿不到）。
#[derive(Clone)]
pub struct ScriptVm {
    runtime: Arc<RuntimeContext>,
    unit: Arc<Unit>,
}

impl std::fmt::Debug for ScriptVm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ScriptVm")
            .field("unit", &self.unit)
            .finish_non_exhaustive()
    }
}

impl ScriptVm {
    /// 用默认基础上下文（安全标准模块）编译一段脚本。
    pub fn compile(source: &str) -> Result<Self, CompileError> {
        let context = Context::with_default_modules().map_err(|e| CompileError(e.to_string()))?;
        Self::compile_with_context(source, &context)
    }

    /// 用给定上下文编译（宿主函数由调用方装进 context；未安装函数 = 编译失败）。
    pub fn compile_with_context(source: &str, context: &Context) -> Result<Self, CompileError> {
        let mut sources = Sources::new();
        sources
            .insert(Source::memory(source).map_err(|e| CompileError(e.to_string()))?)
            .map_err(|e| CompileError(e.to_string()))?;
        let mut diagnostics = Diagnostics::new();
        let unit = rune::prepare(&mut sources)
            .with_context(context)
            .with_diagnostics(&mut diagnostics)
            .build()
            .map_err(|err| {
                let detail: Vec<String> = diagnostics
                    .diagnostics()
                    .iter()
                    .map(|d| format!("{d:?}"))
                    .collect();
                let mut msg = detail.join("；");
                if !msg.is_empty() {
                    msg.push('；');
                }
                msg.push_str(&err.to_string());
                CompileError(msg)
            })?;
        let runtime = context.runtime().map_err(|e| CompileError(e.to_string()))?;
        Ok(Self {
            runtime: Arc::new(runtime),
            unit: Arc::new(unit),
        })
    }

    /// 调脚本函数（0/1/2 个参数），返回 rune `Value`。
    pub async fn call(&self, name: &str, args: &[Value]) -> Result<Value, CallError> {
        let mut vm = Vm::new(self.runtime.clone(), self.unit.clone());
        let mut execution = match args.len() {
            0 => vm
                .execute([name], ())
                .map_err(|e| CallError(e.to_string()))?,
            1 => vm
                .execute([name], (args[0].clone(),))
                .map_err(|e| CallError(e.to_string()))?,
            2 => vm
                .execute([name], (args[0].clone(), args[1].clone()))
                .map_err(|e| CallError(e.to_string()))?,
            n => return Err(CallError(format!("最多支持 2 个参数，收到 {n}"))),
        };
        execution
            .async_complete()
            .await
            .into_result()
            .map_err(|e| CallError(e.to_string()))
    }

    /// 调脚本函数并转成 JSON（rune Value ↔ serde_json）。
    pub async fn call_json(
        &self,
        name: &str,
        args: &[Value],
    ) -> Result<serde_json::Value, CallError> {
        let value = self.call(name, args).await?;
        json_from_value(&value)
    }
}

/// rune `Value` → serde_json（rune Value 自带 Serialize）。
pub fn json_from_value(value: &Value) -> Result<serde_json::Value, CallError> {
    serde_json::to_value(value).map_err(|e| CallError(format!("rune 值转 JSON 失败：{e}")))
}

/// serde_json → rune `Value`（rune Value 自带 Deserialize）。
#[allow(dead_code)] // P1-B1 Rune 用户插件桥消费（检查点对齐后实现）
pub fn value_from_json(json: serde_json::Value) -> Result<Value, CallError> {
    serde_json::from_value(json).map_err(|e| CallError(format!("JSON 转 rune 值失败：{e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rune::runtime::Value;

    #[tokio::test]
    async fn pure_script_call_round_trip() {
        let vm = ScriptVm::compile(
            r#"
            pub fn main(number) { number + 10 }
            "#,
        )
        .unwrap();
        let out = vm.call("main", &[Value::from(33i64)]).await.unwrap();
        assert_eq!(out.as_signed().unwrap(), 43);
    }

    #[tokio::test]
    async fn json_round_trip() {
        let vm = ScriptVm::compile(
            r#"
            pub fn echo(params) { params }
            "#,
        )
        .unwrap();
        let params = serde_json::json!({ "a": [1, 2, 3], "b": "x", "ok": true });
        let arg = value_from_json(params.clone()).unwrap();
        let out = vm.call_json("echo", &[arg]).await.unwrap();
        assert_eq!(out, params);
    }

    #[tokio::test]
    async fn async_script_function_driven_by_async_complete() {
        // async_complete 驱动异步脚本函数（await 宿主函数的链路见 host::tests）。
        let vm = ScriptVm::compile(
            r#"
            pub async fn tick(a) { a + 1 }
            "#,
        )
        .unwrap();
        let out = vm.call("tick", &[Value::from(41i64)]).await.unwrap();
        assert_eq!(out.as_signed().unwrap(), 42);
    }

    #[tokio::test]
    async fn unresolved_function_fails_compile() {
        let err = ScriptVm::compile(
            r#"
            pub fn main() { secret_helper() }
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("secret_helper"), "{err}");
    }

    #[tokio::test]
    async fn call_error_maps_to_call_error() {
        let vm = ScriptVm::compile(
            r#"
            pub fn boom() { panic!("脚本爆炸") }
            "#,
        )
        .unwrap();
        let err = vm.call("boom", &[]).await.unwrap_err();
        assert!(err.to_string().contains("脚本爆炸"), "{err}");
    }
}
