// sl-agent 协议类型（对应 Rust 侧 RpcRequest / RpcFrame / Event / Method，M4 定型；
// P3 增量：send_user_message.force_tool、ToolSchema.title/icon、read_session 全量树）。
// 类型安全：前端与后端共享的帧形状在此定义，组件只消费这些类型。

// ---- 下行帧 ----

export type RpcFrame =
  | { type: "response"; id: number; result?: unknown; error?: { code: string; message: string } }
  | { type: "event"; event: AgentEvent };

// ---- 上行请求（Method 子集）----

/** 显式工具调用（对应 Rust ForcedToolRequest）：开回合强制模型首轮调用指定工具。 */
export interface ForcedToolRequest {
  /** 内部全名 namespace::tool */
  entry: string;
  /** 用户输入的可选参数文本（进模型指令） */
  hint?: string | null;
  /** 前端原始展示文本（缺省时 kernel 按 entry title＋hint 兜底，落盘用） */
  display?: string | null;
}

export type RpcMethod =
  | {
      type: "send_user_message";
      session_key?: string | null;
      text: string;
      force_tool?: ForcedToolRequest | null;
    }
  | { type: "trigger_command"; entry: string; params?: unknown }
  | { type: "edit_message"; session_key: string; message_id: string; text: string }
  | { type: "switch_branch"; session_key: string; message_id: string }
  | { type: "abort" }
  | { type: "get_state" }
  | { type: "list_sessions" }
  | { type: "read_session"; session_key: string }
  | { type: "list_tools" }
  | { type: "custom"; method: string; params?: unknown };

export interface RpcRequest {
  id: number;
  method: RpcMethod;
}

// ---- 事件流（Event 枚举）----

export type AgentEvent =
  | { event: "message_delta"; message_id: string; delta: string }
  | { event: "reasoning_delta"; delta: string }
  | { event: "tool_start"; entry: string; icon?: string | null }
  | { event: "tool_end"; entry: string; ok: boolean }
  | { event: "tool_progress"; entry: string; message: string; icon?: string | null }
  | { event: "turn_end"; stop_reason: string }
  | { event: "session_switched"; from: string; to: string }
  | { event: "compaction"; session: string }
  | { event: "error"; message: string }
  | { event: "custom"; name: string; payload: unknown };

// ---- 会话 / 消息 ----

export interface SessionMeta {
  key: string;
  goal?: { text: string } | null;
  status: "active" | "archived";
  created_at: string;
  archived_at?: string | null;
  last_activity_at: string;
  active_path?: string | null;
}

export interface Message {
  id: string;
  parent_id?: string | null;
  kind: MessageKind;
  created_at: string;
}

export type MessageKind =
  | {
      type: "user";
      text: string;
      display_text?: string | null;
      attachments?: Attachment[];
    }
  | { type: "assistant"; text: string }
  | { type: "tool_call"; entry: string; params: unknown; result: unknown; call_id?: string }
  | { type: "reasoning"; id: string; text: string }
  | { type: "system"; text: string };

// ---- 工具目录（list_tools 结果；对应 Rust ToolSchema）----

/** 用户可见工具目录项：wire name + JSON Schema + GUI 展示元数据。 */
export interface ToolSchema {
  name: string;
  description: string;
  input_schema: unknown;
  title?: string | null;
  icon?: string | null;
}

// ---- 说明 ----

// read_session 返回**全量消息树**（逻辑顺序时间线，含被遮蔽分支与压缩摘要节点——
// 前端据此构建树视图 + < / > 分支导航；渲染会话正文时按需过滤被遮蔽分支）。

export interface Attachment {
  path: string;
  name?: string | null;
  mime?: string | null;
  data_base64?: string | null;
}
