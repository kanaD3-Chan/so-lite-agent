# KernelBuilder 自动补齐默认服务，插件注册保持 fail-fast

为满足"`cargo add` 后十行代码跑通 hello 回合"，`KernelBuilder::build()` 对未显式提供的组件自动补齐默认值：会话存储 = `InMemorySessionStore`、模型 = `MockModelService`、事件流 = `MemoryEventSink`、审计 = `MemoryAuditSink`、system_prompt = 空串 provider。显式传过的一律优先。安全边界不因默认值放宽：插件注册仍 fail-fast（namespace/wire 撞名、requires 能力缺失、重复 provides 当场报错）。备选"缺省即报错"被否：第一体验会退化成配置清单，与开箱即用相悖。
