# 混合式服务标识：字符串 ServiceId + 内置类型化槽位 + 自定义 Any 包

通用 crate 需要支持使用方自写内核插件提供业务服务，封闭枚举（{Storage, Memory, Compute, Model}）每加一种服务就要发版，不满足"开箱即用 + 插件自写"。采用混合式：`ServiceId` 是字符串背书的 newtype（内置 `session()` / `model()`，业务服务用 `custom(name)`）；`ServiceHandles` 为会话、模型保留类型化槽位（trait object 无法走 `Any` 反转型槽位），其余自定义服务进 `HashMap<ServiceId, Arc<dyn Any + Send + Sync>>`，插件侧经 `get_custom::<T>()` 运行时 downcast 取回。注册表照旧按 `provides` 全局唯一。代价：自定义服务少一点编译期保证，换来不改 crate 就能扩展服务集。
