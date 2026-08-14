// sl-agent 协议类型（对应 Rust 侧 RpcRequest / RpcFrame / Event / Method，M4 定型）。
// 类型安全：前端与后端共享的帧形状在此定义，组件只消费这些类型。

// ---- 下行帧 ----

export type RpcFrame =
  | { type: "response"; id: number; result?: unknown; error?: { code: string; message: string } }
  | { type: "event"; event: AgentEvent };

// ---- 上行请求（Method 子集）----

export type RpcMethod =
  | { type: "send_user_message"; session_key?: string | null; text: string }
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

export interface Attachment {
  path: string;
  name?: string | null;
  mime?: string | null;
  data_base64?: string | null;
}
