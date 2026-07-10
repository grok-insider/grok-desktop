import { describe, expect, it } from "vitest";
import { shouldDeferAppQuit, shouldHideWindowOnClose } from "./windowClosePolicy.js";

describe("window close policy", () => {
  it("hides normal closes by default", () => {
    expect(shouldHideWindowOnClose(true, false)).toBe(true);
  });

  it("allows close when the preference is disabled or explicit shutdown began", () => {
    expect(shouldHideWindowOnClose(false, false)).toBe(false);
    expect(shouldHideWindowOnClose(true, true)).toBe(false);
  });

  it("keeps repeated quit requests deferred until daemon shutdown completes", () => {
    expect(shouldDeferAppQuit(true, false)).toBe(true);
    expect(shouldDeferAppQuit(true, true)).toBe(false);
    expect(shouldDeferAppQuit(false, false)).toBe(false);
  });
});
