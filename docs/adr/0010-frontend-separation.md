# 前后端分离：sl-agent 纯 API 服务，前端独立仓库（React）

**协作决策（2026-08-14）**：sl-agent **不再内嵌前端**（移除 rust-embed 静态服务），
改为**纯 API 服务**（HTTP/WS）；前端拆为**独立 React 工程**（`frontend/`，官方参考
实现），其他人 fork 或自建前端"自己看着办"。这是对 ADR-0006「浏览器 Web 应用
形态（内嵌前端）」的修订——形态保留（仍是浏览器 GUI 交付），**内嵌改为分离**。

## 决策内容

- **sl-agent = API 服务**：只提供 `/ws`（RPC/事件）+ `/healthz`；不服务静态文件、
  不内嵌前端。前端自己起 dev server 或静态托管（nginx/GitHub Pages/任意），
  经 WS 连 sl-agent；
- **前端 = 独立工程**：`frontend/` 目录，React + TypeScript（类型安全覆盖 WS
  协议帧）；协议契约在 `frontend/src/protocol.ts`（对应 Rust 侧
  `rpc.rs` / `events.rs`）。官方参考实现只保证"能用的 GUI"（聊天流式 + 会话列表
  + 工具面板）；fork/自建前端 = 连同一 WS 协议、技术栈自选；
- **CORS/来源**：WS 握手回 `Access-Control-Allow-Origin: *`（API 公开，任意来源
  可连）；`VITE_SL_AGENT_WS` 注入后端地址（前端不硬编码）。

## 动机

- **前端演进独立于内核**：GUI 工程化（组件、样式、交互）不该阻塞 Rust 内核迭代，
  反之亦然；内嵌把两者绑死（前端升级 = 重编二进制）；
- **对齐 DSH 形态**：DSH 的 web 是独立应用（apps/web），CLI 不内嵌；前端独立
  是通用 agent 运行时 GUI 的成熟组织方式；
- **"自己看着办"**：第三方基于 sl-agent 做自己的前端（技术栈、UI 全自由），
  只认 WS 协议；官方不强制统一前端技术栈（用户明确否了 Vue，选 React 作参考实现）。

## 影响与边界

- **Rust**：`server` feature 去掉 `rust-embed`；sl-agent 路由只剩 `/ws` + `/healthz`；
  `web/`（P1 纯 JS 最小聊天页）退役，前端迁至 `frontend/`（React+TS）；
- **文档**：README / agent-dev-guide 更新启动方式（前后端两个进程）；
  plan.md P2 GUI 长全 = 前端工程化（React 参考实现）✅；
- **单二进制分发**：不再成立（ADR-0006 曾设想的"单二进制含 GUI"被本决策取代）——
  sl-agent 是可执行 API 服务，前端独立部署；想打包单体的可用 Tauri 等壳
  （把 frontend/dist + sl-agent 包一起，由使用方自行决定）；
- **后端**：WS 协议不变（RpcRequest/RpcFrame），前端帧契约零改动。

## 被否备选

- **维持内嵌（rust-embed）**：前端升级锁死内核迭代，无独立演进；
- **内嵌参考版 + SL_AGENT_WEB_DIR 磁盘可替换**：保留内嵌兜底，但仍把前端绑在
  仓库内、且二进制带一份冗余 UI——用户选择更彻底的分离；
- **前端用 Vue**：用户明确否（选用 React 作参考实现）。
