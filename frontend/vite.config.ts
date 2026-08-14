import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// sl-agent 前端：独立工程（ADR-0010）。dev server 默认 5173；
// WS 后端地址经 import.meta.env.VITE_SL_AGENT_WS 覆盖（默认 ws://127.0.0.1:8080/ws）。
export default defineConfig({
  plugins: [react()],
  server: {
    port: 5173,
  },
});

