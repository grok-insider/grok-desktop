// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import { withStartupDeadline } from "./startupDeadline.js";

describe("withStartupDeadline", () => {
  it("returns an operation that settles before its deadline", async () => {
    await expect(withStartupDeadline(Promise.resolve("ready"), 100)).resolves.toBe("ready");
  });

  it("rejects a stalled operation at the exact bounded deadline", async () => {
    vi.useFakeTimers();
    const result = withStartupDeadline(new Promise<never>(() => undefined), 2_500);
    const assertion = expect(result).rejects.toThrow("startup deadline exceeded");
    await vi.advanceTimersByTimeAsync(2_500);
    await assertion;
    vi.useRealTimers();
  });
});
