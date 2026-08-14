//! Rune 用户插件桥（ADR-0006）：manifest 声明 + 脚本 register() 绑定 + requires 白名单。
//!
//! 目录形态（P1 检查点结论 c2）：一插件一目录——`manifest.json`（info 结构化声明，
//! 纯数据不执行）+ `plugin.rn`（register() + handlers）。流程：
//!
//! 1. [`ScriptPlugin::from_dir`] 读 manifest → [`Info`]（与 Rust 插件同构校验）；
//! 2. [`ScriptPluginHandle::new`] requires 预检（fail-fast），把白名单规格交给**专用
//!    执行线程**：线程上构造宿主模块 → 编译（fail-fast 握手）→ 消息循环；
//! 3. 懒加载时经通道执行脚本 `register()`（同步绑定）：宿主函数 `tool/command/event`
//!    把脚本函数包装为 ToolHandler 写入注册表（与 Rust 路径同一绑定规则）；
//! 4. 绑定后校验：声明过的入口点必须全部绑定（fail-fast，防脚本静默漏绑）。
//!
//! **执行线程模型**：rune 0.14 的 `Value`/`Function` 不是 `Send`（无 sync feature），
//! 而工具 handler 的 future 必须 `Send`（dispatch 用 `tokio::spawn`）。因此每个脚本
//! 插件持有一条专用执行线程（current-thread tokio runtime 驱动）：rune VM、Context、
//! 脚本函数绑定（`Rc<RefCell>`）全部留在该线程；线程间只传 Send 的 JSON + 通道消息
//! （`mpsc` + `oneshot`）。内核 drop 后通道关闭，线程自然退出。P1 规模可接受。
//!
//! 宿主函数面（eBPF helper 白名单）：
//! - 恒有：`tool` / `command` / `event`（绑定）、`emit_event` / `progress` / `log`（播报）；
//! - `requires: [session]`：`session_list` / `session_read(key)`（异步）；
//! - `requires: [custom(id)]`：`call_<sanitized_id>(method, params)`（异步，
//!   服务必须实现 [`DynamicService`]）；
//! - `requires: [model]`：P1 不支持，注册期 fail-fast。

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use rune::Module;
use rune::compile::Context;
use rune::runtime::{Function as RuneFunction, Value as RuneValue, VmResult};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::sync::oneshot;

use crate::agent::dispatch::{EventHandler, ToolCallContext, ToolHandler};
use crate::agent::session::SessionKey;
use crate::context::{bind_command, bind_event, bind_tool};
use crate::contract::{Info, PluginError, ToolError};
use crate::events::{Event, EventSink};
use crate::logger::{Level, LoggerHandle};
use crate::registry::RegisteredEntry;
use crate::services::{DynamicService, ServiceHandles, ServiceId};

use super::host::{HostError, install, module};
use super::vm::{ScriptVm, json_from_value, value_from_json};

// 脚本函数绑定表：**执行线程本地**（thread_local）。
// 宿主函数闭包必须 `Send + Sync`（rune `Function` trait 要求），而 rune 函数值是
// !Send——所以脚本函数值不能放进任何被闭包捕获的容器。tool/command/event 绑定宿主
// 函数在 register() 执行期间（执行线程上）写入本表，run_call 在同一线程读取；
// 每插件一条执行线程，按 namespace 隔离。
thread_local! {
    static BINDINGS: std::cell::RefCell<
        HashMap<String, HashMap<String, std::rc::Rc<RuneFunction>>>,
    > = std::cell::RefCell::new(HashMap::new());
}

/// 脚本执行线程的消息。
enum Msg {
    /// 执行脚本 register()（同步绑定）+ 绑定后校验。
    /// 用 std mpsc 回复：register() 可能被 tokio 运行时内的懒加载路径调用，
    /// tokio oneshot 的 blocking_recv 在运行时内会 panic；std mpsc 只是阻塞
    /// 调用线程等待独立执行线程的结果（无死锁）。
    Register {
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
    /// 调用已绑定的脚本 handler（params 为 JSON；结果回 JSON）。
    Call {
        short: String,
        params: serde_json::Value,
        reply: oneshot::Sender<Result<serde_json::Value, String>>,
    },
    /// 热重载：用新脚本源码重编译（复用执行线程与白名单模块）。
    /// 成功 = 新 VM 就绪、BINDINGS 已清空（等下次 register() 重绑）；
    /// 失败 = 保留旧 VM（回滚），旧绑定不受影响。
    Reload {
        script: String,
        reply: std::sync::mpsc::Sender<Result<(), String>>,
    },
}

/// 目录形态的 Rune 脚本插件（源描述：manifest 声明 + 脚本源码）。
pub struct ScriptPlugin {
    /// manifest.json 反序列化出的 info 结构化声明（enabled 缺省 false，ADR-0005）。
    pub manifest: Info,
    /// plugin.rn 脚本源码（register() + handlers）。
    pub script: String,
}

impl ScriptPlugin {
    /// 从目录加载：目录名必须等于 `manifest.namespace`（一插件一目录约定）。
    pub fn from_dir(dir: &Path) -> Result<Self, PluginError> {
        let dir_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| PluginError::Internal("插件目录名无效".into()))?;
        let manifest_bytes = std::fs::read(dir.join("manifest.json"))
            .map_err(|e| PluginError::Internal(format!("读 manifest.json 失败：{e}")))?;
        let manifest: Info = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| PluginError::Internal(format!("manifest.json 无效：{e}")))?;
        if manifest.namespace != dir_name {
            return Err(PluginError::Internal(format!(
                "目录名 {dir_name} 与 manifest.namespace {} 不一致",
                manifest.namespace
            )));
        }
        let script = std::fs::read_to_string(dir.join("plugin.rn"))
            .map_err(|e| PluginError::Internal(format!("读 plugin.rn 失败：{e}")))?;
        Ok(Self { manifest, script })
    }
}

/// 白名单规格：只含 Send 值，随消息送往执行线程（rune 相关全部在线程侧构造）。
struct WhitelistSpec {
    namespace: String,
    declared: Info,
    session: Option<crate::services::SessionHandle>,
    dynamics: Vec<(ServiceId, Arc<dyn DynamicService>)>,
    events: Arc<dyn EventSink>,
    logger: LoggerHandle,
    handlers: Arc<RwLock<HashMap<String, RegisteredEntry>>>,
    wire_to_full: Arc<RwLock<HashMap<String, String>>>,
    /// 包装器（注册表内）发 Call 消息用；与 handle 侧同一通道。
    tx: UnboundedSender<Msg>,
    script: String,
    /// 单次脚本调用超时（B2 不可信插件防护；死循环不能卡死执行线程）。
    call_timeout: std::time::Duration,
}

/// 编译完成、按 requires 裁剪好白名单的脚本插件运行时（注册表登记 + 懒加载执行）。
#[derive(Clone)]
pub struct ScriptPluginHandle {
    info: Info,
    /// 通往执行线程的通道（注册表登记 + 工具调用都经它）。
    tx: UnboundedSender<Msg>,
}

impl ScriptPluginHandle {
    /// requires 预检 → 组装白名单规格 → 执行线程编译（fail-fast 握手）。
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        plugin: ScriptPlugin,
        services: &ServiceHandles,
        events: Arc<dyn EventSink>,
        logger: LoggerHandle,
        handlers: Arc<RwLock<HashMap<String, RegisteredEntry>>>,
        wire_to_full: Arc<RwLock<HashMap<String, String>>>,
        call_timeout: std::time::Duration,
    ) -> Result<Self, PluginError> {
        let ScriptPlugin { manifest, script } = plugin;

        // ---- 1. requires 预检（与注册表校验一致的清晰错误）----
        if !manifest.provides.is_empty() {
            return Err(PluginError::ProvisionNotAllowed(manifest.provides.clone()));
        }
        let available = services.available();
        let missing: Vec<ServiceId> = manifest
            .requires
            .iter()
            .filter(|r| !available.contains(r))
            .cloned()
            .collect();
        if !missing.is_empty() {
            return Err(PluginError::CapabilityUnavailable(missing));
        }
        if manifest.requires.iter().any(|r| r == &ServiceId::model()) {
            return Err(PluginError::Internal(
                "脚本插件不支持 requires model（P1；模型服务只经 Rust 内核路径）。".into(),
            ));
        }
        let filtered = services.filter(&manifest.requires);
        let session = manifest
            .requires
            .iter()
            .any(|r| r == &ServiceId::session())
            .then(|| filtered.session().expect("requires 已校验").clone());
        let mut dynamics = Vec::new();
        for id in &manifest.requires {
            if id == &ServiceId::session() || id == &ServiceId::model() {
                continue;
            }
            let svc = filtered.get_dynamic(id).ok_or_else(|| {
                PluginError::Internal(format!(
                    "requires 的自定义服务 {id} 未实现 DynamicService，脚本无法访问"
                ))
            })?;
            dynamics.push((id.clone(), svc));
        }

        // ---- 2. 执行线程：构造白名单模块 → 编译（fail-fast 握手）→ 消息循环 ----
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let spec = WhitelistSpec {
            namespace: manifest.namespace.clone(),
            declared: manifest.clone(),
            session,
            dynamics,
            events,
            logger,
            handlers: handlers.clone(),
            wire_to_full,
            tx: tx.clone(),
            script,
            call_timeout,
        };
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name(format!("sl-rune-{}", manifest.namespace))
            .spawn(move || run_script_thread(spec, rx, ready_tx))
            .map_err(|e| PluginError::Internal(format!("脚本执行线程启动失败：{e}")))?;
        ready_rx
            .recv()
            .map_err(|e| PluginError::Internal(format!("脚本执行线程退出：{e}")))?
            .map_err(PluginError::Internal)?;

        Ok(Self { info: manifest, tx })
    }

    pub fn info(&self) -> &Info {
        &self.info
    }

    /// 懒加载：经通道执行脚本 `register()`（同步绑定）+ 绑定后校验。
    pub fn register(&self) -> Result<(), PluginError> {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        self.tx
            .send(Msg::Register { reply: reply_tx })
            .map_err(|_| PluginError::Internal("脚本执行线程已退出".into()))?;
        reply_rx
            .recv()
            .map_err(|e| PluginError::Internal(format!("脚本执行线程退出：{e}")))?
            .map_err(PluginError::Internal)
    }

    /// 热重载：用新脚本源码重编译（复用执行线程与白名单模块；requires 变更需
    /// 先卸载再重新注册，本方法只换脚本内容）。失败 = 保留旧脚本（回滚）。
    ///
    /// 调用方流程（配合 [`crate::registry::Registry::remove_namespace`]）：
    /// `remove_namespace(ns)` 摘旧条目 → `reload(new_script)` 重编译 →
    /// `register()` 重挂绑定（懒加载会再次触发，或显式调用）。
    pub fn reload(&self, script: &str) -> Result<(), PluginError> {
        let (reply_tx, reply_rx) = std::sync::mpsc::channel();
        self.tx
            .send(Msg::Reload {
                script: script.to_string(),
                reply: reply_tx,
            })
            .map_err(|_| PluginError::Internal("脚本执行线程已退出".into()))?;
        reply_rx
            .recv()
            .map_err(|e| PluginError::Internal(format!("脚本执行线程退出：{e}")))?
            .map_err(PluginError::Internal)
    }
}

/// 执行线程主体：构造宿主模块 → 编译（握手回报）→ 消息循环（register / call）。
fn run_script_thread(
    spec: WhitelistSpec,
    mut rx: UnboundedReceiver<Msg>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            let _ = ready.send(Err(format!("执行 runtime 构建失败：{e}")));
            return;
        }
    };
    runtime.block_on(async move {
        // ---- 线程侧：context + 白名单模块 + 编译 ----
        let mut context = match Context::with_default_modules() {
            Ok(c) => c,
            Err(e) => {
                let _ = ready.send(Err(e.to_string()));
                return;
            }
        };
        let mut m = module();
        if let Err(e) = install_base_fns(&mut m, &spec) {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
        if let Some(session) = &spec.session
            && let Err(e) = install_session_fns(&mut m, session.clone())
        {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
        for (id, svc) in &spec.dynamics {
            if let Err(e) = install_call_fns(&mut m, id, svc.clone()) {
                let _ = ready.send(Err(e.to_string()));
                return;
            }
        }
        if let Err(e) = install(&mut context, m) {
            let _ = ready.send(Err(e.to_string()));
            return;
        }
        let vm = match ScriptVm::compile_with_context(&spec.script, &context) {
            Ok(v) => v,
            Err(e) => {
                let _ = ready.send(Err(e.to_string()));
                return;
            }
        };
        let _ = ready.send(Ok(()));
        let mut vm = vm;

        // ---- 消息循环 ----
        while let Some(msg) = rx.recv().await {
            match msg {
                Msg::Register { reply } => {
                    let res = vm
                        .call_sync("register", &[])
                        .map_err(|e| format!("脚本 register() 失败：{e}"))
                        .and_then(|_| verify_bound(&spec.declared, &spec.handlers));
                    let _ = reply.send(res);
                }
                Msg::Call {
                    short,
                    params,
                    reply,
                } => {
                    let res = run_call(&spec.namespace, &short, params, spec.call_timeout).await;
                    let _ = reply.send(res);
                }
                Msg::Reload { script, reply } => {
                    // 热重载：新脚本重编译；失败保留旧 VM（回滚），成功替换并清空绑定表。
                    match ScriptVm::compile_with_context(&script, &context) {
                        Ok(new_vm) => {
                            BINDINGS.with(|b| {
                                b.borrow_mut().remove(&spec.namespace);
                            });
                            vm = new_vm;
                            let _ = reply.send(Ok(()));
                        }
                        Err(e) => {
                            let _ = reply.send(Err(e.to_string()));
                        }
                    }
                }
            }
        }
    });
}

/// 绑定后校验：声明过的入口点必须全部绑定（防脚本静默漏绑）。
fn verify_bound(
    declared: &Info,
    handlers: &RwLock<HashMap<String, RegisteredEntry>>,
) -> Result<(), String> {
    let handlers = handlers.read().expect("registry poisoned");
    for t in &declared.tools {
        let full = crate::contract::full_name(&declared.namespace, &t.name);
        if !handlers.contains_key(&full) {
            return Err(format!("脚本插件未绑定声明的工具：{full}"));
        }
    }
    for c in &declared.commands {
        let full = crate::contract::full_name(&declared.namespace, &c.name);
        if !handlers.contains_key(&full) {
            return Err(format!("脚本插件未绑定声明的命令：{full}"));
        }
    }
    for e in &declared.events {
        let full = crate::contract::full_name(&declared.namespace, &e.name);
        if !handlers.contains_key(&full) {
            return Err(format!("脚本插件未绑定声明的事件：{full}"));
        }
    }
    Ok(())
}

/// 线程内执行一次脚本 handler 调用（rune 值全程不离开本线程）。
/// 带执行超时（B2，不可信插件防护）：脚本死循环不能卡死执行线程——
/// dispatch 层的工具超时只 abort wrapper future，执行线程上真正跑脚本，
/// 必须在这里兜底。超时后返回错误（脚本副作用：本调用失败，线程存活）。
async fn run_call(
    namespace: &str,
    short: &str,
    params: serde_json::Value,
    call_timeout: std::time::Duration,
) -> Result<serde_json::Value, String> {
    let function = BINDINGS
        .with(|b| {
            b.borrow()
                .get(namespace)
                .and_then(|m| m.get(short))
                .cloned()
        })
        .ok_or_else(|| format!("脚本未绑定：{namespace}::{short}"))?;
    let arg = value_from_json(params).map_err(|e| e.to_string())?;
    // `Function::call` 无 Send 约束（async_send_call 的参数必须 Send，rune Value 不是）。
    // 同步 handler 直接出结果；async handler 同步执行到挂起点返回 rune Future 值，
    // 在本地（非 Send 要求）await 收尾——复刻 rune 内部 async_send_call 的语义。
    // rune 的 await 在 current-thread runtime 内执行；tokio timeout 包一层，
    // 超时即取消整个 handler future（含其 async 宿主函数调用链）。
    let future = async move {
        let value: RuneValue = function
            .call((arg,))
            .into_result()
            .map_err(|e| e.to_string())?;
        let value = match value.clone().into_future() {
            Ok(future) => future.await.into_result().map_err(|e| e.to_string())?,
            Err(_) => value,
        };
        json_from_value(&value).map_err(|e| e.to_string())
    };
    match tokio::time::timeout(call_timeout, future).await {
        Ok(res) => res,
        Err(_) => Err(format!(
            "脚本调用超时（>{call_timeout:?}）：{namespace}::{short}"
        )),
    }
}

/// JSON 值转 rune Value；错误经 `VmResult::panic` 抛给脚本调用点
/// （宿主函数返回 `VmResult` = rune 的可失败宿主函数语义，脚本直接调用即可）。
fn vm_value(json: serde_json::Value) -> VmResult<RuneValue> {
    serde_json::from_value(json)
        .map(VmResult::Ok)
        .unwrap_or_else(|e| VmResult::panic(format!("JSON 转 rune 值失败：{e}")))
}

/// base 宿主函数：tool/command/event 绑定 + emit_event/progress/log 播报。
fn install_base_fns(m: &mut Module, spec: &WhitelistSpec) -> Result<(), HostError> {
    // ---- tool / command / event 绑定（同步宿主函数，脚本 register() 内调用）----
    {
        let ns = spec.namespace.clone();
        let declared = spec.declared.clone();
        let handlers = spec.handlers.clone();
        let wire = spec.wire_to_full.clone();
        let tx = spec.tx.clone();
        m.function(
            ["tool"],
            move |short: String, handler: RuneFunction| -> VmResult<()> {
                BINDINGS.with(|b| {
                    b.borrow_mut()
                        .entry(ns.clone())
                        .or_default()
                        .insert(short.clone(), std::rc::Rc::new(handler));
                });
                bind_entry(
                    &ns,
                    &declared,
                    &short,
                    &handlers,
                    &wire,
                    &tx,
                    EntryKind::Tool,
                )
            },
        )
        .build()?;
    }
    {
        let ns = spec.namespace.clone();
        let declared = spec.declared.clone();
        let handlers = spec.handlers.clone();
        let wire = spec.wire_to_full.clone();
        let tx = spec.tx.clone();
        m.function(
            ["command"],
            move |short: String, handler: RuneFunction| -> VmResult<()> {
                BINDINGS.with(|b| {
                    b.borrow_mut()
                        .entry(ns.clone())
                        .or_default()
                        .insert(short.clone(), std::rc::Rc::new(handler));
                });
                bind_entry(
                    &ns,
                    &declared,
                    &short,
                    &handlers,
                    &wire,
                    &tx,
                    EntryKind::Command,
                )
            },
        )
        .build()?;
    }
    {
        let ns = spec.namespace.clone();
        let declared = spec.declared.clone();
        let handlers = spec.handlers.clone();
        let wire = spec.wire_to_full.clone();
        let tx = spec.tx.clone();
        m.function(
            ["event"],
            move |name: String, handler: RuneFunction| -> VmResult<()> {
                BINDINGS.with(|b| {
                    b.borrow_mut()
                        .entry(ns.clone())
                        .or_default()
                        .insert(name.clone(), std::rc::Rc::new(handler));
                });
                bind_entry(
                    &ns,
                    &declared,
                    &name,
                    &handlers,
                    &wire,
                    &tx,
                    EntryKind::Event,
                )
            },
        )
        .build()?;
    }

    // ---- 播报（恒有，脚本自己的声音）----
    {
        let events = spec.events.clone();
        m.function(
            ["emit_event"],
            move |name: String, payload: RuneValue| -> VmResult<()> {
                let payload = match json_from_value(&payload) {
                    Ok(p) => p,
                    Err(e) => return VmResult::panic(e.to_string()),
                };
                events.emit(Event::Custom { name, payload });
                VmResult::Ok(())
            },
        )
        .build()?;
    }
    {
        let ns = spec.namespace.clone();
        let events = spec.events.clone();
        m.function(["progress"], move |message: String| -> VmResult<()> {
            events.emit(Event::ToolProgress {
                entry: ns.clone(),
                message,
                icon: None,
            });
            VmResult::Ok(())
        })
        .build()?;
    }
    {
        let logger = spec.logger.clone();
        m.function(
            ["log"],
            move |level: String, message: String| -> VmResult<()> {
                let level = match level.as_str() {
                    "debug" => Level::Debug,
                    "info" => Level::Info,
                    "warn" => Level::Warn,
                    "error" => Level::Error,
                    "critical" => Level::Critical,
                    other => {
                        return VmResult::panic(format!(
                            "未知日志级别：{other}（debug/info/warn/error/critical）"
                        ));
                    }
                };
                logger.log(level, &message);
                VmResult::Ok(())
            },
        )
        .build()?;
    }
    Ok(())
}

/// 绑定宿主函数实现：把脚本 handler 包装为注册表 handler（与 Rust 路径同一绑定规则）。
/// 绑定失败（短名未声明等）经 `VmResult::panic` 在脚本调用点抛错（fail-fast）。
fn bind_entry(
    ns: &str,
    declared: &Info,
    short: &str,
    handlers: &Arc<RwLock<HashMap<String, RegisteredEntry>>>,
    wire_to_full: &Arc<RwLock<HashMap<String, String>>>,
    tx: &UnboundedSender<Msg>,
    kind: EntryKind,
) -> VmResult<()> {
    let result = match kind {
        EntryKind::Tool => {
            let wrapped = wrap_tool(tx.clone(), short.to_string());
            bind_tool(ns, declared, short, wrapped, handlers, wire_to_full)
        }
        EntryKind::Command => {
            let wrapped = wrap_tool(tx.clone(), short.to_string());
            bind_command(ns, declared, short, wrapped, handlers, wire_to_full)
        }
        EntryKind::Event => {
            let wrapped = wrap_event(tx.clone(), short.to_string());
            bind_event(ns, declared, short, wrapped, handlers, wire_to_full)
        }
    };
    match result {
        Ok(()) => VmResult::Ok(()),
        Err(e) => VmResult::panic(e.to_string()),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryKind {
    Tool,
    Command,
    Event,
}

/// session 服务宿主函数（requires session）。
fn install_session_fns(
    m: &mut Module,
    session: crate::services::SessionHandle,
) -> Result<(), HostError> {
    {
        let session = session.clone();
        m.function(["session_list"], move || {
            let session = session.clone();
            async move {
                match session.list_sessions().await {
                    Ok(list) => match serde_json::to_value(&list) {
                        Ok(json) => vm_value(json),
                        Err(e) => VmResult::panic(e.to_string()),
                    },
                    Err(e) => VmResult::panic(format!("会话列表失败：{e}")),
                }
            }
        })
        .build()?;
    }
    {
        let session = session.clone();
        m.function(["session_read"], move |key: String| {
            let session = session.clone();
            async move {
                let key = match uuid::Uuid::parse_str(&key) {
                    Ok(k) => SessionKey(k),
                    Err(e) => return VmResult::panic(format!("会话键无效：{e}")),
                };
                match session.read_path(&key).await {
                    Ok(messages) => match serde_json::to_value(&messages) {
                        Ok(json) => vm_value(json),
                        Err(e) => VmResult::panic(e.to_string()),
                    },
                    Err(e) => VmResult::panic(format!("读会话失败：{e}")),
                }
            }
        })
        .build()?;
    }
    Ok(())
}

/// 自定义服务动态调用宿主函数（requires custom(id)，每服务一个 `call_<sanitized>`）。
fn install_call_fns(
    m: &mut Module,
    id: &ServiceId,
    svc: Arc<dyn DynamicService>,
) -> Result<(), HostError> {
    let name = format!("call_{}", sanitize_fn_name(id.as_str()));
    m.function([name.as_str()], move |method: String, params: RuneValue| {
        let svc = svc.clone();
        async move {
            let params = match json_from_value(&params) {
                Ok(p) => p,
                Err(e) => return VmResult::panic(e.to_string()),
            };
            match svc.call(&method, params).await {
                Ok(out) => vm_value(out),
                Err(e) => VmResult::panic(format!("{e:?}")),
            }
        }
    })
    .build()?;
    Ok(())
}

/// 脚本函数名清洗：只保留标识符字符，其余替换为 `_`（`call_<id>` 命名用）。
fn sanitize_fn_name(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// 把脚本 handler 包装为 ToolHandler：经通道把调用发给执行线程（rune 值不跨线程）。
fn wrap_tool(tx: UnboundedSender<Msg>, short: String) -> ToolHandler {
    Arc::new(move |_ctx: &ToolCallContext, params: serde_json::Value| {
        let tx = tx.clone();
        let short = short.clone();
        Box::pin(async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            tx.send(Msg::Call {
                short,
                params,
                reply: reply_tx,
            })
            .map_err(|_| ToolError::handler("脚本执行线程已退出"))?;
            let res = reply_rx
                .await
                .map_err(|_| ToolError::handler("脚本执行线程退出"))?;
            res.map_err(ToolError::handler)
        })
    })
}

/// 把脚本 handler 包装为 EventHandler（payload 作为参数；返回忽略）。
fn wrap_event(tx: UnboundedSender<Msg>, short: String) -> EventHandler {
    Arc::new(move |payload: serde_json::Value| {
        let tx = tx.clone();
        let short = short.clone();
        Box::pin(async move {
            let (reply_tx, reply_rx) = oneshot::channel();
            tx.send(Msg::Call {
                short,
                params: payload,
                reply: reply_tx,
            })
            .map_err(|_| ToolError::handler("脚本执行线程已退出"))?;
            let res = reply_rx
                .await
                .map_err(|_| ToolError::handler("脚本执行线程退出"))?;
            res.map_err(ToolError::handler)?;
            Ok(())
        })
    })
}
