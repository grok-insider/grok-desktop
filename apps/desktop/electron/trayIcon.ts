import path from "node:path";

export type TrayTheme = "dark" | "light";

/** Resolves only canonical, packaged tray assets and fails closed when none exists. */
export function resolveTrayIconPath(
  appPath: string,
  resourcesPath: string,
  platform: NodeJS.Platform,
  theme: TrayTheme,
  exists: (candidate: string) => boolean,
): string {
  if (platform !== "win32" && platform !== "linux") {
    throw new Error(`system tray is unsupported on ${platform}`);
  }
  const file = platform === "win32" ? `tray-${theme}.ico` : `tray-${theme}-24.png`;
  const candidates = [
    path.join(resourcesPath, "tray", file),
    path.join(resourcesPath, "assets", "tray", file),
    path.join(appPath, "assets", "tray", file),
  ];
  const resolved = candidates.find(exists);
  if (!resolved) {
    throw new Error(`canonical tray icon asset is missing (${file})`);
  }
  return resolved;
}
