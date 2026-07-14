import { fileURLToPath, URL } from "node:url";
import { readFileSync } from "node:fs";
import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { defineConfig } from "vitest/config";
import { rendererContentSecurityPolicy } from "./electron/rendererSecurityPolicy.js";

const desktopVersion = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8")).version as string;

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
  define: { "import.meta.env.VITE_APP_VERSION": JSON.stringify(desktopVersion) },
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
