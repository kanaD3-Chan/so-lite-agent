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
use crate::agent::r#loop::AgentLoop;
use crate::agent::session::{InterruptBus, SessionSwitch, StubSummarizer, Summarizer};
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
    max_tool_calls: usize,
    max_consecutive_failures: usize,
    context_limit_tokens: usize,
    compaction_keep_last: usize,
    default_tool_timeout: Duration,
    grace: Duration,
    turn_budget: Duration,
    rpc_extensions: Vec<Arc<dyn RpcExtension>>,
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
            max_tool_calls: 25,
            max_consecutive_failures: 3,
            context_limit_tokens: 131_072,
            compaction_keep_last: 15,
            default_tool_timeout: Duration::from_secs(30),
            grace: Duration::from_secs(5),
            turn_budget: Duration::from_secs(10 * 60),
            rpc_extensions: Vec::new(),
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

        let loop_engine = Arc::new(
            AgentLoop::new(
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
            .with_tool_guards(self.max_tool_calls, self.max_consecutive_failures),
        );

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
        ))
    }
}
