# 插件开发上手

> 完整可运行示例：[examples/plugins.rs](../examples/plugins.rs)。

## 心智模型

插件与内核之间是**两段式契约**：

1. `info()`：静态声明——namespace、能力依赖（`requires` / `provides`）、入口点元数据（工具/命令/事件）、调用方策略；
2. `register(ctx)`：动态绑定——拿句柄、把 handler 绑到声明过的入口点上。

注册表在启动时 fail-fast 校验：namespace/wire 撞名、`requires` 服务缺失、重复 `provides`、用户插件声明 `provides`，都会让 `build()` 直接失败。

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
