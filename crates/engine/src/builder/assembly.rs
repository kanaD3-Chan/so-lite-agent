//! KernelBuilder：装配入口（默认服务自动补齐，插件注册 fail-fast）。
//!
//! ```no_run
//! # async fn demo() -> Result<(), Box<dyn std::error::Error>> {
//! use so_lite_agent::builder::KernelBuilder;
//! use so_lite_agent::events::MemoryEventSink;
//! use std::sync::Arc;
//!
//! let events = Arc::new(MemoryEventSink::default());
//! let kernel = KernelBuilder::new()
//!     .event_sink(events)
//!     .system_prompt(|| "你是 so-lite-agent。".to_string())
//!     .build()?;
//! let outcome = kernel.send_user_message(Default::default(), "你好").await?;
//! println!("{outcome:?}");
//! # Ok(())
//! # }
//! ```

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::agent::dispatch::Dispatch;
use crate::agent::r#loop::{AgentLoop, DefaultAgentLoop};
use crate::agent::session::{
    InterruptBus, SessionDecision, SessionSwitch, StubSummarizer, Summarizer,
};
use crate::audit::{AuditSink, Auditor, MemoryAuditSink};
use crate::contract::PluginError;
use crate::events::{EventSink, MemoryEventSink};
use crate::logger::{Logger, LoggerHandle};
use crate::model::{MockModelService, ModelHandle, ModelService};
use crate::registry::{KernelDescriptor, PluginDescriptor, Registry};
use crate::rpc::RpcExtension;
use crate::services::{InMemorySessionStore, ServiceHandles};

use super::Kernel;

pub struct KernelBuilder {
    event_sink: Arc<dyn EventSink>,
    audit_sink: Arc<dyn AuditSink>,
    service_handles: ServiceHandles,
    kernel_plugins: Vec<KernelDescriptor>,
    plugins: Vec<PluginDescriptor>,
    system_prompt: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    summarizer: Option<Arc<dyn Summarizer>>,
    session_switch: Option<Arc<dyn SessionSwitch>>,
    /// 会话调度决策器（ADR-0010 使用方实现）：注入后 send_user_message 改为
    /// 前置决策（on_new_message 追加/分叉/切换）+ 回合末决策（on_turn_end）。
    session_decision: Option<Arc<dyn SessionDecision>>,
    max_tool_calls: usize,
    max_consecutive_failures: usize,
    context_limit_tokens: usize,
    compaction_keep_last: usize,
    default_tool_timeout: Duration,
    grace: Duration,
    turn_budget: Duration,
    rpc_extensions: Vec<Arc<dyn RpcExtension>>,
    /// 可替换的 agent loop（Capability seam，ADR-0006）；缺省用内置默认实现。
    loop_engine: Option<Arc<dyn AgentLoop>>,
    /// 事件决策分离（P2）：loop 决策 hook 链（before_tool 可拒绝/改写，其余观察）。
    loop_hooks: Vec<Arc<dyn crate::agent::r#loop::LoopHook>>,
    /// Rune 脚本用户插件（ADR-0006；feature rune-plugins）。
    #[cfg(feature = "rune-plugins")]
    script_plugins: Vec<crate::rune::ScriptPlugin>,
}

impl Default for KernelBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelBuilder {
    pub fn new() -> Self {
        Self {
            event_sink: Arc::new(MemoryEventSink::default()),
            audit_sink: Arc::new(MemoryAuditSink::default()),
            service_handles: ServiceHandles::default(),
            kernel_plugins: Vec::new(),
            plugins: Vec::new(),
            system_prompt: None,
            summarizer: None,
            session_switch: None,
            session_decision: None,
            max_tool_calls: 25,
            max_consecutive_failures: 3,
            context_limit_tokens: 131_072,
            compaction_keep_last: 15,
            default_tool_timeout: Duration::from_secs(30),
            grace: Duration::from_secs(5),
            turn_budget: Duration::from_secs(10 * 60),
            rpc_extensions: Vec::new(),
            loop_engine: None,
            loop_hooks: Vec::new(),
            #[cfg(feature = "rune-plugins")]
            script_plugins: Vec::new(),
        }
    }

    pub fn event_sink(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.event_sink = sink;
        self
    }

    pub fn audit_sink(mut self, sink: Arc<dyn AuditSink>) -> Self {
        self.audit_sink = sink;
        self
    }

    /// 服务句柄（使用方构造：with_session / with_model / with_custom）；
    /// 缺省的会话/模型服务在 build 时自动补齐。
    pub fn service_handles(mut self, handles: ServiceHandles) -> Self {
        self.service_handles = handles;
        self
    }

    pub fn register_kernel_plugin(mut self, desc: KernelDescriptor) -> Self {
        self.kernel_plugins.push(desc);
        self
    }

    pub fn register_plugin(mut self, desc: PluginDescriptor) -> Self {
        self.plugins.push(desc);
        self
    }

    /// 人格注入：替代 loop 直调，每轮请求重新生成系统提示。
    pub fn system_prompt(mut self, f: impl Fn() -> String + Send + Sync + 'static) -> Self {
        self.system_prompt = Some(Arc::new(f));
        self
    }

    /// 注入真实摘要器（压缩/交接摘要）；缺省用计数摘要桩 [`StubSummarizer`]。
    pub fn summarizer(mut self, s: Arc<dyn Summarizer>) -> Self {
        self.summarizer = Some(s);
        self
    }

    /// 注入会话切换钩子（回合内 `session::switch` 由 loop 执行）；
    /// 缺省无切换能力（工具会返回"会话切换不可用"）。
    pub fn session_switch(mut self, s: Arc<dyn SessionSwitch>) -> Self {
        self.session_switch = Some(s);
        self
    }

    /// 注入会话调度决策器（mistake-agent SessionScheduler 形态）：新消息前置决策
    /// 与回合末决策。注入后 kernel 的 send_user_message* 委托决策器追加/分叉/切换，
    /// 不再自行 append user 消息。
    pub fn session_decision(mut self, d: Arc<dyn SessionDecision>) -> Self {
        self.session_decision = Some(d);
        self
    }

    pub fn max_tool_calls(mut self, n: usize) -> Self {
        self.max_tool_calls = n;
        self
    }

    pub fn max_consecutive_failures(mut self, n: usize) -> Self {
        self.max_consecutive_failures = n;
        self
    }

    pub fn context_limit_tokens(mut self, n: usize) -> Self {
        self.context_limit_tokens = n;
        self
    }

    pub fn compaction_keep_last(mut self, n: usize) -> Self {
        self.compaction_keep_last = n;
        self
    }

    pub fn default_tool_timeout(mut self, d: Duration) -> Self {
        self.default_tool_timeout = d;
        self
    }

    pub fn turn_budget(mut self, d: Duration) -> Self {
        self.turn_budget = d;
        self
    }

    /// 挂一个业务方法扩展（settings/balance/cache 等走 `custom` 兜底）。
    pub fn rpc_extension(mut self, ext: Arc<dyn RpcExtension>) -> Self {
        self.rpc_extensions.push(ext);
        self
    }

    /// 注入可替换的 agent loop（Capability seam，ADR-0006）：换 loop 不换内核其余部分。
    /// 缺省使用内置默认实现（[`DefaultAgentLoop`]，护栏/压缩参数由本 builder 的
    /// `max_tool_calls` / `context_limit_tokens` 等配置）。自定义实现需自行消费
    /// `InterruptBus`（经 `Kernel::interrupt_bus` 可达）。
    pub fn loop_engine(mut self, engine: Arc<dyn AgentLoop>) -> Self {
        self.loop_engine = Some(engine);
        self
    }

    /// 注入决策 hook（事件决策分离，P2）：按注册顺序链式执行。
    /// `before_tool` 可改写参数/拒绝（错误回喂模型），其余观察式。
    /// 仅对内置默认 loop 生效（自定义 loop 需自行消费 hook）。
    pub fn loop_hook(mut self, hook: Arc<dyn crate::agent::r#loop::LoopHook>) -> Self {
        self.loop_hooks.push(hook);
        self
    }

    /// 注册一个 Rune 脚本用户插件（ADR-0006）：目录形态（manifest.json + plugin.rn）
    /// 经 [`ScriptPlugin::from_dir`] 加载后交给本方法；编译失败在 build() 时 fail-fast。
    #[cfg(feature = "rune-plugins")]
    pub fn script_plugin(mut self, plugin: crate::rune::ScriptPlugin) -> Self {
        self.script_plugins.push(plugin);
        self
    }

    pub fn build(self) -> Result<Kernel, PluginError> {
        let logger: LoggerHandle = Arc::new(Logger);
        let auditor = Auditor::new(self.audit_sink);

        let mut handles = self.service_handles;
        if handles.session().is_none() {
            handles = handles.with_session(Arc::new(InMemorySessionStore::default()));
        }
        if handles.model().is_none() {
            let mock: Arc<dyn ModelService> =
                Arc::new(MockModelService::new("你好，我是 so-lite-agent。"));
            handles = handles.with_model(ModelHandle::new(
                mock,
                self.default_tool_timeout,
                auditor.clone(),
            ));
        }

        let registry = Arc::new(Registry::new(handles.clone(), logger.clone()));
        for desc in self.kernel_plugins {
            registry.register_kernel_plugin(desc)?;
        }
        for desc in self.plugins {
            registry.register_plugin(desc)?;
        }
        #[cfg(feature = "rune-plugins")]
        {
            // Rune 脚本插件：编译 + 白名单 fail-fast，再走同一张注册表校验。
            let (handlers_arc, wire_arc) = registry.targets_arc();
            for plugin in self.script_plugins {
                let handle = crate::rune::ScriptPluginHandle::new(
                    plugin,
                    &handles,
                    self.event_sink.clone(),
                    logger.clone(),
                    handlers_arc.clone(),
                    wire_arc.clone(),
                    // 脚本调用超时（B2 不可信插件防护，默认 30s）。
                    std::time::Duration::from_secs(30),
                )?;
                registry.register_script(handle)?;
            }
        }

        let events = self.event_sink;
        let dispatch = Arc::new(Dispatch::new(
            registry.clone(),
            auditor.clone(),
            self.default_tool_timeout,
            self.grace,
            self.turn_budget,
            events.clone(),
        ));

        let bus = InterruptBus::new();
        let model = handles.model().expect("默认模型已注入").inner().clone();
        let store = handles.session().expect("默认会话存储已注入").clone();
        let system_prompt: Arc<dyn Fn() -> String + Send + Sync> =
            self.system_prompt.unwrap_or_else(|| Arc::new(String::new));
        let summarizer: Arc<dyn Summarizer> =
            self.summarizer.unwrap_or_else(|| Arc::new(StubSummarizer));

        let loop_engine: Arc<dyn AgentLoop> = match self.loop_engine {
            Some(engine) => engine,
            None => {
                let mut engine = DefaultAgentLoop::new(
                    model,
                    dispatch.clone(),
                    auditor.clone(),
                    events.clone(),
                    system_prompt,
                    summarizer,
                    bus.clone(),
                    self.session_switch,
                )
                .with_compaction_limits(self.context_limit_tokens, self.compaction_keep_last)
                .with_tool_guards(self.max_tool_calls, self.max_consecutive_failures);
                for hook in self.loop_hooks {
                    engine = engine.with_hook(hook);
                }
                Arc::new(engine)
            }
        };

        Ok(Kernel::assemble(
            registry,
            dispatch,
            loop_engine,
            store,
            auditor,
            events,
            bus,
            self.turn_budget,
            self.rpc_extensions,
            Mutex::new(None),
            self.session_decision,
        ))
    }
}
