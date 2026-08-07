# 插件开发上手

> 完整可运行示例：[examples/plugins.rs](../examples/plugins.rs)。

## 心智模型

插件与内核之间是**两段式契约**：

1. `info()`：静态声明——namespace、能力依赖（`requires` / `provides`）、入口点元数据（工具/命令/事件）、调用方策略；
2. `register(ctx)`：动态绑定——拿句柄、把 handler 绑到声明过的入口点上。

注册表在启动时 fail-fast 校验：namespace/wire 撞名、`requires` 服务缺失、重复 `provides`、用户插件声明 `provides`，都会让 `build()` 直接失败。

## 注册流程：trait 是契约，描述符才是注册

实现 `UserPlugin` / `KernelPlugin` 只是**定义契约**，插件不会因此自动出现；必须把描述符显式交给注册表：

```rust
// 用户插件
let desc = PluginDescriptor::from_plugin::<StudyPlugin>();
KernelBuilder::new().register_plugin(desc);
// 等价：registry.register_plugin(desc)

// 内核插件
let desc = KernelDescriptor::from_plugin::<NotesKernelPlugin>();
KernelBuilder::new().register_kernel_plugin(desc);
// 等价：registry.register_kernel_plugin(desc)
```

`from_plugin::<P>()` 把 `P::info()`（静态声明）和 `P::register`（绑定函数指针）打包成一个描述符；插件本身不需要实例、不持有状态，状态都在 register 阶段捕获的 Arc 句柄里。

两段式的时间点：

- **注册时**（`build()` / `register_*_plugin` 调用）：只校验 `info()` 的声明——namespace/wire 唯一、`requires` 可满足、`provides` 不重复、用户插件不得 provides；
- **加载时**（默认懒加载，首次命中入口点才执行）：调用 `register(ctx)` 绑定 handler；`info().load = LoadPolicy::Eager` 可改为注册时立即绑定。

**为什么不用宏/自动发现**：注册保持**显式链式调用**（`register_plugin` / `register_kernel_plugin` 各一行），延续 mistake-agent 的显式装配设计（对应其 ADR-0036 的结论）。曾评估过两类替代：属性宏 + 一次聚合（仍需显式清单，收益有限）与 `inventory`/`linkme` 链接期自动收集（最接近 Python 装饰器，但引入隐式全局注册表、平台坑，且破坏 build 时 fail-fast 的可预期性）——均被否。以后若想减少样板，加宏是纯增量、不破坏现有 API。

用户插件与内核插件对照：

| | 用户插件 | 内核插件 |
|---|---|---|
| trait | `UserPlugin` | `KernelPlugin` |
| 描述符 | `PluginDescriptor` | `KernelDescriptor` |
| 注册方法 | `register_plugin` | `register_kernel_plugin` |
| 注册上下文 | `PluginContext`（只含 `requires` 声明的句柄） | `KernelContext`（全量句柄） |
| 能力声明 | 只能 `requires` | 可 `requires` + `provides` |
| 信任边界 | 外（受限句柄） | 内（特权入口） |

## 用户插件（业务工具）

用户插件在信任边界外：register 只收到**声明过的**服务句柄，看不到完整服务接口。

```rust
pub struct StudyPlugin;

impl UserPlugin for StudyPlugin {
    fn info() -> Info {
        Info {
            namespace: "study".into(),
            requires: vec![ServiceId::custom("notes")],
            tools: vec![tool_def("remind", "提醒复习", CallerPolicy::UserAndModel)],
            ..Default::default()
        }
    }

    fn register(ctx: PluginContext<'_>) -> Result<(), PluginError> {
        let notes = ctx
            .handles
            .get_custom::<MemoryNoteService>(&ServiceId::custom("notes"))
            .expect("requires 已校验");
        ctx.registrar.tool(
            "remind",
            Arc::new(move |_ctx, _params| {
                let notes = notes.clone();
                Box::pin(async move { /* 调 notes，返回 Ok(json!(...)) */ })
            }),
        )
    }
}
```

注意：插件只写**短名**（`remind`），kernel 拼全名（`study::remind`），模型看到的是 wire name（`study__remind`）。

## 目录编排（推荐约定）

契约与代码放哪无关，但推荐**一插件一目录**（沿用 mistake-agent 的组织风格，不含它的编译期发现语义）：

```text
src/
├── plugins/
│   ├── mod.rs              ← 聚合点：每个插件一行显式注册
│   ├── study/
│   │   ├── mod.rs          ← 两段式契约：info() + register() + descriptor()
│   │   └── core.rs         ← handler 绑定与业务逻辑
│   └── kernel_notes/
│       ├── mod.rs
│       └── core.rs
└── notes.rs                ← 共享业务服务（trait + 实现 + ServiceId）
```

```rust
// plugins/mod.rs 聚合点（唯一显式清单）
pub mod kernel_notes;
pub mod study;
```

```rust
// 装配处
.register_kernel_plugin(PluginDescriptor::from_plugin::<plugins::kernel_notes::NotesKernelPlugin>())
.register_plugin(PluginDescriptor::from_plugin::<plugins::study::StudyPlugin>())
```

禁用插件 = 从聚合点删掉那一行（或 feature flag 条件编译），不需要 `disabled` 标记文件——那是 mistake-agent 编译期 include! 聚合的配套，这里注册是显式的。

完整可运行示例：[examples/folder_plugins](../examples/folder_plugins/main.rs)（study/ 用户插件 + kernel_notes/ 内核插件 + 共享 notes.rs）。

## 内核插件（特权入口）

内核插件在信任边界内：register 拿到**全量**句柄，可注册需要特权的入口（如记忆路由、验算、会话切换）。

```rust
impl KernelPlugin for NotesKernelPlugin {
    fn info() -> Info {
        Info {
            namespace: "kernel_notes".into(),
            provides: vec![ServiceId::custom("notes")], // 声明服务身份（全局唯一）
            tools: vec![tool_def("stats", "统计", CallerPolicy::UserAndModel)],
            ..Default::default()
        }
    }

    fn register(ctx: KernelContext<'_>) -> Result<(), PluginError> {
        // 全量句柄：ctx.handles 里什么都有
        ctx.registrar.tool("stats", Arc::new(/* ... */))
    }
}
```

## 自定义服务的接线

服务**实例**不进插件代码，而是放进 `ServiceHandles`，由 `KernelBuilder` 注入：

```rust
let handles = ServiceHandles::default()
    .with_model(ModelHandle::new(model, timeout, auditor))
    .with_custom(ServiceId::custom("notes"), Arc::new(MemoryNoteService::default()));

let kernel = KernelBuilder::new()
    .service_handles(handles)
    .register_kernel_plugin(KernelDescriptor::from_plugin::<NotesKernelPlugin>())
    .register_plugin(PluginDescriptor::from_plugin::<StudyPlugin>())
    .build()?;
```

取回方式：`handles.get_custom::<MemoryNoteService>(&ServiceId::custom("notes"))`。
自定义服务按**具体类型** downcast（ADR-0002 的取舍），所以内核插件与用户插件共享同一个具体类型即可；需要抽象时把 trait 实现类型放在共享模块里。

## 入口点与策略

- `Tool`：模型/用户可调；`CallerPolicy::UserAndModel` 进模型工具列表，`UserOnly` 不进模型列表且调度层再拒一次（双墙）。
- `Command`：恒为 `UserOnly`，经 `kernel.call_command(entry, params)`（trigger_command 等价）触发；找不到 Command 时回退同名 Tool。
- `Event`：kernel 生命周期回调，不对外暴露。

## 缺省行为

没显式给会话/模型服务时，`KernelBuilder` 自动补 `InMemorySessionStore` + `MockModelService`，所以从最小例子起步不需要配任何东西。
