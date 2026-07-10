// @vitest-environment node
import { describe, expect, it } from "vitest";
import { ExternalUrlLaunchLimiter } from "./externalUrlLaunchLimiter.js";

describe("ExternalUrlLaunchLimiter", () => {
  it("allows only one shell launch at a time", () => {
    const limiter = new ExternalUrlLaunchLimiter();
    const release = limiter.tryAcquire(1_000);
    expect(release).toBeTypeOf("function");
    expect(limiter.tryAcquire(1_001)).toBeUndefined();
    release?.();
    expect(limiter.tryAcquire(1_002)).toBeTypeOf("function");
  });

  it("bounds launch attempts in a rolling window", () => {
    const limiter = new ExternalUrlLaunchLimiter(2, 1_000);
    limiter.tryAcquire(1_000)?.();
    limiter.tryAcquire(1_100)?.();
    expect(limiter.tryAcquire(1_999)).toBeUndefined();
    expect(limiter.tryAcquire(2_000)).toBeTypeOf("function");
  });

  it("rejects invalid time input and configuration", () => {
    const limiter = new ExternalUrlLaunchLimiter();
    expect(limiter.tryAcquire(-1)).toBeUndefined();
    expect(() => new ExternalUrlLaunchLimiter(0, 1)).toThrow();
    expect(() => new ExternalUrlLaunchLimiter(1, 0)).toThrow();
  });
});
