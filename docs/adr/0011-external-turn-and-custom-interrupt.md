# 外部驱动回合与业务自定义中断（run_turn + Interrupt::Custom）

**Status**: accepted（2026-08-16；iot-agent ADR-0020 alarm 系统的上游前置依赖，
由 iot-agent fork 定制者提交并同步）

## 背景与动机

使用方存在"内核组件/外部任务主动驱动模型开回合"的需求（iot-agent alarm 系统：
传感器阈值报警 / 定时提醒触发后，以 tool 消息插入会话并**空闲时主动开回合**，
让模型看到事实并自主决策调整外设）。现状缺口：

- `Interrupt` 只有内核自有变体（session/goal/config/compaction），**业务中断
  无通用入口**；loop 只在回合边界消费中断且仅审计，**无人驱动新回合**；
- kernel 回合入口只有 `send_user_message*`（必须追加 user 消息）——报警不是
  用户消息，注入 user 消息会占对话面、形态混淆（iot-agent ADR-0020 明确否决）；
- `InterruptBus` 由 build 内部创建，使用方**无法在 build 前持同一实例**（外部
  驱动任务与 kernel 必须共享同一总线）。

## 决策内容

1. **`Interrupt::Custom { name, payload }`**：业务自定义中断变体，kernel 不解析
   name/payload，只负责运输与审计（审计名 = `custom:<name>`，loop 回合边界消费
   与其他中断同路径）；
2. **`KernelBuilder::interrupt_bus(bus)`**：build 前注入共享中断总线；缺省仍由
   build 自建（零行为变化）。注入后 `Kernel::interrupt_bus()` 暴露同一实例；
3. **`Kernel::run_turn(key)`**：外部驱动回合——不追加用户消息，按会话活跃链
   投影直接跑一轮 loop，新增消息走与 `send_user_message*` **同一落盘管线**
   （append / 压缩 splice / 活跃路径推进 / 回合末决策 / last_activity / 审计）；
   回合进行中调用返回错误（并发拒绝，调用方先查 `get_state().running`）；
4. **职责约定**：外部事实消息（如 tool 消息）由调用方先落盘（经 `SessionStore`），
   `run_turn` 只负责驱动与落盘；中断总线的消费与审计由外部驱动任务负责
   （loop 只在回合边界消费，不抢占进行中的回合）。

## 理由

- **tool 消息进消息流**是报警的核心形态（与设备查询同构：外部事实 → 工具结果），
  模型天然信任、历史可审计、时间线可见——`run_turn` 提供"无用户消息回合"的
  通用能力，具体消息形态由使用方决定（业务语义不进引擎，ADR-0004）；
- **忙时拒绝而非抢占**：告警不打断进行中的回合（抢占会造成模型上下文撕裂），
  错过回合的告警仍留在时间线，下个回合自然可见；
- **共享总线注入**保持"回合边界消费、仅审计"的既有语义不变，外部驱动只做
  增量（空闲开回合）。

## 后果与约束

- 使用方装配模式（iot-agent main.rs 示范）：build 前创建 `InterruptBus` →
  `KernelBuilder::interrupt_bus(bus.clone())` → build 后 spawn 驱动任务（轮询
  `bus.take_all()`，遇 `Custom` 且 `get_state().running == false` 时
  `kernel.run_turn(key)`）；
- `Interrupt::Custom` 是通用变体：任何业务中断（告警/定时器/外部通知）复用，
  不做 alarm 专属语义；
- 上游测试基线 +3（Custom 往返 / run_turn 外部驱动 / 共享总线注入）。

## 被否备选

- **`Kernel::send_business_message`（业务语义入口）**：把"tool 消息插入"固化成
  引擎 API——业务消息形态（entry/result 结构）属于使用方语义，引擎只提供
  "读活跃链跑回合 + 落盘"的通用驱动（ADR-0004 通用边界）；
- **loop 内自消费 Custom 自动开回合**：loop 是回合执行器不是调度器，自动开回合
  需要会话/存储编排，放 kernel 层已越权，放使用方驱动任务职责最清晰。
