// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import { focusPrimaryWindow } from "./instancePolicy.js";

describe("single instance window policy", () => {
  it("restores and focuses the existing primary window", () => {
    const window = { isMinimized: () => true, restore: vi.fn(), show: vi.fn(), focus: vi.fn() };
    expect(focusPrimaryWindow([window])).toBe(true);
    expect(window.restore).toHaveBeenCalledOnce();
    expect(window.show).toHaveBeenCalledOnce();
    expect(window.focus).toHaveBeenCalledOnce();
  });

  it("does nothing when the primary window is not ready", () => {
    expect(focusPrimaryWindow([])).toBe(false);
  });
});
