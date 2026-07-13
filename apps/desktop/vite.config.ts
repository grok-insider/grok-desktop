import { fileURLToPath, URL } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";
import { rendererContentSecurityPolicy } from "./electron/rendererSecurityPolicy.js";

export function contentSecurityPolicy(development: boolean): string {
  return rendererContentSecurityPolicy(development, "header");
}

export default defineConfig(({ command }) => ({
  plugins: [react(), tailwindcss(), {
    name: "grok-desktop-csp",
    transformIndexHtml: {
      order: "pre",
      handler: () => [{
        tag: "meta",
        attrs: {
          "http-equiv": "Content-Security-Policy",
          content: rendererContentSecurityPolicy(command === "serve", "meta"),
        },
        injectTo: "head-prepend",
      }],
    },
  }],
  base: "./",
  resolve: {
    alias: { "@": fileURLToPath(new URL("./src", import.meta.url)) },
  },
  build: {
    outDir: "dist",
    sourcemap: false,
  },
  server: {
    strictPort: true,
  },
  test: {
    environment: "jsdom",
    setupFiles: "./src/test/setup.ts",
    exclude: ["dist/**", "dist-electron/**", "node_modules/**", "scripts/**/*.test.mjs"],
  },
}));
