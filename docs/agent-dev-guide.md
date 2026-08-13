# 基于 SL Agent 开发自己的 Agent

> 定位（ADR-0009）：so-lite-agent **不发布 crates.io**，是**可执行文件项目**——
> 主交付 = 官方二进制 `sl-agent`。开发自己的 Agent 有**两条主线**，先选路线：

| | **A. sl-agent 扩展者** | **B. fork 定制者** |
|---|---|---|
| 改动面 | 只写 Rune 脚本插件 + 环境变量 | fork 仓库，改 Rust（内核插件/装配） |
| 需要 | 官方二进制 / `cargo run` | Rust 工具链 + 本仓库 |
| 适合 | 业务扩展，不动内核 | 需要新内核能力 / 深度定制 |
| 能力边界 | 只经 `requires` 声明的服务句柄 | 信任边界内（Linus 模式） |

两种路线共用**同一套心智模型**：插件 = 两段式契约（`info` 声明 + `register` 绑定），
服务经句柄注入。差异只在"写 Rust 还是写 Rune"。

---

## 路线 A：sl-agent 扩展者（写 Rune 脚本，最快）

### 1. 跑起官方二进制

```bash
cargo run --bin sl-agent --features server,rune-plugins
# 打开 http://127.0.0.1:8080（默认 mock 模型，零配置 hello 回合）
```

接真实模型（OpenAI 兼容端点，如 DeepSeek）：

```bash
SL_AGENT_API_URL=https://api.deepseek.com SL_AGENT_API_KEY=xxx SL_AGENT_MODEL=deepseek-chat \
  SL_AGENT_DATA_DIR=./data cargo run --bin sl-agent --features server,rune-plugins
```

环境变量：`SL_AGENT_PORT`（默认 8080）、`SL_AGENT_PLUGINS_DIR`（默认 `./plugins`）、
`SL_AGENT_DATA_DIR`（默认 `./data`，会话 JSONL 落盘，ADR-0007）。

### 2. 写第一个 Rune 脚本插件

一插件一目录（目录名 = namespace）：

```
myagent/
└── plugins/
    ├── demo/                  ← 本仓库自带示例（manifest.json + plugin.rn）
    └── notes/                 ← 你的插件
        ├── manifest.json      ← 声明：namespace / enabled / requires / tools
        └── plugin.rn          ← 脚本：register() 绑定 + handler
```

`plugins/notes/manifest.json`：

```json
{
  "namespace": "notes",
  "enabled": true,
  "requires": ["session"],
  "tools": [
    {
      "name": "remind",
      "description": "提醒复习最近笔记",
      "params": { "type": "object" },
      "policy": "user_and_model"
    }
  ]
}
```

`plugins/notes/plugin.rn`：

```rune
pub fn register() {
    tool("remind", handle_remind)
}

async fn handle_remind(params) {
    // requires 白名单里的宿主函数才能调（这里是 session_list）。
    let sessions = session_list().await;
    emit_event("notes.remind", #{ "count": sessions.len() });
    #{
        "remind": "你有 #{sessions.len()} 个会话，开始复习吧",
        "params": params,
    }
}
```

启动时脚本插件自动从 `--plugins` 目录加载（懒加载：首次命中工具才执行
`register()` 绑定）。宿主函数白名单 = `requires` 声明的服务 → 只装对应函数
（结构性白名单，见 docs/plugin-dev.md §Rune 路径）。

### 3. 验证

```bash
cargo run --example script_plugin --features rune-plugins
# 或浏览器里对模型说"调用 notes::remind"
```

---

## 路线 B：fork 定制者（写 Rust，深度定制）

### 1. 克隆并跑通

```bash
git clone <你的 fork>
cd so-lite-agent
cargo test --all-targets --all-features      # 门禁
cargo run --bin sl-agent --features server,rune-plugins
```

### 2. 加一个业务服务（使用方自己的服务实例）

业务服务 = 普通 trait + 实现，放进 `ServiceHandles`（不进 crate，ADR-0004）：

```rust
#[async_trait]
pub trait NoteService: Send + Sync {
    async fn save(&self, content: &str) -> Result<u64, String>;
    async fn count(&self) -> Result<usize, String>;
}
// MemoryNoteService 实现略（见 examples/plugins.rs）

let handles = ServiceHandles::default()
    .with_custom(ServiceId::custom("notes"), Arc::new(MemoryNoteService::default()));
```

### 3. 写用户插件（业务工具，requires 声明依赖）

```rust
pub struct StudyPlugin;
impl UserPlugin for StudyPlugin {
    fn info() -> Info {
        Info {
            namespace: "study".into(),
            enabled: true,
            requires: vec![ServiceId::custom("notes")],
            tools: vec![tool_def("remind", "提醒复习", CallerPolicy::UserAndModel)],
            ..Default::default()
        }
    }
    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError> {
        let notes = ctx.handles.get_custom::<MemoryNoteService>(&ServiceId::custom("notes"))
            .expect("requires 已校验，服务必然注入");
        ctx.registrar.tool("remind", /* 绑定 handler，见 examples/plugins.rs */)
    }
}
```

完整可运行：`examples/plugins.rs`（自定义服务 + 内核插件 + 用户插件端到端）。

### 4. 写内核插件（信任边界内，Linus 模式）

内核插件放 `src/plugin/<name>/`，build.rs 自动发现（ADR-0036），无需改聚合文件：

```rust
// src/plugin/mything/mod.rs
pub struct MyThingPlugin;
impl KernelPlugin for MyThingPlugin {
    fn info() -> Info {
        Info {
            namespace: "mything".into(),
            enabled: true,
            provides: vec![ServiceId::custom("mything")],
            tools: vec![tool_def("stats", "统计", CallerPolicy::UserAndModel)],
            ..Default::default()
        }
    }
    fn register(ctx: KernelContext<'_>) -> Result<(), PluginError> {
        // 内核插件拿全量句柄（不做 requires 过滤）。
        Ok(())
    }
}
pub fn descriptor() -> KernelDescriptor { KernelDescriptor::from_plugin::<MyThingPlugin>() }
```

- 新增插件 = 复制 `docs/plugin-dev/reference/kernel_plugin.rs` 到
  `src/plugin/<你的插件名>/`；目录根放空文件 `disabled` 可整目录禁用；
- `sl-agent` 装配时逐条注册 `builtin_kernel_plugins()`（`src/bin/sl-agent/main.rs`）。

### 5. 装配并跑

```rust
let kernel = KernelBuilder::new()
    .service_handles(handles)          // 业务服务
    .register_plugin(PluginDescriptor::from_plugin::<StudyPlugin>())
    .register_kernel_plugin(KernelDescriptor::from_plugin::<MyThingPlugin>())
    .build()?;
let outcome = kernel.send_user_message(key, "你好").await?;
```

---

## 两条路线共同的部分

### 接模型（路线 B 代码内）

```rust
let registry = ProviderRegistry::new();
let service = register_openai_compatible(&registry, "deepseek", OpenAiCompatibleConfig {
    api_url: "https://api.deepseek.com".into(),
    api_key: key.into(),
    model: "deepseek-v4-flash".into(),
    transport: OpenAiTransport::Responses,
    ..Default::default()
})?;
let auditor = Auditor::new(Arc::new(MemoryAuditSink::default()));
let handles = ServiceHandles::default()
    .with_model(ModelHandle::new(service, Duration::from_secs(30), auditor));
```

### 会话持久化（默认已开）

`sl-agent` 默认 `JsonlSessionStore`（`SL_AGENT_DATA_DIR`）；路线 B 代码内：

```rust
let store = Arc::new(JsonlSessionStore::open(Path::new("./data"))?);
// handles.with_session(store as Arc<dyn SessionStore>)
```

### 会话切换决策（由使用方实现）

crate 只给 `SessionSwitch` 钩子 + `Summarizer`；continue/update_goal/start_new
决策是业务语义，由你注入：

```rust
KernelBuilder::new()
    .session_switch(Arc::new(MySwitcher))
    .summarizer(Arc::new(MySummarizer))
```

### 门禁（fork 后每次改动跑）

```bash
cargo test --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo fmt --check
```

---

## 决策树：该走哪条路

| 你要做什么 | 路线 | 参考 |
|---|---|---|
| 加一个业务工具（查询/写入/通知） | A：Rune 插件 | docs/plugin-dev.md §Rune 路径 |
| 接自己的模型 / 改系统提示 / 换会话存储 | A：环境变量 + 配置 | 本文档「共同部分」 |
| 新内核能力（特权入口、新服务提供者） | B：内核插件 | docs/kernel-dev.md §4.4 |
| 换 agent loop / 深度集成 | B：fork 改装配 | docs/kernel-dev.md §8 |
| 完整示例 | — | examples/hello.rs、examples/plugins.rs、examples/script_plugin.rs |
