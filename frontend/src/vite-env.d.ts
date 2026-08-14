/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** sl-agent API 的 WS 地址（部署时指向实际后端）。 */
  readonly VITE_SL_AGENT_WS?: string;
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
