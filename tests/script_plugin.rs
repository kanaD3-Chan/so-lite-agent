//! Rune 脚本用户插件桥端到端（ADR-0006）：manifest 声明 + register 绑定 + requires 白名单。
//! 需要 `--features rune-plugins`。

#![cfg(feature = "rune-plugins")]

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use so_lite_agent::builder::KernelBuilder;
use so_lite_agent::contract::{CallerPolicy, Info, PluginError, ToolDef, ToolError};
use so_lite_agent::events::{Event, MemoryEventSink};
use so_lite_agent::rune::{ScriptPlugin, ScriptPluginHandle};
use so_lite_agent::services::{DynamicService, ServiceHandles, ServiceId};

fn tool_def(name: &str, description: &str) -> ToolDef {
    ToolDef {
        name: name.into(),
        user_visible: true,
        title: None,
        group: None,
        description: description.into(),
        params: schemars::json_schema!({"type": "object"}),
        policy: CallerPolicy::UserAndModel,
        timeout: None,
        icon: None,
    }
}

const PING_SCRIPT: &str = r#"
pub fn register() {
    tool("ping", handle_ping)
}

fn handle_ping(params) {
    emit_event("demo.ping", params);
    #{ "pong": params }
}
"#;

fn ping_plugin() -> ScriptPlugin {
    ScriptPlugin {
        manifest: Info {
            namespace: "demo".into(),
            enabled: true,
            requires: Vec::new(),
            tools: vec![tool_def("ping", "回显参数")],
            ..Default::default()
        },
        script: PING_SCRIPT.to_string(),
    }
}

fn make_kernel(plugin: ScriptPlugin) -> Result<so_lite_agent::builder::Kernel, PluginError> {
    let kernel = KernelBuilder::new().script_plugin(plugin).build()?;
    Ok(kernel)
}

#[tokio::test]
async fn script_tool_registers_and_calls_end_to_end() {
    let events = Arc::new(MemoryEventSink::default());
    let kernel = KernelBuilder::new()
        .event_sink(events.clone())
        .script_plugin(ping_plugin())
        .build()
        .expect("build 应成功");

    let out = kernel
        .call_command("demo::ping", json!({"hello": 1}))
        .await
        .expect("工具调用应成功");
    assert_eq!(out["pong"]["hello"], 1);

    // 脚本 emit_event → Event::Custom 上浮。
    let evs = events.take();
    assert!(
        evs.iter()
            .any(|e| matches!(e, Event::Custom { name, .. } if name == "demo.ping")),
        "应收到 demo.ping 自定义事件：{evs:?}"
    );
}

#[tokio::test]
async fn disabled_script_plugin_is_skipped() {
    let mut plugin = ping_plugin();
    plugin.manifest.enabled = false;
    let kernel = make_kernel(plugin).expect("未启用插件应静默跳过");
    let err = kernel.call_command("demo::ping", json!({})).await;
    assert!(err.is_err(), "未启用插件的工具不应可调");
}

#[tokio::test]
async fn requires_session_gives_session_host_functions() {
    let plugin = ScriptPlugin {
        manifest: Info {
            namespace: "demo".into(),
            enabled: true,
            requires: vec![ServiceId::session()],
            tools: vec![tool_def("count", "会话数")],
            ..Default::default()
        },
        script: r#"
pub fn register() {
    tool("count", handle_count)
}

async fn handle_count(params) {
    let sessions = session_list().await;
    #{ "count": sessions.len() }
}
"#
        .to_string(),
    };
    let kernel = make_kernel(plugin).expect("build 应成功");
    let out = kernel
        .call_command("demo::count", json!({}))
        .await
        .expect("工具调用应成功");
    assert_eq!(out["count"], 0, "初始无会话");
}

struct NoteDynamic;

#[async_trait]
impl DynamicService for NoteDynamic {
    async fn call(&self, method: &str, params: Value) -> Result<Value, ToolError> {
        match method {
            "count" => Ok(json!({"count": 3, "echo": params})),
            _ => Err(ToolError::handler(format!("未知方法：{method}"))),
        }
    }
}

#[tokio::test]
async fn requires_custom_with_dynamic_service_works() {
    let plugin = ScriptPlugin {
        manifest: Info {
            namespace: "demo".into(),
            enabled: true,
            requires: vec![ServiceId::custom("notes")],
            tools: vec![tool_def("notes", "查笔记")],
            ..Default::default()
        },
        script: r#"
pub fn register() {
    tool("notes", handle_notes)
}

async fn handle_notes(params) {
    call_notes("count", #{ "q": "rust" }).await
}
"#
        .to_string(),
    };
    let handles =
        ServiceHandles::default().with_dynamic(ServiceId::custom("notes"), Arc::new(NoteDynamic));
    let kernel = KernelBuilder::new()
        .service_handles(handles)
        .script_plugin(plugin)
        .build()
        .expect("build 应成功");
    let out = kernel
        .call_command("demo::notes", json!({}))
        .await
        .expect("工具调用应成功");
    assert_eq!(out["count"], 3);
    assert_eq!(out["echo"]["q"], "rust");
}

#[tokio::test]
async fn requires_custom_without_dynamic_fails_fast() {
    let plugin = ping_plugin();
    let mut plugin = plugin;
    plugin.manifest.requires = vec![ServiceId::custom("notes")];
    // 服务存在但未实现 DynamicService：build 必须 fail-fast。
    struct NotDynamic;
    let handles =
        ServiceHandles::default().with_custom(ServiceId::custom("notes"), Arc::new(NotDynamic));
    let err = KernelBuilder::new()
        .service_handles(handles)
        .script_plugin(plugin)
        .build()
        .err()
        .expect("未实现 DynamicService 应注册失败");
    assert!(
        format!("{err:?}").contains("DynamicService"),
        "错误应点名 DynamicService：{err:?}"
    );
}

#[tokio::test]
async fn uninstalled_host_function_fails_compile() {
    let plugin = ScriptPlugin {
        manifest: Info {
            namespace: "demo".into(),
            enabled: true,
            requires: vec![ServiceId::session()],
            ..Default::default()
        },
        // 声明只给了 session，却调用未安装的 call_notes → prepare 编译失败。
        script: r#"
pub fn register() { tool("notes", handle_notes) }
fn handle_notes(params) { call_notes("count", #{}) }
"#
        .to_string(),
    };
    let err = make_kernel(plugin).err().expect("未安装函数应编译失败");
    assert!(
        format!("{err:?}").contains("call_notes"),
        "错误应点名未安装函数：{err}"
    );
}

#[tokio::test]
async fn unbound_declared_tool_fails_at_load() {
    let plugin = ScriptPlugin {
        manifest: Info {
            namespace: "demo".into(),
            enabled: true,
            tools: vec![tool_def("ghost", "声明但未绑定")],
            ..Default::default()
        },
        script: r#"
pub fn register() {
    // 声明了 ghost 却不绑定
}
"#
        .to_string(),
    };
    let kernel = make_kernel(plugin).expect("build 应成功（绑定校验在懒加载）");
    let err = kernel
        .call_command("demo::ghost", json!({}))
        .await
        .expect_err("未绑定声明的工具应加载失败");
    assert!(
        format!("{err:?}").contains("ghost"),
        "错误应点名未绑定的工具：{err:?}"
    );
}

#[test]
fn requires_model_fails_fast() {
    let mut plugin = ping_plugin();
    plugin.manifest.requires = vec![ServiceId::model()];
    let err = make_kernel(plugin)
        .err()
        .expect("脚本 requires model 应被拒绝");
    assert!(format!("{err:?}").contains("model"), "{err}");
}

#[test]
fn from_dir_rejects_namespace_mismatch() {
    let dir = std::env::temp_dir().join(format!("sl-agent-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(dir.join("other_name")).unwrap();
    std::fs::write(
        dir.join("other_name/manifest.json"),
        json!({"namespace": "demo"}).to_string(),
    )
    .unwrap();
    std::fs::write(dir.join("other_name/plugin.rn"), "").unwrap();
    let err = ScriptPlugin::from_dir(&dir.join("other_name"))
        .err()
        .expect("目录名与 namespace 不一致应报错");
    assert!(format!("{err:?}").contains("不一致"), "{err}");
    std::fs::remove_dir_all(&dir).unwrap();
}

// ScriptPluginHandle 的构造签名冒烟（防止公共面漂移）。
#[allow(dead_code)]
fn _handle_signature_smoke(
    plugin: ScriptPlugin,
    services: &ServiceHandles,
    events: Arc<dyn so_lite_agent::events::EventSink>,
    logger: so_lite_agent::logger::LoggerHandle,
    handlers: Arc<
        std::sync::RwLock<
            std::collections::HashMap<String, so_lite_agent::registry::RegisteredEntry>,
        >,
    >,
    wire: Arc<std::sync::RwLock<std::collections::HashMap<String, String>>>,
) -> Result<ScriptPluginHandle, PluginError> {
    ScriptPluginHandle::new(plugin, services, events, logger, handlers, wire)
}
