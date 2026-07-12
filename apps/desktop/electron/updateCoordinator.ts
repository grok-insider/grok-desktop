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

export interface LinuxAppImageUpdater {
  download(): Promise<boolean>;
}

const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1_000;
const INITIAL_CHECK_DELAY_MS = 30 * 1_000;

export class UpdateCoordinator {
  private state: DesktopUpdateState;
  private checkTimer: ReturnType<typeof setInterval> | undefined;
  private initialCheckTimer: ReturnType<typeof setTimeout> | undefined;
  private readonly linuxUpdater: LinuxAppImageUpdater | undefined;
  private readonly restart: (() => void) | undefined;

  constructor(
    private readonly updater: NativeAutoUpdater | undefined,
    options: {
      packaged: boolean;
      platform: NodeJS.Platform;
      architecture: string;
      version: string;
      linuxUpdater?: LinuxAppImageUpdater;
      restart?: () => void;
    },
  ) {
    this.linuxUpdater = options.linuxUpdater;
    this.restart = options.restart;
    const supported = options.packaged && (
      (options.platform === "win32" && (options.architecture === "x64" || options.architecture === "arm64") && updater)
      || (options.platform === "linux" && options.linuxUpdater && options.restart)
    );
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
    if (updater && options.platform === "win32") {
      updater.setFeedURL({
        url: `https://github.com/grok-insider/grok-desktop/releases/latest/download/GrokDesktop-stable-${options.architecture}.msix`,
        allowAnyVersion: false,
      });
      this.bindEvents(updater);
    }
  }

  getState(): DesktopUpdateState {
    return structuredClone(this.state);
  }

  start(): void {
    if ((!this.updater && !this.linuxUpdater) || this.state.phase === "unsupported"
        || this.checkTimer || this.initialCheckTimer) return;
    this.initialCheckTimer = setTimeout(() => {
      this.initialCheckTimer = undefined;
      this.check();
    }, INITIAL_CHECK_DELAY_MS);
    this.initialCheckTimer.unref?.();
    this.checkTimer = setInterval(() => this.check(), CHECK_INTERVAL_MS);
    this.checkTimer.unref?.();
  }

  stop(): void {
    if (this.initialCheckTimer) clearTimeout(this.initialCheckTimer);
    if (this.checkTimer) clearInterval(this.checkTimer);
    this.initialCheckTimer = undefined;
    this.checkTimer = undefined;
  }

  check(): DesktopUpdateState {
    if ((!this.updater && !this.linuxUpdater) || this.state.phase === "unsupported") return this.getState();
    if (this.state.phase === "checking" || this.state.phase === "available") return this.getState();
    this.state = { ...this.state, phase: "checking", reasonCode: "" };
    if (this.linuxUpdater) {
      void this.linuxUpdater.download().then((changed) => {
        this.state = {
          ...this.state,
          phase: changed ? "downloaded" : "not_available",
          checkedAtUnixMs: Date.now(),
          targetVersion: changed ? "latest" : "",
          reasonCode: "",
        };
      }, () => this.fail());
    } else {
      try {
        this.updater?.checkForUpdates();
      } catch {
        this.fail();
      }
    }
    return this.getState();
  }

  install(): boolean {
    if (this.state.phase !== "downloaded") return false;
    if (this.linuxUpdater && this.restart) this.restart();
    else if (this.updater) this.updater.quitAndInstall();
    else return false;
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
