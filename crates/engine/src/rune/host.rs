//! 宿主函数安装（eBPF helper 白名单的基础件，ADR-0006）：
//! 脚本能调用的函数 = 装进 context 的函数；未安装的函数 = prepare 编译失败
//! （结构性拿不到，防越权由「函数白名单」保证）。
//!
//! 安装路径：
//! - **动态闭包**：`Module::function(path, |args| async move { ... }).build()`
//!   ——按需装/卸，Rune 用户插件桥的「按 requires 裁剪」与 P2 热重载都靠它；
//! - **编译期函数**：`#[rune::function]` + `Module::function_meta`（类型化、性能好）。
//!
//! 动态 async 闭包参数逐个为 rune `FromValue`、返回为 rune `ToValue`；
//! 桥接层统一以 `rune::runtime::Value` 与 serde_json 互转（见 [`super::vm`]），
//! 宿主函数内部拿到的都是中立 JSON 语义，不暴露 rune 类型。

use rune::compile::Context;
use rune::{ContextError, Module};

/// 宿主函数安装失败（context 合并失败等）。
#[derive(Debug, thiserror::Error)]
#[error("宿主函数安装失败：{0}")]
pub struct HostError(pub String);

impl From<ContextError> for HostError {
    fn from(e: ContextError) -> Self {
        Self(e.to_string())
    }
}

/// 新建一个空模块：调用方往里装宿主函数后经 [`install`] 并入 context。
#[allow(dead_code)] // P1-B1 Rune 用户插件桥消费（检查点对齐后实现）
pub fn module() -> Module {
    Module::new()
}

/// 把模块（含宿主函数）并入 context：此后脚本可解析其中函数。
#[allow(dead_code)] // P1-B1 Rune 用户插件桥消费（检查点对齐后实现）
pub fn install(context: &mut Context, module: Module) -> Result<(), HostError> {
    context.install(module)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rune::vm::ScriptVm;
    use rune::runtime::Value;

    /// 动态安装 async 宿主函数 + 脚本 await 调用（桥的异步调用链基础）。
    #[tokio::test]
    async fn dynamic_async_host_function_callable_from_script() {
        let mut context = Context::with_default_modules().expect("默认模块上下文构建失败");
        let mut m = module();

        /// 宿主函数：把两个数相加后返回（模拟「桥回内核」的 async 形态）。
        async fn add_async(a: i64, b: i64) -> i64 {
            a + b
        }

        fn install_add(m: &mut Module) -> Result<(), HostError> {
            m.function(["add"], add_async).build()?;
            Ok(())
        }
        install_add(&mut m).expect("安装 add 失败");
        install(&mut context, m).expect("合并 context 失败");

        let vm = ScriptVm::compile_with_context(
            r#"
            pub async fn main(a, b) {
                let sum = add(a, b).await;
                sum
            }
            "#,
            &context,
        )
        .expect("脚本编译失败");
        let out = vm
            .call("main", &[Value::from(20i64), Value::from(22i64)])
            .await
            .expect("调用失败");
        assert_eq!(out.as_signed().unwrap(), 42);
    }

    /// 结构性白名单：未安装的函数在编译期就解析失败。
    #[test]
    fn uninstalled_host_function_fails_compile() {
        let context = Context::with_default_modules().expect("默认模块上下文构建失败");
        let err = ScriptVm::compile_with_context(
            r#"
            pub fn main() { not_installed() }
            "#,
            &context,
        )
        .expect_err("未安装函数应编译失败");
        assert!(err.to_string().contains("not_installed"), "{err}");
    }

    /// 类型化安装路径（#[rune::function] + function_meta）也走通。
    #[tokio::test]
    async fn function_meta_installation_works() {
        let mut context = Context::with_default_modules().expect("默认模块上下文构建失败");
        let mut m = module();
        m.function_meta(host_echo).expect("function_meta 安装失败");
        install(&mut context, m).expect("合并 context 失败");

        let vm = ScriptVm::compile_with_context(
            r#"
            pub fn main() { host_echo("hi") }
            "#,
            &context,
        )
        .expect("脚本编译失败");
        let out = vm.call("main", &[]).await.expect("调用失败");
        assert_eq!(&*out.borrow_string_ref().unwrap(), "echo:hi");
    }

    /// 编译期宿主函数（#[rune::function] 宏路径）。
    #[rune::function]
    fn host_echo(s: String) -> String {
        format!("echo:{s}")
    }
}
