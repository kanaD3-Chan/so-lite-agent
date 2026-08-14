//! 插件注册上下文：两段式契约第二阶段（注入句柄 + 绑定 handler）。

use std::collections::HashMap;
use std::sync::RwLock;
use std::time::Duration;

use crate::agent::dispatch::{CommandHandler, EventHandler, ToolHandler};
use crate::contract::{CallerPolicy, Info, PluginError, full_name, full_to_wire};
use crate::logger::LoggerHandle;
use crate::registry::{EntryKind, Handler, RegisteredEntry};
use crate::services::ServiceHandles;

/// 注册目标：kernel 注册表内部结构，只经 EntryRegistrar 暴露受限写入。
pub struct RegistrarTargets<'a> {
    pub handlers: &'a RwLock<HashMap<String, RegisteredEntry>>,
    pub wire_to_full: &'a RwLock<HashMap<String, String>>,
}

/// 校验短名在声明内 + 构造 RegisteredEntry + 插入注册表。
/// Rust 插件（[`EntryRegistrar`]）与 Rune 脚本桥共用同一套绑定规则
/// （声明与实现一致、wire 全局唯一，ADR-0006）。
pub(crate) fn bind_tool(
    namespace: &str,
    declared: &Info,
    short: &str,
    handler: ToolHandler,
    handlers: &RwLock<HashMap<String, RegisteredEntry>>,
    wire_to_full: &RwLock<HashMap<String, String>>,
) -> Result<(), PluginError> {
    let def = declared
        .tools
        .iter()
        .find(|t| t.name == short)
        .ok_or_else(|| PluginError::UndeclaredEntry(short.into()))?;
    let full = full_name(namespace, short);
    let entry = RegisteredEntry {
        full_name: full.clone(),
        kind: EntryKind::Tool,
        policy: def.policy,
        timeout: def.timeout.map(Duration::from_secs),
        description: def.description.clone(),
        icon: def.icon.clone(),
        params: def.params.clone(),
        handler: Handler::Tool(handler),
    };
    insert_entry(full, entry, handlers, wire_to_full)
}

pub(crate) fn bind_command(
    namespace: &str,
    declared: &Info,
    short: &str,
    handler: CommandHandler,
    handlers: &RwLock<HashMap<String, RegisteredEntry>>,
    wire_to_full: &RwLock<HashMap<String, String>>,
) -> Result<(), PluginError> {
    let def = declared
        .commands
        .iter()
        .find(|c| c.name == short)
        .ok_or_else(|| PluginError::UndeclaredEntry(short.into()))?;
    let full = full_name(namespace, short);
    let entry = RegisteredEntry {
        full_name: full.clone(),
        kind: EntryKind::Command,
        // 命令恒为 UserOnly：结构上不给 policy 字段。
        policy: CallerPolicy::UserOnly,
        timeout: None,
        description: def.description.clone(),
        icon: def.icon.clone(),
        params: def.params.clone(),
        handler: Handler::Command(handler),
    };
    insert_entry(full, entry, handlers, wire_to_full)
}

pub(crate) fn bind_event(
    namespace: &str,
    declared: &Info,
    name: &str,
    handler: EventHandler,
    handlers: &RwLock<HashMap<String, RegisteredEntry>>,
    wire_to_full: &RwLock<HashMap<String, String>>,
) -> Result<(), PluginError> {
    if !declared.events.iter().any(|e| e.name == name) {
        return Err(PluginError::UndeclaredEntry(name.into()));
    }
    let full = full_name(namespace, name);
    let entry = RegisteredEntry {
        full_name: full.clone(),
        kind: EntryKind::Event,
        policy: CallerPolicy::UserOnly,
        timeout: None,
        description: String::new(),
        icon: None,
        params: crate::contract::empty_params(),
        handler: Handler::Event(handler),
    };
    insert_entry(full, entry, handlers, wire_to_full)
}

fn insert_entry(
    full: String,
    entry: RegisteredEntry,
    handlers: &RwLock<HashMap<String, RegisteredEntry>>,
    wire_to_full: &RwLock<HashMap<String, String>>,
) -> Result<(), PluginError> {
    let mut h = handlers.write().expect("registry poisoned");
    if h.contains_key(&full) {
        return Err(PluginError::DuplicateEntry(full));
    }
    h.insert(full.clone(), entry);
    wire_to_full
        .write()
        .expect("registry poisoned")
        .insert(full_to_wire(&full), full);
    Ok(())
}

/// 只允许登记 info 中声明过的短名（声明与实现一致）。
pub struct EntryRegistrar<'a> {
    namespace: &'a str,
    declared: &'a Info,
    targets: RegistrarTargets<'a>,
}

impl<'a> EntryRegistrar<'a> {
    pub fn new(namespace: &'a str, declared: &'a Info, targets: RegistrarTargets<'a>) -> Self {
        Self {
            namespace,
            declared,
            targets,
        }
    }

    pub fn tool(&self, short: &str, handler: ToolHandler) -> Result<(), PluginError> {
        bind_tool(
            self.namespace,
            self.declared,
            short,
            handler,
            self.targets.handlers,
            self.targets.wire_to_full,
        )
    }

    pub fn command(&self, short: &str, handler: CommandHandler) -> Result<(), PluginError> {
        bind_command(
            self.namespace,
            self.declared,
            short,
            handler,
            self.targets.handlers,
            self.targets.wire_to_full,
        )
    }

    pub fn event(&self, name: &str, handler: EventHandler) -> Result<(), PluginError> {
        bind_event(
            self.namespace,
            self.declared,
            name,
            handler,
            self.targets.handlers,
            self.targets.wire_to_full,
        )
    }
}

/// 插件注册上下文（两段式第二阶段）。
pub struct PluginContext<'a> {
    pub handles: ServiceHandles,
    pub logger: LoggerHandle,
    pub registrar: EntryRegistrar<'a>,
}

/// 内核插件注册上下文：与 PluginContext 同形，但注入**全量**服务句柄——
/// 内核插件在信任边界内，不按 requires 过滤；requires 对内核插件无意义。
pub struct KernelContext<'a> {
    pub handles: ServiceHandles,
    pub logger: LoggerHandle,
    pub registrar: EntryRegistrar<'a>,
}
