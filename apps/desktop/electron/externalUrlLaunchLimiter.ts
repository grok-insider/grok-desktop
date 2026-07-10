const DEFAULT_WINDOW_MS = 10_000;
const DEFAULT_MAX_LAUNCHES = 4;

/**
 * Bounds renderer-triggered shell launches even when a compromised renderer
 * invokes the preload contract without a physical user gesture.
 */
export class ExternalUrlLaunchLimiter {
  private active = false;
  private launches: number[] = [];

  constructor(
    private readonly maximumLaunches = DEFAULT_MAX_LAUNCHES,
    private readonly windowMs = DEFAULT_WINDOW_MS,
  ) {
    if (!Number.isSafeInteger(maximumLaunches) || maximumLaunches < 1) {
      throw new TypeError("external URL launch limit must be a positive integer");
    }
    if (!Number.isSafeInteger(windowMs) || windowMs < 1) {
      throw new TypeError("external URL launch window must be a positive integer");
    }
  }

  tryAcquire(nowUnixMs = Date.now()): (() => void) | undefined {
    if (!Number.isSafeInteger(nowUnixMs) || nowUnixMs < 0) return undefined;
    this.launches = this.launches.filter((launchedAt) => (
      launchedAt <= nowUnixMs && nowUnixMs - launchedAt < this.windowMs
    ));
    if (this.active || this.launches.length >= this.maximumLaunches) return undefined;

    this.active = true;
    this.launches.push(nowUnixMs);
    let released = false;
    return () => {
      if (released) return;
      released = true;
      this.active = false;
    };
  }
}
