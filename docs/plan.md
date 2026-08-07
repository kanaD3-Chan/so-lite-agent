# 里程碑计划（M2 进行中）

> 决策背景：mistake-agent 本仓库不修改（ADR-0001），so-lite-agent 独立推进。

## M2：新仓库骨架（当前）

验收：`cargo add so-lite-agent` 后十行代码跑通 hello 回合（mock 模型）。

| 项 | 状态 |
|---|---|
| 仓库骨架（Cargo.toml / LICENSE / git） | ✅ |
| 通用模块：contract / registry / context / events / audit / message | ✅ |
| 服务契约：ServiceId / SessionStore / InMemorySessionStore / ServiceHandles | ✅ |
| model：ModelService 全套 trait + MockModelService + ProviderRegistry 骨架 | ✅ |
| agent：session / dispatch / loop（含压缩、中断、护栏） | ✅ |
| KernelBuilder（默认服务自动补齐、插件注册 fail-fast）+ Kernel 直连 API | ✅ |
| hello 回合示例 + 集成测试（默认 mock、工具调用、持久化） | ✅ |
| cargo test / clippy / fmt 全绿（18 测试） | ✅ |
| 文档：README / CONTEXT.md / ADR 0001-0004 | ✅ |

## M3：Provider 层

- ✅ OpenAI 兼容适配器（Responses API + Chat Completions，覆盖 DeepSeek/SiliconFlow/Ollama）；
- ✅ Anthropic Messages API 适配器（流式 text/tool_use）；
- ✅ `register_openai_compatible()` / `ProviderRegistry` 接真实 API；
- 🟡 验收：`tests/live_api.rs` 已就绪（`--ignored`，需 `SO_LITE_DEEPSEEK_URL/KEY/MODEL` 跑真实回合）。

## M4：通用 RPC 与插件手册

- ✅ `RpcRequest`/`RpcFrame` + 通用 Method 子集（send_user_message / trigger_command / edit_message / switch_branch / abort / get_state / list_sessions / read_session / list_tools）+ `custom` 兜底 + `RpcExtension`；
- ✅ Kernel 补齐：abort / edit_message / switch_branch / get_state / 附件（中性 path+name）；
- ✅ 插件开发手册 + 参考模板（`docs/plugin-dev/reference/`，复制即开工）。

## M5：发布与切换

- 决策（2026-08-07）：**crates.io 上架除外**，仅做消费切换；
- 🟡 mistake-agent 切到新 crate 消费并删除重复代码（届时才改 mistake-agent），双端回归通过——待评估启动。
