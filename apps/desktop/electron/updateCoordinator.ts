export type DesktopUpdatePhase =
  | "unsupported"
  | "idle"
  | "checking"
  | "available"
  | "downloaded"
  | "not_available"
  | "failed";

export interface DesktopUpdateState {
  phase: DesktopUpdatePhase;
  currentVersion: string;
  targetVersion: string;
  channel: "stable";
  checkedAtUnixMs: number;
  reasonCode: "" | "development_install" | "platform_unsupported" | "check_failed";
}

export interface NativeAutoUpdater {
  on(event: "checking-for-update" | "update-available" | "update-not-available" | "update-downloaded" | "error", listener: (...arguments_: unknown[]) => void): this;
  setFeedURL(options: { url: string; allowAnyVersion: false }): void;
  checkForUpdates(): void;
  quitAndInstall(): void;
}

const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1_000;

export class UpdateCoordinator {
  private state: DesktopUpdateState;
  private checkTimer: ReturnType<typeof setInterval> | undefined;

  constructor(
    private readonly updater: NativeAutoUpdater | undefined,
    options: { packaged: boolean; platform: NodeJS.Platform; architecture: string; version: string },
  ) {
    const supported = options.packaged && options.platform === "win32"
      && (options.architecture === "x64" || options.architecture === "arm64") && updater;
    this.state = {
      phase: supported ? "idle" : "unsupported",
      currentVersion: options.version,
      targetVersion: "",
      channel: "stable",
      checkedAtUnixMs: 0,
      reasonCode: options.packaged ? "platform_unsupported" : "development_install",
    };
    if (!supported) return;
    this.state.reasonCode = "";
    updater.setFeedURL({
      url: `https://github.com/grok-insider/grok-desktop/releases/latest/download/GrokDesktop-stable-${options.architecture}.msix`,
      allowAnyVersion: false,
    });
    this.bindEvents(updater);
  }

  getState(): DesktopUpdateState {
    return structuredClone(this.state);
  }

  start(): void {
    if (!this.updater || this.state.phase === "unsupported" || this.checkTimer) return;
    this.checkTimer = setInterval(() => this.check(), CHECK_INTERVAL_MS);
    this.checkTimer.unref?.();
  }

  stop(): void {
    if (this.checkTimer) clearInterval(this.checkTimer);
    this.checkTimer = undefined;
  }

  check(): DesktopUpdateState {
    if (!this.updater || this.state.phase === "unsupported") return this.getState();
    if (this.state.phase === "checking" || this.state.phase === "available") return this.getState();
    this.state = { ...this.state, phase: "checking", reasonCode: "" };
    try {
      this.updater.checkForUpdates();
    } catch {
      this.fail();
    }
    return this.getState();
  }

  install(): boolean {
    if (!this.updater || this.state.phase !== "downloaded") return false;
    this.updater.quitAndInstall();
    return true;
  }

  private bindEvents(updater: NativeAutoUpdater): void {
    updater.on("checking-for-update", () => {
      this.state = { ...this.state, phase: "checking", reasonCode: "" };
    });
    updater.on("update-available", (...arguments_) => {
      this.state = { ...this.state, phase: "available", targetVersion: eventVersion(arguments_[0]), reasonCode: "" };
    });
    updater.on("update-not-available", () => {
      this.state = { ...this.state, phase: "not_available", checkedAtUnixMs: Date.now(), targetVersion: "", reasonCode: "" };
    });
    updater.on("update-downloaded", (...arguments_) => {
      this.state = { ...this.state, phase: "downloaded", checkedAtUnixMs: Date.now(), targetVersion: eventVersion(arguments_[1]) || this.state.targetVersion, reasonCode: "" };
    });
    updater.on("error", () => this.fail());
  }

  private fail(): void {
    this.state = { ...this.state, phase: "failed", checkedAtUnixMs: Date.now(), reasonCode: "check_failed" };
  }
}

function eventVersion(value: unknown): string {
  if (!value || typeof value !== "object") return "";
  const candidate = "version" in value ? value.version : "releaseName" in value ? value.releaseName : "";
  return typeof candidate === "string" && candidate.length <= 64 && /^[0-9A-Za-z.-]+$/.test(candidate)
    ? candidate
    : "";
}
