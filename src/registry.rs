//! 注册表：启动 fail-fast 校验、两段式契约、懒注册、模型工具列表过滤。

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use schemars::Schema;

use crate::agent::dispatch::{CommandHandler, EventHandler, ToolHandler};
use crate::context::{EntryRegistrar, KernelContext, PluginContext, RegistrarTargets};
use crate::contract::{
    CallerPolicy, Info, LoadPolicy, PluginError, ToolDef, full_name, full_to_wire,
};
use crate::logger::LoggerHandle;
use crate::model::ToolSchema;
use crate::services::{ServiceHandles, ServiceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Tool,
    Command,
    Event,
}

#[derive(Clone)]
pub enum Handler {
    Tool(ToolHandler),
    Command(CommandHandler),
    Event(EventHandler),
}

#[derive(Clone)]
pub struct RegisteredEntry {
    pub full_name: String,
    pub kind: EntryKind,
    pub policy: CallerPolicy,
    pub timeout: Option<Duration>,
    pub description: String,
    pub icon: Option<String>,
    pub params: Schema,
    pub handler: Handler,
}

/// 编译期内置用户插件的静态描述符。
pub struct PluginDescriptor {
    pub info: Info,
    pub register: fn(PluginContext<'_>) -> Result<(), PluginError>,
}

impl PluginDescriptor {
    pub fn from_plugin<P: UserPlugin>() -> Self {
        Self {
            info: P::info(),
            register: P::register,
        }
    }
}

/// 用户插件两段式契约：info 静态元数据，register 绑定 handler。
pub trait UserPlugin {
    fn info() -> Info;
    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError>;
}

/// 编译期内置内核插件描述符。
pub struct KernelDescriptor {
    pub info: Info,
    pub register: fn(KernelContext<'_>) -> Result<(), PluginError>,
}

impl KernelDescriptor {
    pub fn from_plugin<P: KernelPlugin>() -> Self {
        Self {
            info: P::info(),
            register: P::register,
        }
    }
}

/// 内核插件两段式契约：与 UserPlugin 同形（info + register），
/// 但注册上下文注入**全量**服务句柄——内核插件在信任边界内，是服务的提供者。
pub trait KernelPlugin {
    fn info() -> Info;
    fn register(ctx: KernelContext<'_>) -> Result<(), PluginError>;
}

enum PluginBody {
    User(fn(PluginContext<'_>) -> Result<(), PluginError>),
    Kernel(fn(KernelContext<'_>) -> Result<(), PluginError>),
}

struct PluginEntry {
    info: Info,
    body: PluginBody,
    loaded: AtomicBool,
}

pub struct Registry {
    entries: RwLock<HashMap<String, Arc<PluginEntry>>>,
    handlers: RwLock<HashMap<String, RegisteredEntry>>,
    wire_to_full: RwLock<HashMap<String, String>>,
    services: ServiceHandles,
    logger: LoggerHandle,
}

impl Registry {
    pub fn new(services: ServiceHandles, logger: LoggerHandle) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            handlers: RwLock::new(HashMap::new()),
            wire_to_full: RwLock::new(HashMap::new()),
            services,
            logger,
        }
    }

    pub fn logger(&self) -> &LoggerHandle {
        &self.logger
    }

    /// 注册用户插件：启动时 fail-fast 校验。
    pub fn register_plugin(&self, desc: PluginDescriptor) -> Result<(), PluginError> {
        self.register_inner(desc.info, PluginBody::User(desc.register))
    }

    /// 注册内核插件：与用户插件**同一张注册表**校验
    /// （namespace/wire 唯一、CallerPolicy、懒/急加载），但跳过 requires 能力检查——
    /// 内核插件是服务的提供者，register 收到全量句柄；provides 声明服务身份且不得重复。
    pub fn register_kernel_plugin(&self, desc: KernelDescriptor) -> Result<(), PluginError> {
        self.register_inner(desc.info, PluginBody::Kernel(desc.register))
    }

    fn register_inner(&self, info: Info, body: PluginBody) -> Result<(), PluginError> {
        let is_kernel = matches!(&body, PluginBody::Kernel(_));
        {
            let entries = self.entries.read().expect("registry poisoned");
            if entries.contains_key(&info.namespace) {
                return Err(PluginError::NamespaceTaken(info.namespace.clone()));
            }
        }

        if is_kernel {
            // 每个 ServiceId 至多由一个内核插件提供（fail-fast）。
            let mut provided: HashSet<ServiceId> = HashSet::new();
            {
                let entries = self.entries.read().expect("registry poisoned");
                for e in entries.values() {
                    provided.extend(e.info.provides.iter().cloned());
                }
            }
            for id in &info.provides {
                if !provided.insert(id.clone()) {
                    return Err(PluginError::ServiceTaken(id.clone()));
                }
            }
        } else if !info.provides.is_empty() {
            return Err(PluginError::ProvisionNotAllowed(info.provides.clone()));
        }

        if !is_kernel {
            let available = self.services.available();
            let missing: Vec<_> = info
                .requires
                .iter()
                .filter(|r| !available.contains(r))
                .cloned()
                .collect();
            if !missing.is_empty() {
                return Err(PluginError::CapabilityUnavailable(missing));
            }
        }

        let mut fulls: HashSet<String> = HashSet::new();
        let mut wires: HashSet<String> = HashSet::new();
        {
            // 跨已注册插件做 wire 全局唯一检查。
            let entries = self.entries.read().expect("registry poisoned");
            for e in entries.values() {
                for t in &e.info.tools {
                    wires.insert(full_to_wire(&full_name(&e.info.namespace, &t.name)));
                }
                for c in &e.info.commands {
                    wires.insert(full_to_wire(&full_name(&e.info.namespace, &c.name)));
                }
                for ev in &e.info.events {
                    wires.insert(full_to_wire(&full_name(&e.info.namespace, &ev.name)));
                }
            }
        }
        let mut check_name = |short: &str, kind: &str| -> Result<(), PluginError> {
            let full = full_name(&info.namespace, short);
            let wire = full_to_wire(&full);
            if !fulls.insert(full.clone()) {
                return Err(PluginError::DuplicateEntry(full));
            }
            if !wires.insert(wire.clone()) {
                return Err(PluginError::WireNameCollision(format!(
                    "{kind} {short} → {wire}"
                )));
            }
            Ok(())
        };
        for t in &info.tools {
            check_name(&t.name, "工具")?;
        }
        for c in &info.commands {
            check_name(&c.name, "命令")?;
        }
        for e in &info.events {
            check_name(&e.name, "事件")?;
        }

        let eager = matches!(info.load, LoadPolicy::Eager);
        let entry = Arc::new(PluginEntry {
            info,
            body,
            loaded: AtomicBool::new(false),
        });
        self.entries
            .write()
            .expect("registry poisoned")
            .insert(entry.info.namespace.clone(), entry.clone());
        if eager {
            self.load_plugin(&entry.info.namespace)?;
        }
        Ok(())
    }

    /// 懒注册：首次命中某插件任一入口点时调用 register。
    pub fn load_plugin(&self, namespace: &str) -> Result<(), PluginError> {
        let entry = {
            let entries = self.entries.read().expect("registry poisoned");
            entries
                .get(namespace)
                .cloned()
                .ok_or_else(|| PluginError::Internal(format!("未知插件：{namespace}")))?
        };
        if entry.loaded.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let targets = RegistrarTargets {
            handlers: &self.handlers,
            wire_to_full: &self.wire_to_full,
        };
        let registrar = EntryRegistrar::new(namespace, &entry.info, targets);
        let result = match &entry.body {
            PluginBody::User(register) => {
                let ctx = PluginContext {
                    handles: self.services.filter(&entry.info.requires),
                    logger: self.logger.clone(),
                    registrar,
                };
                register(ctx)
            }
            PluginBody::Kernel(register) => {
                let ctx = KernelContext {
                    // 内核插件在信任边界内：注入全量句柄，不做 requires 过滤。
                    handles: self.services.clone(),
                    logger: self.logger.clone(),
                    registrar,
                };
                register(ctx)
            }
        };
        if result.is_err() {
            entry.loaded.store(false, Ordering::SeqCst);
        }
        result
    }

    pub fn ensure_tool(&self, full: &str) -> Result<RegisteredEntry, PluginError> {
        self.ensure(full, EntryKind::Tool)
    }

    pub fn ensure_command(&self, full: &str) -> Result<RegisteredEntry, PluginError> {
        self.ensure(full, EntryKind::Command)
    }

    fn ensure(&self, full: &str, kind: EntryKind) -> Result<RegisteredEntry, PluginError> {
        if !self
            .handlers
            .read()
            .expect("registry poisoned")
            .contains_key(full)
        {
            let ns = full.split("::").next().unwrap_or_default().to_string();
            self.load_plugin(&ns)?;
        }
        let entry = self
            .handlers
            .read()
            .expect("registry poisoned")
            .get(full)
            .cloned();
        match entry {
            Some(e) if e.kind == kind => Ok(e),
            _ => Err(PluginError::Internal(format!("入口点不存在：{full}"))),
        }
    }

    pub fn resolve_wire(&self, wire: &str) -> Option<String> {
        {
            let map = self.wire_to_full.read().expect("registry poisoned");
            if let Some(full) = map.get(wire) {
                return Some(full.clone());
            }
        }
        // 懒插件尚未注册：按 info 声明的 wire 名反查命中 → 触发懒加载后再查。
        // 模型只能拿到声明过的工具，wire 名不会凭空出现。
        let namespace = {
            let entries = self.entries.read().expect("registry poisoned");
            entries.values().find_map(|e| {
                let hit =
                    e.info
                        .tools
                        .iter()
                        .any(|t| full_to_wire(&full_name(&e.info.namespace, &t.name)) == wire)
                        || e.info
                            .commands
                            .iter()
                            .any(|c| full_to_wire(&full_name(&e.info.namespace, &c.name)) == wire)
                        || e.info.events.iter().any(|ev| {
                            full_to_wire(&full_name(&e.info.namespace, &ev.name)) == wire
                        });
                hit.then(|| e.info.namespace.clone())
            })
        }?;
        let _ = self.load_plugin(&namespace);
        self.wire_to_full
            .read()
            .expect("registry poisoned")
            .get(wire)
            .cloned()
    }

    /// 入口点图标（Iconify 名，GUI 展示用）。
    pub fn entry_icon(&self, full_name: &str) -> Option<String> {
        self.handlers
            .read()
            .expect("registry poisoned")
            .get(full_name)
            .and_then(|e| e.icon.clone())
    }

    /// 入口点展示标题（用户友好名；无 title 时回退短名；找不到返回 None）。
    pub fn entry_title(&self, full: &str) -> Option<String> {
        let entries = self.entries.read().expect("registry poisoned");
        for e in entries.values() {
            let ns = &e.info.namespace;
            for t in &e.info.tools {
                if full_name(ns, &t.name) == full {
                    return Some(t.title.clone().unwrap_or_else(|| t.name.clone()));
                }
            }
            for c in &e.info.commands {
                if full_name(ns, &c.name) == full {
                    return Some(c.title.clone().unwrap_or_else(|| c.name.clone()));
                }
            }
        }
        None
    }

    /// 模型工具列表：只含 UserAndModel 工具，名字为 wire name。
    /// 读 info 声明（含未加载的懒插件）：模型第一轮即可见全部声明工具，
    /// 调用时经 resolve_wire / ensure_tool 触发懒加载。
    pub fn model_tools(&self) -> Vec<ToolSchema> {
        let entries = self.entries.read().expect("registry poisoned");
        let mut out = Vec::new();
        for e in entries.values() {
            for t in &e.info.tools {
                if t.policy == CallerPolicy::UserAndModel {
                    out.push(ToolSchema {
                        name: full_to_wire(&full_name(&e.info.namespace, &t.name)),
                        description: t.description.clone(),
                        input_schema: serde_json::to_value(&t.params).unwrap_or_default(),
                    });
                }
            }
        }
        out
    }

    pub fn namespaces(&self) -> Vec<String> {
        self.entries
            .read()
            .expect("registry poisoned")
            .keys()
            .cloned()
            .collect()
    }

    /// 用户可调入口点清单（Tool/Command，含懒加载插件的声明），GUI 工具面板用。
    pub fn user_entries(&self) -> Vec<serde_json::Value> {
        let entries = self.entries.read().expect("registry poisoned");
        let mut out = Vec::new();
        for entry in entries.values() {
            let ns = &entry.info.namespace;
            for t in &entry.info.tools {
                if !t.user_visible {
                    continue;
                }
                out.push(serde_json::json!({
                    "entry": full_name(ns, &t.name),
                    "kind": "tool",
                    "title": t.title,
                    "group": t.group,
                    "policy": t.policy,
                    "description": t.description,
                    "icon": t.icon,
                    "params": t.params,
                }));
            }
            for c in &entry.info.commands {
                if !c.user_visible {
                    continue;
                }
                out.push(serde_json::json!({
                    "entry": full_name(ns, &c.name),
                    "kind": "command",
                    "title": c.title,
                    "group": c.group,
                    "policy": CallerPolicy::UserOnly,
                    "description": c.description,
                    "icon": c.icon,
                    "params": c.params,
                }));
            }
        }
        out
    }
}

pub fn tool_def(name: &str, description: &str, policy: CallerPolicy) -> ToolDef {
    ToolDef {
        name: name.into(),
        user_visible: true,
        title: None,
        group: None,
        description: description.into(),
        params: crate::contract::empty_params(),
        policy,
        timeout: None,
        icon: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::dispatch::ToolCallContext;
    use crate::logger::Logger;
    use serde_json::{Value, json};

    fn logger() -> LoggerHandle {
        Arc::new(Logger)
    }

    #[test]
    fn duplicate_namespace_rejected() {
        let registry = Registry::new(ServiceHandles::default(), logger());
        let desc = PluginDescriptor {
            info: Info {
                namespace: "demo".into(),
                ..Default::default()
            },
            register: |_| Ok(()),
        };
        registry.register_plugin(desc).unwrap();
        let desc2 = PluginDescriptor {
            info: Info {
                namespace: "demo".into(),
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
    fn wire_name_mapping_separates_single_underscores() {
        // 双下划线映射：a::b_c → a__b_c，a_b::c → a_b__c，不再撞名。
        assert_eq!(full_to_wire("a::b_c"), "a__b_c");
        assert_eq!(full_to_wire("a_b::c"), "a_b__c");

        let registry = Registry::new(ServiceHandles::default(), logger());
        let desc = PluginDescriptor {
            info: Info {
                namespace: "a".into(),
                tools: vec![tool_def("b_c", "t1", CallerPolicy::UserAndModel)],
                ..Default::default()
            },
            register: |_| Ok(()),
        };
        registry.register_plugin(desc).unwrap();
        let desc2 = PluginDescriptor {
            info: Info {
                namespace: "a_b".into(),
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
                tools: vec![tool_def("b__c", "t1", CallerPolicy::UserAndModel)],
                ..Default::default()
            },
            register: |_| Ok(()),
        };
        registry.register_plugin(desc).unwrap();
        let desc2 = PluginDescriptor {
            info: Info {
                namespace: "a__b".into(),
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
        let registry = Registry::new(ServiceHandles::default(), logger());
        let desc = PluginDescriptor {
            info: Info {
                namespace: "demo".into(),
                tools: vec![tool_def("hello", "打招呼", CallerPolicy::UserAndModel)],
                ..Default::default()
            },
            register: |ctx| {
                ctx.registrar.tool(
                    "hello",
                    Arc::new(|_ctx: &ToolCallContext, _p: Value| {
                        Box::pin(async move { Ok(json!({"reply": "hi"})) })
                    }),
                )
            },
        };
        registry.register_plugin(desc).unwrap();
        assert!(registry.handlers.read().unwrap().is_empty());
        let entry = registry.ensure_tool("demo::hello").unwrap();
        assert_eq!(entry.full_name, "demo::hello");
        assert_eq!(registry.model_tools().len(), 1);
        assert_eq!(registry.model_tools()[0].name, "demo__hello");
        // 重复注册被拒
        let desc2 = PluginDescriptor {
            info: Info {
                namespace: "demo".into(),
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
        let registry = Registry::new(ServiceHandles::default(), logger());
        let desc = PluginDescriptor {
            info: Info {
                namespace: "demo".into(),
                tools: vec![tool_def("hello", "x", CallerPolicy::UserAndModel)],
                ..Default::default()
            },
            register: |ctx| {
                ctx.registrar.tool(
                    "hello",
                    Arc::new(|_ctx: &ToolCallContext, _p: Value| {
                        Box::pin(async move { Ok(json!({"reply": "hi"})) })
                    }),
                )
            },
        };
        registry.register_plugin(desc).unwrap();
        assert!(registry.handlers.read().unwrap().is_empty());
        assert_eq!(registry.model_tools().len(), 1);
        assert_eq!(registry.model_tools()[0].name, "demo__hello");
        let full = registry
            .resolve_wire("demo__hello")
            .expect("懒加载后应可解析");
        assert_eq!(full, "demo::hello");
        assert_eq!(registry.handlers.read().unwrap().len(), 1);
    }

    #[test]
    fn user_entries_filter_invisible() {
        let registry = Registry::new(ServiceHandles::default(), logger());
        let desc = PluginDescriptor {
            info: Info {
                namespace: "demo".into(),
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
        let registry = Registry::new(ServiceHandles::default(), logger());
        let desc = KernelDescriptor {
            info: Info {
                namespace: "kernel_demo".into(),
                provides: vec![ServiceId::custom("memory")],
                tools: vec![tool_def("ping", "内核工具", CallerPolicy::UserAndModel)],
                ..Default::default()
            },
            register: |ctx| {
                ctx.registrar.tool(
                    "ping",
                    Arc::new(|_ctx: &ToolCallContext, _p: Value| {
                        Box::pin(async move { Ok(json!({"kernel": true})) })
                    }),
                )
            },
        };
        registry.register_kernel_plugin(desc).unwrap();
        assert!(registry.handlers.read().unwrap().is_empty());
        let entry = registry.ensure_tool("kernel_demo::ping").unwrap();
        assert_eq!(entry.full_name, "kernel_demo::ping");
        assert_eq!(registry.model_tools().len(), 1);
        assert_eq!(registry.model_tools()[0].name, "kernel_demo__ping");
    }

    #[test]
    fn duplicate_service_provision_rejected() {
        let registry = Registry::new(ServiceHandles::default(), logger());
        let desc = KernelDescriptor {
            info: Info {
                namespace: "a".into(),
                provides: vec![ServiceId::custom("memory")],
                ..Default::default()
            },
            register: |_| Ok(()),
        };
        registry.register_kernel_plugin(desc).unwrap();
        let desc2 = KernelDescriptor {
            info: Info {
                namespace: "b".into(),
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
                tools: vec![tool_def("b__c", "t1", CallerPolicy::UserAndModel)],
                ..Default::default()
            },
            register: |_| Ok(()),
        };
        registry.register_kernel_plugin(kernel).unwrap();
        let user = PluginDescriptor {
            info: Info {
                namespace: "a__b".into(),
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
}
