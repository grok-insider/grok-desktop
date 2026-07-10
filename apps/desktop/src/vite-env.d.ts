/// <reference types="vite/client" />

import type { DesktopBridge } from "./contracts/bridge";

interface ImportMetaEnv {
  readonly VITE_BROWSER_PREVIEW?: "true";
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}

declare global {
  interface Window {
    grokDesktop?: DesktopBridge;
  }
}

export {};
