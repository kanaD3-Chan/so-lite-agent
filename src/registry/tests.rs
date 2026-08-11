use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::agent::dispatch::ToolCallContext;
use crate::contract::{CallerPolicy, Info, PluginError, ToolDef, full_to_wire};
use crate::logger::{Logger, LoggerHandle};
use crate::services::{ServiceHandles, ServiceId};
use serde_json::{Value, json};

static LOADED: AtomicBool = AtomicBool::new(false);
static LOADED_WIRE: AtomicBool = AtomicBool::new(false);
static LOADED_KERNEL: AtomicBool = AtomicBool::new(false);

fn logger() -> LoggerHandle {
    Arc::new(Logger)
}

#[test]
fn duplicate_namespace_rejected() {
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = PluginDescriptor {
        info: Info {
            namespace: "demo".into(),
            enabled: true,
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    registry.register_plugin(desc).unwrap();
    let desc2 = PluginDescriptor {
        info: Info {
            namespace: "demo".into(),
            enabled: true,
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    assert!(matches!(
        registry.register_plugin(desc2),
        Err(PluginError::NamespaceTaken(_))
    ));
}

#[test]
fn disabled_plugin_skipped_by_default() {
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = PluginDescriptor {
        info: Info {
            namespace: "demo".into(),
            tools: vec![tool_def("hello", "x", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    // enabled 缺省 false：注册被静默跳过，不占 namespace、不进模型列表。
    registry.register_plugin(desc).unwrap();
    assert!(registry.namespaces().is_empty());
    assert!(registry.model_tools().is_empty());
}

#[test]
fn wire_name_mapping_separates_single_underscores() {
    // 双下划线映射：a::b_c → a__b_c，a_b::c → a_b__c，不再撞名。
    assert_eq!(full_to_wire("a::b_c"), "a__b_c");
    assert_eq!(full_to_wire("a_b::c"), "a_b__c");

    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = PluginDescriptor {
        info: Info {
            namespace: "a".into(),
            enabled: true,
            tools: vec![tool_def("b_c", "t1", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    registry.register_plugin(desc).unwrap();
    let desc2 = PluginDescriptor {
        info: Info {
            namespace: "a_b".into(),
            enabled: true,
            tools: vec![tool_def("c", "t2", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    registry.register_plugin(desc2).unwrap();
}

#[test]
fn double_underscore_wire_collision_rejected() {
    // 病态组合仍撞：a::b__c → a__b__c 与 a__b::c → a__b__c，注册期全局校验兜底。
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = PluginDescriptor {
        info: Info {
            namespace: "a".into(),
            enabled: true,
            tools: vec![tool_def("b__c", "t1", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    registry.register_plugin(desc).unwrap();
    let desc2 = PluginDescriptor {
        info: Info {
            namespace: "a__b".into(),
            enabled: true,
            tools: vec![tool_def("c", "t2", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    assert!(matches!(
        registry.register_plugin(desc2),
        Err(PluginError::WireNameCollision(_))
    ));
}

#[test]
fn requires_must_be_satisfiable() {
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = PluginDescriptor {
        info: Info {
            namespace: "demo".into(),
            enabled: true,
            requires: vec![ServiceId::custom("model")],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    assert!(matches!(
        registry.register_plugin(desc),
        Err(PluginError::CapabilityUnavailable(_))
    ));
}

#[test]
fn lazy_registration_on_first_use() {
    LOADED.store(false, Ordering::SeqCst);
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = PluginDescriptor {
        info: Info {
            namespace: "demo".into(),
            enabled: true,
            tools: vec![tool_def("hello", "打招呼", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |ctx| {
            LOADED.store(true, Ordering::SeqCst);
            ctx.registrar.tool(
                "hello",
                Arc::new(|_ctx: &ToolCallContext, _p: Value| {
                    Box::pin(async move { Ok(json!({"reply": "hi"})) })
                }),
            )
        },
    };
    registry.register_plugin(desc).unwrap();
    assert!(!LOADED.load(Ordering::SeqCst), "懒插件注册时不加载");
    assert_eq!(registry.model_tools().len(), 1);
    assert_eq!(registry.model_tools()[0].name, "demo__hello");
    let entry = registry.ensure_tool("demo::hello").unwrap();
    assert_eq!(entry.full_name, "demo::hello");
    assert!(LOADED.load(Ordering::SeqCst), "首次命中应触发 register");
    // 重复注册被拒
    let desc2 = PluginDescriptor {
        info: Info {
            namespace: "demo".into(),
            enabled: true,
            tools: vec![tool_def("hello", "x", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    assert!(matches!(
        registry.register_plugin(desc2),
        Err(PluginError::NamespaceTaken(_))
    ));
}

#[test]
fn lazy_wire_resolution_loads_plugin_on_first_call() {
    // 模型走 wire 名：未加载插件也能被解析（按 info 声明反查 → 懒加载）。
    LOADED_WIRE.store(false, Ordering::SeqCst);
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = PluginDescriptor {
        info: Info {
            namespace: "demo".into(),
            enabled: true,
            tools: vec![tool_def("hello", "x", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |ctx| {
            LOADED_WIRE.store(true, Ordering::SeqCst);
            ctx.registrar.tool(
                "hello",
                Arc::new(|_ctx: &ToolCallContext, _p: Value| {
                    Box::pin(async move { Ok(json!({"reply": "hi"})) })
                }),
            )
        },
    };
    registry.register_plugin(desc).unwrap();
    assert!(!LOADED_WIRE.load(Ordering::SeqCst));
    assert_eq!(registry.model_tools().len(), 1);
    assert_eq!(registry.model_tools()[0].name, "demo__hello");
    let full = registry
        .resolve_wire("demo__hello")
        .expect("懒加载后应可解析");
    assert_eq!(full, "demo::hello");
    assert!(LOADED_WIRE.load(Ordering::SeqCst), "wire 反查应触发懒加载");
}

#[test]
fn user_entries_filter_invisible() {
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = PluginDescriptor {
        info: Info {
            namespace: "demo".into(),
            enabled: true,
            tools: vec![
                ToolDef {
                    name: "hidden".into(),
                    user_visible: false,
                    title: None,
                    group: None,
                    description: "模型专用".into(),
                    params: crate::contract::empty_params(),
                    policy: CallerPolicy::UserAndModel,
                    timeout: None,
                    icon: None,
                },
                ToolDef {
                    name: "shown".into(),
                    user_visible: true,
                    title: Some("可见工具".into()),
                    group: Some("测试".into()),
                    description: "用户可用".into(),
                    params: crate::contract::empty_params(),
                    policy: CallerPolicy::UserAndModel,
                    timeout: None,
                    icon: None,
                },
            ],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    registry.register_plugin(desc).unwrap();
    let entries = registry.user_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["entry"], "demo::shown");
    assert_eq!(entries[0]["title"], "可见工具");
    assert_eq!(entries[0]["group"], "测试");
}

#[test]
fn kernel_plugin_lazy_registration_binds_handler() {
    LOADED_KERNEL.store(false, Ordering::SeqCst);
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = KernelDescriptor {
        info: Info {
            namespace: "kernel_demo".into(),
            enabled: true,
            provides: vec![ServiceId::custom("memory")],
            tools: vec![tool_def("ping", "内核工具", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |ctx| {
            LOADED_KERNEL.store(true, Ordering::SeqCst);
            ctx.registrar.tool(
                "ping",
                Arc::new(|_ctx: &ToolCallContext, _p: Value| {
                    Box::pin(async move { Ok(json!({"kernel": true})) })
                }),
            )
        },
    };
    registry.register_kernel_plugin(desc).unwrap();
    assert!(
        !LOADED_KERNEL.load(Ordering::SeqCst),
        "内核插件注册时不加载"
    );
    let entry = registry.ensure_tool("kernel_demo::ping").unwrap();
    assert_eq!(entry.full_name, "kernel_demo::ping");
    assert!(
        LOADED_KERNEL.load(Ordering::SeqCst),
        "首次命中应触发 register"
    );
    assert_eq!(registry.model_tools().len(), 1);
    assert_eq!(registry.model_tools()[0].name, "kernel_demo__ping");
}

#[test]
fn duplicate_service_provision_rejected() {
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = KernelDescriptor {
        info: Info {
            namespace: "a".into(),
            enabled: true,
            provides: vec![ServiceId::custom("memory")],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    registry.register_kernel_plugin(desc).unwrap();
    let desc2 = KernelDescriptor {
        info: Info {
            namespace: "b".into(),
            enabled: true,
            provides: vec![ServiceId::custom("memory")],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    assert!(matches!(
        registry.register_kernel_plugin(desc2),
        Err(PluginError::ServiceTaken(_))
    ));
}

#[test]
fn user_plugin_cannot_declare_provides() {
    let registry = Registry::new(ServiceHandles::default(), logger());
    let desc = PluginDescriptor {
        info: Info {
            namespace: "demo".into(),
            enabled: true,
            provides: vec![ServiceId::custom("storage")],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    assert!(matches!(
        registry.register_plugin(desc),
        Err(PluginError::ProvisionNotAllowed(_))
    ));
}

#[test]
fn kernel_and_user_wire_collision_rejected() {
    let registry = Registry::new(ServiceHandles::default(), logger());
    let kernel = KernelDescriptor {
        info: Info {
            namespace: "a".into(),
            enabled: true,
            tools: vec![tool_def("b__c", "t1", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    registry.register_kernel_plugin(kernel).unwrap();
    let user = PluginDescriptor {
        info: Info {
            namespace: "a__b".into(),
            enabled: true,
            tools: vec![tool_def("c", "t2", CallerPolicy::UserAndModel)],
            ..Default::default()
        },
        register: |_| Ok(()),
    };
    assert!(matches!(
        registry.register_plugin(user),
        Err(PluginError::WireNameCollision(_))
    ));
}
