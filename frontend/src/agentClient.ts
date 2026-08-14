// sl-agent WS 协议桥（对应 Rust 侧 RpcRequest / RpcFrame，M4 定型）。
// 连接：ws://127.0.0.1:8080/ws（可用 VITE_SL_AGENT_WS 环境变量覆盖）。
// 断线自动重连；重连后上层重拉会话列表补渲染。

import type { RpcFrame, RpcMethod, RpcRequest } from "./protocol";

const DEFAULT_WS: string =
  (import.meta.env.VITE_SL_AGENT_WS as string | undefined) || "ws://127.0.0.1:8080/ws";

export type ConnStatus = "connecting" | "connected" | "disconnected";

interface AgentClientOptions {
  onEvent: (frame: Extract<RpcFrame, { type: "event" }>) => void;
  onResponse: (frame: Extract<RpcFrame, { type: "response" }>) => void;
  onStatus?: (status: ConnStatus) => void;
}

export class AgentClient {
  private ws: WebSocket | null = null;
  private nextId = 1;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private readonly onEvent: AgentClientOptions["onEvent"];
  private readonly onResponse: AgentClientOptions["onResponse"];
  private readonly onStatus: AgentClientOptions["onStatus"];

  constructor({ onEvent, onResponse, onStatus }: AgentClientOptions) {
    this.onEvent = onEvent;
    this.onResponse = onResponse;
    this.onStatus = onStatus;
    this.connect();
  }

  connect(): void {
    this.setStatus("connecting");
    const ws = new WebSocket(DEFAULT_WS);
    this.ws = ws;
    ws.onopen = () => this.setStatus("connected");
    ws.onclose = () => {
      this.setStatus("disconnected");
      this.reconnectTimer = setTimeout(() => this.connect(), 1000);
    };
    ws.onerror = () => ws.close();
    ws.onmessage = (ev) => {
      let frame: RpcFrame;
      try {
        frame = JSON.parse(ev.data as string) as RpcFrame;
      } catch {
        return;
      }
      if (frame.type === "event") this.onEvent(frame);
      else this.onResponse(frame);
    };
  }

  private setStatus(status: ConnStatus): void {
    if (this.onStatus) this.onStatus(status);
  }

  send(method: RpcMethod): number | null {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return null;
    const request: RpcRequest = { id: this.nextId++, method };
    this.ws.send(JSON.stringify(request));
    return request.id;
  }

  /** 关闭连接并停止自动重连（组件卸载时调用）。 */
  dispose(): void {
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    if (this.ws) {
      this.ws.onclose = null;
      this.ws.close();
      this.ws = null;
    }
  }

  // ---- 通用 RPC 方法子集（M4 定型）----

  sendUserMessage(text: string, sessionKey: string | null = null): number | null {
    return this.send({ type: "send_user_message", session_key: sessionKey, text });
  }

  listSessions(): number | null {
    return this.send({ type: "list_sessions" });
  }

  readSession(sessionKey: string): number | null {
    return this.send({ type: "read_session", session_key: sessionKey });
  }

  editMessage(sessionKey: string, messageId: string, text: string): number | null {
    return this.send({ type: "edit_message", session_key: sessionKey, message_id: messageId, text });
  }

  switchBranch(sessionKey: string, messageId: string): number | null {
    return this.send({ type: "switch_branch", session_key: sessionKey, message_id: messageId });
  }

  triggerCommand(entry: string, params: unknown = {}): number | null {
    return this.send({ type: "trigger_command", entry, params });
  }
}
