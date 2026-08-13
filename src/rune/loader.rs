//! Rune 脚本插件加载器（热重载，P2）：目录 watch + 变更检测 + 自动 reload。
//!
//! 通用机制（下游 fork 定制者直接继承）：sl-agent / 下游二进制把插件目录交给
//! [`ScriptPluginLoader`]，后台任务轮询插件文件变更（manifest.json / plugin.rn /
//! 目录新增删除），变更时执行「摘旧条目 → 重编译 → 重挂绑定」，失败回滚保留旧版。
//!
//! 流程（对齐 DSH 可逆副作用语义，ADR-0006）：
//! 1. 目录内插件清单扫描（一插件一目录，manifest.json + plugin.rn）；
//! 2. 变更检测：记录每个插件的文件指纹（manifest 内容 + 脚本长度 + mtime）；
//! 3. reload：`Registry::remove_namespace`（摘旧条目）→
//!    `ScriptPluginHandle::reload`（线程重编译，失败回滚旧 VM）→
//!    `handle.register()`（重挂绑定）→ 重新登记到注册表；
//! 4. manifest（requires/tools 声明）变更 = 白名单规格变化，卸载 + 重新加载
//!    （新线程 + 新白名单，对应 requires 白名单不可原地改）。
//!
//! 轮询而非 notify：无新增依赖（crate 保持轻依赖）；轮询间隔可配。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::contract::PluginError;
use crate::events::EventSink;
use crate::logger::LoggerHandle;
use crate::registry::Registry;
use crate::rune::{ScriptPlugin, ScriptPluginHandle};
use crate::services::ServiceHandles;

/// 插件文件指纹：manifest 内容 + 脚本长度 + 修改时间（粗粒度变更检测）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    manifest_json: String,
    script_len: usize,
    mtime: Option<std::time::SystemTime>,
}

impl FileFingerprint {
    fn of_dir(dir: &Path) -> Result<Option<Self>, PluginError> {
        let manifest = dir.join("manifest.json");
        let script = dir.join("plugin.rn");
        if !manifest.is_file() || !script.is_file() {
            return Ok(None);
        }
        let manifest_json = std::fs::read_to_string(&manifest)
            .map_err(|e| PluginError::Internal(format!("读 manifest.json 失败：{e}")))?;
        let script_len = std::fs::metadata(&script)
            .map_err(|e| PluginError::Internal(format!("读 plugin.rn 元数据失败：{e}")))?
            .len() as usize;
        let mtime = std::fs::metadata(&manifest).and_then(|m| m.modified()).ok();
        Ok(Some(Self {
            manifest_json,
            script_len,
            mtime,
        }))
    }
}

/// 已加载插件状态。
struct LoadedPlugin {
    handle: ScriptPluginHandle,
    fingerprint: FileFingerprint,
}

/// 脚本插件加载器：持有插件目录 + 注册表 + 服务句柄，轮询变更并热重载。
pub struct ScriptPluginLoader {
    dir: PathBuf,
    registry: Arc<Registry>,
    services: ServiceHandles,
    events: Arc<dyn EventSink>,
    logger: LoggerHandle,
    call_timeout: std::time::Duration,
    loaded: std::sync::Mutex<HashMap<String, LoadedPlugin>>,
}

impl ScriptPluginLoader {
    /// 构造加载器（不启动后台任务；调用方决定何时 [`Self::load_all`] / [`Self::poll`]）。
    pub fn new(
        dir: impl Into<PathBuf>,
        registry: Arc<Registry>,
        services: ServiceHandles,
        events: Arc<dyn EventSink>,
        logger: LoggerHandle,
    ) -> Self {
        Self {
            dir: dir.into(),
            registry,
            services,
            events,
            logger,
            call_timeout: std::time::Duration::from_secs(30),
            loaded: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// 脚本单次调用超时（B2；默认 30s，测试/特殊场景可调短）。
    pub fn with_call_timeout(mut self, d: std::time::Duration) -> Self {
        self.call_timeout = d;
        self
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// 扫描目录，加载新增插件（首次调用 = 全量加载；单个失败只告警不中断）。
    pub fn load_all(&self) -> Result<(), PluginError> {
        let mut loaded = self.loaded.lock().expect("loader poisoned");
        if !self.dir.is_dir() {
            log::info!("插件目录不存在，跳过：{}", self.dir.display());
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.dir)
            .map_err(|e| PluginError::Internal(format!("读插件目录失败：{e}")))?
        {
            let entry = entry.map_err(|e| PluginError::Internal(format!("读目录项失败：{e}")))?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(ns) = path.file_name().and_then(|n| n.to_str()).map(String::from) else {
                continue;
            };
            if loaded.contains_key(&ns) {
                continue;
            }
            let Some(fp) = FileFingerprint::of_dir(&path)? else {
                continue;
            };
            match self.load_one(&path, &ns) {
                Ok(handle) => {
                    log::info!("脚本插件已加载：{ns}");
                    loaded.insert(
                        ns,
                        LoadedPlugin {
                            handle,
                            fingerprint: fp,
                        },
                    );
                }
                Err(e) => {
                    eprintln!("LOAD_FAIL ns={ns} err={e}");
                    log::warn!("跳过插件 {ns}：{e}");
                }
            }
        }
        Ok(())
    }

    /// 轮询一次：对每个已加载插件检测变更，变了就热重载（失败回滚保留旧版）。
    pub fn poll(&self) -> Result<(), PluginError> {
        let mut loaded = self.loaded.lock().expect("loader poisoned");
        let mut to_remove = Vec::new();
        for (ns, plugin) in loaded.iter_mut() {
            let dir = self.dir.join(ns);
            if !dir.is_dir() {
                log::info!("插件目录已删除，卸载：{ns}");
                self.registry.remove_namespace(ns);
                to_remove.push(ns.clone());
                continue;
            }
            let Some(fp) = FileFingerprint::of_dir(&dir)? else {
                continue;
            };
            if fp == plugin.fingerprint {
                continue;
            }
            // 变更 → 热重载（摘旧 → 重编译 → 重挂；失败回滚保留旧版）。
            match self.reload_one(&dir, ns, plugin, &fp) {
                Ok(()) => log::info!("脚本插件已热重载：{ns}"),
                Err(e) => log::warn!("插件 {ns} 热重载失败（保留旧版）：{e}"),
            }
        }
        for ns in to_remove {
            loaded.remove(&ns);
        }
        Ok(())
    }

    /// 后台轮询循环（调用方 spawn；间隔可配）。
    pub async fn run_loop(self: Arc<Self>, interval: std::time::Duration) {
        loop {
            tokio::time::sleep(interval).await;
            if let Err(e) = self.poll() {
                log::warn!("插件轮询失败：{e}");
            }
        }
    }

    fn load_one(&self, dir: &Path, ns: &str) -> Result<ScriptPluginHandle, PluginError> {
        let plugin = ScriptPlugin::from_dir(dir)?;
        if plugin.manifest.namespace != ns {
            return Err(PluginError::Internal(format!(
                "目录名 {ns} 与 manifest.namespace {} 不一致",
                plugin.manifest.namespace
            )));
        }
        let (handlers_arc, wire_arc) = self.registry.targets_arc();
        let handle = ScriptPluginHandle::new(
            plugin,
            &self.services,
            self.events.clone(),
            self.logger.clone(),
            handlers_arc,
            wire_arc,
            self.call_timeout,
        )?;
        // 只登记（懒加载：首次命中工具时自动执行 register() 绑定）。
        self.registry.register_script(handle.clone())?;
        Ok(handle)
    }

    fn reload_one(
        &self,
        dir: &Path,
        ns: &str,
        plugin: &mut LoadedPlugin,
        new_fp: &FileFingerprint,
    ) -> Result<(), PluginError> {
        let plugin_new = ScriptPlugin::from_dir(dir)?;
        if plugin_new.manifest.namespace != ns {
            return Err(PluginError::Internal(format!(
                "目录名 {ns} 与 manifest.namespace {} 不一致",
                plugin_new.manifest.namespace
            )));
        }
        // requires / 声明变化 = 白名单规格变化，只能卸载 + 重新加载（换线程）。
        let manifest_changed = plugin_new.manifest != plugin.handle.info().clone();
        if manifest_changed {
            // 白名单规格变化：新线程编译成功才替换（失败保留旧版）；成功后摘旧登记新。
            let (handlers_arc, wire_arc) = self.registry.targets_arc();
            let handle = ScriptPluginHandle::new(
                plugin_new,
                &self.services,
                self.events.clone(),
                self.logger.clone(),
                handlers_arc,
                wire_arc,
                self.call_timeout,
            )?;
            self.registry.remove_namespace(ns);
            self.registry.register_script(handle.clone())?;
            plugin.handle = handle;
            plugin.fingerprint = new_fp.clone();
            return Ok(());
        }
        // 仅脚本内容变化：先线程重编译（失败直接返回，旧 VM + 旧条目原封不动），
        // 成功后才摘旧条目并重新登记（懒加载重绑）——失败回滚语义。
        plugin.handle.reload(&plugin_new.script)?;
        self.registry.remove_namespace(ns);
        self.registry.register_script(plugin.handle.clone())?;
        plugin.fingerprint = new_fp.clone();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// 测试：热重载（脚本变更生效 / 语法错误回滚 / 目录删除卸载）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::MemoryEventSink;
    use crate::logger::Logger;
    use crate::registry::Registry;
    use crate::services::ServiceHandles;

    fn write_plugin(dir: &Path, script: &str) {
        let manifest = r#"{
            "namespace": "hot",
            "enabled": true,
            "requires": [],
            "tools": [
                { "name": "ping", "description": "回显",
                  "params": { "type": "object" }, "policy": "user_and_model" }
            ]
        }"#;
        let plugin_dir = dir.join("hot");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        std::fs::write(plugin_dir.join("manifest.json"), manifest).unwrap();
        std::fs::write(plugin_dir.join("plugin.rn"), script).unwrap();
    }

    fn make_loader(dir: &Path) -> (Arc<ScriptPluginLoader>, Arc<Registry>) {
        let registry = Arc::new(Registry::new(ServiceHandles::default(), Arc::new(Logger)));
        let loader = Arc::new(ScriptPluginLoader::new(
            dir.to_path_buf(),
            registry.clone(),
            ServiceHandles::default(),
            Arc::new(MemoryEventSink::default()),
            Arc::new(Logger),
        ));
        (loader, registry)
    }

    /// 工具是否存在（注册表条目 = 已绑定）。
    fn tool_exists(registry: &Registry) -> bool {
        registry.ensure_tool("hot::ping").is_ok()
    }

    /// 取当前工具 handler（reload 重挂后应变化）。
    fn tool_handler(registry: &Registry) -> crate::agent::dispatch::ToolHandler {
        let entry = registry.ensure_tool("hot::ping").expect("工具已绑定");
        match &entry.handler {
            crate::registry::Handler::Tool(h) => h.clone(),
            _ => panic!("非工具条目"),
        }
    }

    #[tokio::test]
    async fn reload_applies_new_script() {
        let dir = tempfile::TempDir::new().unwrap();
        write_plugin(
            dir.path(),
            r#"
            pub fn register() { tool("ping", handle) }
            async fn handle(params) { #{ "pong": "v1", "params": params } }
        "#,
        );
        let (loader, registry) = make_loader(dir.path());
        loader.load_all().unwrap();
        assert!(tool_exists(&registry));
        let old_handler = tool_handler(&registry);

        // 改脚本 → 轮询 → 旧条目被摘、新绑定生效（handler 指针变化）。
        write_plugin(
            dir.path(),
            r#"
            pub fn register() { tool("ping", handle) }
            async fn handle(params) { #{ "pong": "v2", "params": params } }
        "#,
        );
        loader.poll().unwrap();
        assert!(tool_exists(&registry), "热重载后工具仍绑定");
        assert!(
            !Arc::ptr_eq(&tool_handler(&registry), &old_handler),
            "重挂后 handler 应为新包装"
        );
    }

    #[tokio::test]
    async fn reload_syntax_error_rolls_back() {
        let dir = tempfile::TempDir::new().unwrap();
        write_plugin(
            dir.path(),
            r#"
            pub fn register() { tool("ping", handle) }
            async fn handle(params) { #{ "pong": "v1" } }
        "#,
        );
        let (loader, registry) = make_loader(dir.path());
        loader.load_all().unwrap();
        let old_handler = tool_handler(&registry);

        // 写入语法错误脚本 → 轮询失败回滚 → 旧绑定仍可用（指针不变）。
        write_plugin(dir.path(), "pub fn register() { 语法错误 }");
        loader.poll().unwrap(); // poll 内部捕获错误，不 panic
        assert!(tool_exists(&registry), "回滚后旧版仍绑定");
        assert!(
            Arc::ptr_eq(&tool_handler(&registry), &old_handler),
            "语法错误应保留旧绑定"
        );
    }

    #[tokio::test]
    async fn removed_dir_unloads_plugin() {
        let dir = tempfile::TempDir::new().unwrap();
        write_plugin(
            dir.path(),
            r#"
            pub fn register() { tool("ping", handle) }
            async fn handle(params) { #{ "pong": "v1" } }
        "#,
        );
        let (loader, registry) = make_loader(dir.path());
        loader.load_all().unwrap();
        assert!(tool_exists(&registry));

        std::fs::remove_dir_all(dir.path().join("hot")).unwrap();
        loader.poll().unwrap();
        assert!(!tool_exists(&registry), "目录删除后工具应卸载");
    }

    /// B2：死循环脚本不能卡死执行线程——短超时后调用返回错误。
    #[tokio::test]
    async fn infinite_loop_times_out() {
        let dir = tempfile::TempDir::new().unwrap();
        write_plugin(
            dir.path(),
            r#"
            pub fn register() { tool("ping", handle) }
            async fn handle(params) { loop {} }
        "#,
        );
        let registry = Arc::new(Registry::new(ServiceHandles::default(), Arc::new(Logger)));
        let loader = Arc::new(
            ScriptPluginLoader::new(
                dir.path().to_path_buf(),
                registry.clone(),
                ServiceHandles::default(),
                Arc::new(MemoryEventSink::default()),
                Arc::new(Logger),
            )
            .with_call_timeout(std::time::Duration::from_millis(200)),
        );
        loader.load_all().unwrap();

        let dispatch = crate::agent::dispatch::Dispatch::new(
            registry.clone(),
            crate::audit::Auditor::new(Arc::new(crate::audit::MemoryAuditSink::default())),
            std::time::Duration::from_secs(5),
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(30),
            Arc::new(MemoryEventSink::default()),
        );
        // 死循环：应超时返回错误，而不是挂死（tokio test 默认 60s 超时兜底）。
        let out = dispatch
            .call_tool(
                "hot::ping",
                serde_json::json!({}),
                crate::agent::dispatch::Caller::User,
            )
            .await;
        assert!(out.is_err(), "死循环脚本应超时：{out:?}");
    }
}
