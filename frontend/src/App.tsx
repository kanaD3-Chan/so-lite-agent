import { useCallback, useEffect, useRef, useState } from "react";
import { AgentClient, type ConnStatus } from "./agentClient";
import type { Message, RpcFrame, SessionMeta } from "./protocol";

// ---- 工具面板条目（ToolStart/ToolProgress/ToolEnd 聚合）----

interface ToolPanelEntry {
  entry: string;
  state: "running" | "ok" | "err";
  progress: string[];
  icon?: string | null;
}

// ---- 消息气泡 ----

interface Bubble {
  id: string;
  kind: "user" | "assistant" | "reasoning" | "tool" | "system" | "error";
  text: string;
}

type PendingKind = "list" | "read" | "send";

export default function App() {
  const [status, setStatus] = useState<ConnStatus>("connecting");
  const [sessions, setSessions] = useState<SessionMeta[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [bubbles, setBubbles] = useState<Bubble[]>([]);
  const [tools, setTools] = useState<ToolPanelEntry[]>([]);
  const [input, setInput] = useState("");
  const clientRef = useRef<AgentClient | null>(null);
  const pendingRef = useRef<Map<number, PendingKind>>(new Map());
  const activeRef = useRef<string | null>(null);
  const listEndRef = useRef<HTMLDivElement | null>(null);

  // 会话切换：读会话消息 → 全量渲染历史（流式只对新回合增量）。
  const openSession = useCallback((client: AgentClient, key: string) => {
    activeRef.current = key;
    setActiveKey(key);
    const id = client.readSession(key);
    if (id !== null) pendingRef.current.set(id, "read");
  }, []);

  const handleEvent = useCallback((frame: Extract<RpcFrame, { type: "event" }>) => {
    const ev = frame.event;
    switch (ev.event) {
      case "message_delta": {
        setBubbles((prev) => {
          const last = prev[prev.length - 1];
          if (last && last.kind === "assistant" && last.id === ev.message_id) {
            const copy = [...prev];
            copy[copy.length - 1] = { ...last, text: last.text + ev.delta };
            return copy;
          }
          return [...prev, { id: ev.message_id, kind: "assistant", text: ev.delta }];
        });
        break;
      }
      case "reasoning_delta": {
        setBubbles((prev) => {
          const last = prev[prev.length - 1];
          if (last && last.kind === "reasoning") {
            const copy = [...prev];
            copy[copy.length - 1] = { ...last, text: last.text + ev.delta };
            return copy;
          }
          return [...prev, { id: `r${Date.now()}`, kind: "reasoning", text: ev.delta }];
        });
        break;
      }
      case "tool_start": {
        setTools((prev) => [
          ...prev.filter((t) => t.entry !== ev.entry),
          { entry: ev.entry, state: "running", progress: [], icon: ev.icon },
        ]);
        break;
      }
      case "tool_progress": {
        setTools((prev) =>
          prev.map((t) =>
            t.entry === ev.entry ? { ...t, progress: [...t.progress, ev.message] } : t,
          ),
        );
        break;
      }
      case "tool_end": {
        setTools((prev) =>
          prev.map((t) => (t.entry === ev.entry ? { ...t, state: ev.ok ? "ok" : "err" } : t)),
        );
        break;
      }
      case "turn_end": {
        setTools((prev) => prev.map((t) => (t.state === "running" ? { ...t, state: "ok" } : t)));
        break;
      }
      case "error":
        setBubbles((prev) => [...prev, { id: `e${Date.now()}`, kind: "error", text: ev.message }]);
        break;
      default:
        console.log("event", ev);
    }
  }, []);

  const handleResponse = useCallback((frame: Extract<RpcFrame, { type: "response" }>) => {
    const kind = pendingRef.current.get(frame.id);
    if (frame.error) {
      setBubbles((prev) => [
        ...prev,
        { id: `e${Date.now()}`, kind: "error", text: frame.error!.message },
      ]);
      return;
    }
    if (kind === "list") {
      const list = frame.result as SessionMeta[] | undefined;
      if (Array.isArray(list)) {
        setSessions(list);
        // 自动打开第一个会话（无活动会话时）。
        if (activeRef.current === null && list.length > 0 && clientRef.current) {
          openSession(clientRef.current, list[0].key);
        }
      }
    } else if (kind === "read") {
      const msgs = frame.result as Message[] | undefined;
      if (Array.isArray(msgs)) {
        const rendered = msgs.map((m) => ({
          id: m.id,
          kind: bubbleKindOf(m),
          text: messageTextOf(m),
        }));
        setBubbles(rendered);
      }
    }
    // "send" 回执：事件流已实时渲染，无需处理（兜底见 handleEvent 的流式路径）。
  }, [openSession]);

  useEffect(() => {
    const client = new AgentClient({
      onEvent: handleEvent,
      onResponse: handleResponse,
      onStatus: setStatus,
    });
    clientRef.current = client;
    // 首拉会话列表（打 pending 标记）。
    const id = client.listSessions();
    if (id !== null) pendingRef.current.set(id, "list");
    return () => {
      client.dispose();
      clientRef.current = null;
    };
  }, [handleEvent, handleResponse]);

  const sendMessage = () => {
    const text = input.trim();
    const client = clientRef.current;
    if (!text || !client) return;
    setBubbles((prev) => [...prev, { id: `u${Date.now()}`, kind: "user", text }]);
    const id = client.sendUserMessage(text, activeRef.current);
    if (id !== null) pendingRef.current.set(id, "send");
    setInput("");
  };

  useEffect(() => {
    listEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [bubbles]);

  const statusLabel =
    status === "connected" ? "已连接" : status === "connecting" ? "连接中…" : "已断开，重连中…";

  return (
    <div className="app">
      <header className="app-header">
        <h1>sl-agent</h1>
        <span className={`status ${status}`}>{statusLabel}</span>
      </header>
      <div className="app-body">
        <aside className="sidebar">
          <h2>会话</h2>
          <ul className="session-list">
            {sessions.map((s) => (
              <li key={s.key}>
                <button
                  className={s.key === activeKey ? "active" : ""}
                  onClick={() => clientRef.current && openSession(clientRef.current, s.key)}
                >
                  <span className="session-time">
                    {new Date(s.last_activity_at).toLocaleString()}
                  </span>
                  {s.goal ? s.goal.text : `会话 ${s.key.slice(0, 8)}`}
                </button>
              </li>
            ))}
            {sessions.length === 0 && <li className="empty">暂无会话，发送消息自动创建</li>}
          </ul>
          <h2>工具</h2>
          <ul className="tool-panel">
            {tools.map((t) => (
              <li key={t.entry} className={t.state}>
                <span className="tool-icon">
                  {t.state === "running" ? "⏳" : t.state === "ok" ? "✅" : "❌"}
                </span>
                <span className="tool-name">{t.entry}</span>
                {t.progress.length > 0 && (
                  <ul className="tool-progress">
                    {t.progress.map((p, i) => (
                      <li key={i}>{p}</li>
                    ))}
                  </ul>
                )}
              </li>
            ))}
            {tools.length === 0 && <li className="empty">尚无工具调用</li>}
          </ul>
        </aside>
        <main className="chat">
          <div className="messages">
            {bubbles.map((b) => (
              <div key={b.id} className={`bubble ${b.kind}`}>
                {b.text}
              </div>
            ))}
            <div ref={listEndRef} />
          </div>
          <footer className="composer">
            <input
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => e.key === "Enter" && sendMessage()}
              placeholder="输入消息，回车发送"
            />
            <button onClick={sendMessage}>发送</button>
          </footer>
        </main>
      </div>
    </div>
  );
}

function bubbleKindOf(m: Message): Bubble["kind"] {
  switch (m.kind.type) {
    case "user":
      return "user";
    case "assistant":
      return "assistant";
    case "reasoning":
      return "reasoning";
    case "tool_call":
      return "tool";
    case "system":
      return "system";
  }
}

function messageTextOf(m: Message): string {
  switch (m.kind.type) {
    case "user":
      return m.kind.display_text ?? m.kind.text;
    case "assistant":
    case "system":
      return m.kind.text;
    case "reasoning":
      return m.kind.text;
    case "tool_call":
      return `🔧 ${m.kind.entry}\n${JSON.stringify(m.kind.params, null, 2)}`;
  }
}
