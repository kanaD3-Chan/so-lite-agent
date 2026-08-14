# sl-agent 前端（React + TypeScript）

前后端分离（ADR-0010）：本工程是 **sl-agent 的独立前端**，不内嵌进二进制；
sl-agent 只提供 API（WS），前端自己起 dev server 或静态托管。

## 启动

```bash
cd frontend
npm install
npm run dev          # http://localhost:5173
```

另开终端起后端（见仓库根 README）：

```bash
cargo run -p sl-agent
```

浏览器打开 `http://localhost:5173` 即连 `ws://127.0.0.1:8080/ws`。

## 配置

| 环境变量 | 默认 | 说明 |
|---|---|---|
| `VITE_SL_AGENT_WS` | `ws://127.0.0.1:8080/ws` | sl-agent API 的 WS 地址（部署时指向实际后端） |

## 功能

- 聊天流式渲染（message_delta / reasoning_delta，打字机增量）；
- 会话列表（list_sessions / read_session，切换会话读历史）；
- 工具面板（ToolStart / ToolProgress / ToolEnd 聚合展示）；
- 断线自动重连（重连后重拉会话列表）。

## 协议

WS 帧复用 crate 通用 RPC（`RpcRequest` / `RpcFrame`，见 `docs/api.md`）；
类型定义在 `src/protocol.ts`（与 Rust 侧 `rpc.rs` / `events.rs` 对应）。

## 构建

```bash
npm run build        # 产物 dist/，可静态托管（nginx / GitHub Pages 等）
npm run typecheck    # tsc --noEmit
```

## 独立前端仓库约定

本工程是官方**参考实现**。fork 或自建前端时：

- 连同一 WS 协议（`src/protocol.ts` 即协议契约），技术栈自选（React/Vue/vanilla 均可）；
- 后端地址经 `VITE_SL_AGENT_WS` 注入，不硬编码；
- 服务端不设 CORS 白名单（WS 回 `Access-Control-Allow-Origin: *`），任意来源可连。
