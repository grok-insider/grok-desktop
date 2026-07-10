import { realpathSync, statSync } from "node:fs";
import path from "node:path";

export interface ProtocolAsset {
  file: string;
  contentType: string;
}

/** Resolves an application-scheme URL without permitting traversal or symlink escape. */
export function resolveProtocolAsset(distributionRoot: string, requestUrl: string): ProtocolAsset | null {
  try {
    const url = new URL(requestUrl);
    if (url.protocol !== "grok-desktop:" || url.hostname !== "app" || url.username || url.password) return null;
    const decoded = decodeURIComponent(url.pathname);
    if (decoded.includes("\0") || decoded.includes("\\")) return null;
    const relative = decoded === "/" ? "index.html" : decoded.replace(/^\/+/, "");
    if (!relative) return null;
    const canonicalRoot = realpathSync.native(distributionRoot);
    const candidate = realpathSync.native(path.resolve(canonicalRoot, relative));
    const boundary = path.relative(canonicalRoot, candidate);
    if (boundary === ".." || boundary.startsWith(`..${path.sep}`) || path.isAbsolute(boundary)) return null;
    if (!statSync(candidate).isFile()) return null;
    return { file: candidate, contentType: contentType(candidate) };
  } catch {
    return null;
  }
}

export function contentType(file: string): string {
  const values: Record<string, string> = {
    ".html": "text/html; charset=utf-8",
    ".js": "text/javascript; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".css": "text/css; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".map": "application/json; charset=utf-8",
    ".woff2": "font/woff2",
    ".png": "image/png",
    ".jpg": "image/jpeg",
    ".jpeg": "image/jpeg",
    ".webp": "image/webp",
    ".svg": "image/svg+xml",
  };
  return values[path.extname(file).toLowerCase()] ?? "application/octet-stream";
}
