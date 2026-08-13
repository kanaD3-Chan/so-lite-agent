// sl-agent 最小聊天页（P1 纯 JS；协议复用通用 RPC 帧：RpcRequest / RpcFrame）。
// 服务端：HTTP 静态页 + WS（请求→Response 回执；kernel 事件→Event 帧推送）。

const wsUrl = (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/ws";
const messages = document.getElementById("messages");
const input = document.getElementById("input");
const sendBtn = document.getElementById("send");
const statusEl = document.getElementById("status");

let ws = null;
let nextId = 1;
let currentAssistant = null;
let currentReasoning = null;

function connect() {
  setStatus("connecting", "连接中…");
  ws = new WebSocket(wsUrl);
  ws.onopen = () => setStatus("connected", "已连接");
  ws.onclose = () => {
    setStatus("disconnected", "已断开，重连中…");
    currentAssistant = null;
    currentReasoning = null;
    setTimeout(connect, 1000);
  };
  ws.onerror = () => ws.close();
  ws.onmessage = (ev) => {
    let frame;
    try {
      frame = JSON.parse(ev.data);
    } catch {
      return;
    }
    if (frame.type === "event") handleEvent(frame.event);
    else if (frame.type === "response") handleResponse(frame);
  };
}

function setStatus(kind, text) {
  statusEl.className = kind;
  statusEl.textContent = text;
}

function handleEvent(event) {
  switch (event.event) {
    case "message_delta": {
      if (!currentAssistant) currentAssistant = addBubble("assistant");
      currentAssistant.textContent += event.delta;
      scroll();
      break;
    }
    case "reasoning_delta": {
      if (!currentReasoning) currentReasoning = addBubble("reasoning");
      currentReasoning.textContent += event.delta;
      scroll();
      break;
    }
    case "tool_start":
      addLine("tool", "🔧 " + event.entry + (event.icon ? " " + event.icon : ""));
      break;
    case "tool_end":
      addLine("tool", (event.ok ? "✅ " : "❌ ") + event.entry);
      break;
    case "tool_progress":
      addLine("tool", "⏳ " + event.entry + "：" + event.message);
      break;
    case "turn_end":
      currentAssistant = null;
      currentReasoning = null;
      break;
    case "error":
      addLine("error", "错误：" + event.message);
      break;
    default:
      // 业务事件（Event::Custom 等）P1 只打日志，前端工程化后再做面板。
      console.log("event", event);
  }
}

function handleResponse(frame) {
  if (frame.error) {
    addLine("error", "错误：" + frame.error.message);
    return;
  }
  // 事件流已实时渲染；这里兜底：断线重连后补渲染结果消息。
  const outcome = frame.result;
  if (outcome && Array.isArray(outcome.messages) && !currentAssistant) {
    for (const msg of outcome.messages) {
      const kind = msg.kind && msg.kind.type;
      if (kind === "assistant") {
        const text = msg.kind.text || "";
        if (text) addBubble("assistant").textContent = text;
      }
    }
  }
}

function send(text) {
  if (!text.trim()) return;
  if (!ws || ws.readyState !== WebSocket.OPEN) {
    addLine("error", "未连接，请稍候重试");
    return;
  }
  addBubble("user").textContent = text;
  input.value = "";
  const request = { id: nextId++, method: { type: "send_user_message", text } };
  ws.send(JSON.stringify(request));
}

function addBubble(kind) {
  const div = document.createElement("div");
  div.className = "bubble " + kind;
  messages.appendChild(div);
  scroll();
  return div;
}

function addLine(kind, text) {
  const div = document.createElement("div");
  div.className = "line " + kind;
  div.textContent = text;
  messages.appendChild(div);
  scroll();
}

function scroll() {
  messages.scrollTop = messages.scrollHeight;
}

sendBtn.onclick = () => send(input.value);
input.onkeydown = (e) => {
  if (e.key === "Enter") send(input.value);
};

connect();
