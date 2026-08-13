# 会话事实日志：append-only 事件 + 遮蔽投影（参考 DeepSeek Harness 会话模型）

pivot（ADR-0006）后，通用 Agent 可执行文件（`sl-agent`）成为主交付形态；对照
[DeepSeek Harness 调研报告](../research/deepseek-harness-comparison.md)，会话侧最大的
结构性差距是**持久化事件模型**：DSH 以 append-only、lossless-JSON、序号连续的 typed
session event log 为会话真相（消息历史由事件投影得到，可回放/恢复/审计），而本 crate 的
消息树是「追加式 + active path」的树状结构，编辑派生新分支、压缩改链，历史虽不截断但
**没有可重建的事件级真相**，也没有配套持久化。**决策**：消息树转向**会话事实日志**——
会话真相 = 每会话一条 append-only 不可变事件日志（参考 DSH 的 `SessionEventMap` /
`SurfaceOp` / 持久化契约）；编辑、重新生成、压缩退化为「追加 + 遮蔽（replace）+ 投影」，
**不可变的是底层日志，用户可见的编辑/分支 UX 全部保留**。

## 事件词汇表（对齐本 crate 现有语义，参考 DSH `SessionEventMap`）

每条事件 = `{ seq, kind, data, surface_op?, source_event_seqs? }`，lossless JSON、
per-session 序号连续、落盘后不可修改：

| 事件 | 载荷 | 说明 |
|---|---|---|
| `user/message` | text + attachments | 用户消息（append） |
| `assistant/message` | text | 助手消息；**重新生成 = 新事件带 replace 遮蔽旧 assistant** |
| `assistant/reasoning` | text | 推理消息（对应现有 `MessageKind::Reasoning`） |
| `tool/result` | entry + params + result | 工具调用（对应现有 `MessageKind::ToolCall`，P2 合并 call/result，P3 再拆生命周期） |
| `edit` | 新消息 + 被编辑 seq | 用户编辑 = 追加新消息 + replace 遮蔽旧消息（历史保留在日志） |
| `compaction/summary` | summary + 被压区间 | 压缩摘要 + replace 遮蔽被压段（原文留日志；P3 补 start/end 锁事件） |

## 遮蔽（surface op）与投影

- **SurfaceOp**（参考 DSH）：`append` 或 `{ op: "replace", start, end }` + `source_event_seqs`
  （被遮蔽的旧 seq 全集）。编辑/重新生成/压缩统一走 replace。
- **投影 = 现有 active path 语义的落地方式**：模型 history 从「活跃投影链」派生——从目标
  末端沿「谁遮蔽了谁」回溯到根，跳过被遮蔽节点；多条 replace 遮蔽同一段自然形成多条
  候选链，`active path` 选择其中一条末端（即现有 `switch_branch` 的分支 UX）。
  这是对 DSH 线性 surface 的兼容扩展（多一条 replace 就多一个候选），事件模型不变。
- 人读 transcript 读全量日志（append-origin），因为 surface 会遮蔽被替换的旧消息。

## 持久化契约（参考 DSH durability contract）

- `append` 时强制校验 data 可 JSON 序列化（坏事件不进日志）；
- seq 连续；JSONL 落盘为 sl-agent 默认（P2），崩溃尾部修复（截断不完整行）；
- 事实日志与 `EventSink`（GUI 播报）**严格分离**：日志是持久化真相线，播报仍是观察面，
  不把 `Event` 当持久化日志（调研报告第 69 行提醒）。

## 兼容策略

- RPC / Kernel 直连 API **外层不变**：`edit_message` / `switch_branch` / `read_session` /
  `send_user_message` 语义保留，底层从「树操作」改为「追加 + 遮蔽 + 投影」；
- `SessionStore` 契约重写为事件日志 API（append_event / read_events / 投影查询），
  `InMemorySessionStore` 重写为「事件日志 + 投影缓存」；
- 前端帧协议零改动。

## 分步实施

1. **P2（存储转向）**：`SessionStore` → 事件日志契约；`InMemorySessionStore` 重写；
   遮蔽投影（active path 保持）；Kernel/RPC 行为保持；全量测试改造复验；
2. **P2（落盘）**：JSONL `SessionStore` 实现 + 崩溃尾部修复，`sl-agent` 默认启用；
3. **P3（词汇扩展）**：`turn/start|end`、`assistant/chunk`（raw 保真）、tool 生命周期拆分、
   compaction 锁事件、fork（从稳定边界复制事件前缀派生会话）——由 mistake 迁移需求反推。

## 被否备选

- **保留消息树 + 另起 fact log**（调研报告建议的保守路线）：两套真相源并存，投影关系复杂，
  且树分支与日志遮蔽语义重叠——用户选择直接转向单一真相源；
- **纯线性（砍编辑/分支）**：丢编辑消息/切分支的产品 UX，不可变 ≠ 不可编辑；
- **P2 照搬 DSH 全量词汇**（turn/step/raw chunk 全部）：范围过大，分步推进。

## 影响

- `src/services/session.rs`（契约 + InMemory）重写；`Kernel` 的存储调用改造；
  message 树辅助（`append_to_path` 等）职责前移为投影逻辑；
- 涉及测试：session/loop/builder/rpc 相关全部改造复验；
- CONTEXT.md「Message tree」词条改写；plan.md P2 重排；api.md 会话部分同步；
  docs/plugin-dev.md 不受影响（插件不直触存储）。
