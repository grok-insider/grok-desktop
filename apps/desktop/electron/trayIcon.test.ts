import path from "node:path";
import { describe, expect, it } from "vitest";
import { resolveTrayIconPath } from "./trayIcon.js";

describe("tray icon resolution", () => {
  it("uses the development app asset on Linux", () => {
    const expected = path.join("/app", "assets", "tray", "tray-dark-24.png");
    expect(resolveTrayIconPath("/app", "/resources", "linux", "dark", (candidate) => candidate === expected)).toBe(expected);
  });

  it("falls back to packaged resources on Windows", () => {
    const expected = path.join("C:\\resources", "tray", "tray-light.ico");
    expect(resolveTrayIconPath("C:\\app.asar", "C:\\resources", "win32", "light", (candidate) => candidate === expected)).toBe(expected);
  });

  it("fails clearly for a missing asset or unsupported platform", () => {
    expect(() => resolveTrayIconPath("/app", "/resources", "linux", "light", () => false)).toThrow("canonical tray icon asset is missing (tray-light-24.png)");
    expect(() => resolveTrayIconPath("/app", "/resources", "darwin", "light", () => true)).toThrow("system tray is unsupported on darwin");
  });
});
