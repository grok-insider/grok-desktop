import { fileURLToPath, URL } from "node:url";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";

export function contentSecurityPolicy(development: boolean): string {
  const developmentConnect = development ? " ws://127.0.0.1:*" : "";
  const developmentStyle = development ? " 'unsafe-inline'" : "";
  return `default-src 'self'; script-src 'self'; style-src 'self'${developmentStyle}; style-src-attr 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self'; connect-src 'self'${developmentConnect}; object-src 'none'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'`;
}

export default defineConfig(({ command }) => ({
  plugins: [react(), tailwindcss(), {
    name: "grok-desktop-csp",
    transformIndexHtml: {
      order: "pre",
      handler: () => [{
        tag: "meta",
        attrs: { "http-equiv": "Content-Security-Policy", content: contentSecurityPolicy(command === "serve") },
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
