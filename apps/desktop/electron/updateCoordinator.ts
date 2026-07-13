import type { AuthorizedUpdate, UpdateAuthorizer } from "./updateManifestVerifier.js";

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
  channel: "stable" | "beta";
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
  download(expected?: AuthorizedUpdate["artifact"]): Promise<boolean>;
}

const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1_000;
const INITIAL_CHECK_DELAY_MS = 30 * 1_000;

export class UpdateCoordinator {
  private state: DesktopUpdateState;
  private checkTimer: ReturnType<typeof setInterval> | undefined;
  private initialCheckTimer: ReturnType<typeof setTimeout> | undefined;
  private readonly linuxUpdater: LinuxAppImageUpdater | undefined;
  private readonly restart: (() => void) | undefined;
  private readonly authorizer: UpdateAuthorizer | undefined;
  private authorizedVersion = "";
  private channelGeneration = 0;

  constructor(
    private readonly updater: NativeAutoUpdater | undefined,
    options: {
      packaged: boolean;
      platform: NodeJS.Platform;
      architecture: string;
      version: string;
      linuxUpdater?: LinuxAppImageUpdater;
      restart?: () => void;
      authorizer?: UpdateAuthorizer;
      channel?: "stable" | "beta";
    },
  ) {
    this.linuxUpdater = options.linuxUpdater;
    this.restart = options.restart;
    this.authorizer = options.authorizer;
    const supported = options.packaged && (
      (options.platform === "win32" && (options.architecture === "x64" || options.architecture === "arm64")
        && updater && options.authorizer)
      || (options.platform === "linux" && options.linuxUpdater && options.restart && options.authorizer)
    );
    this.state = {
      phase: supported ? "idle" : "unsupported",
      currentVersion: options.version,
      targetVersion: "",
      channel: options.channel ?? "stable",
      checkedAtUnixMs: 0,
      reasonCode: options.packaged ? "platform_unsupported" : "development_install",
    };
    if (!supported) return;
    this.state.reasonCode = "";
    if (updater && options.platform === "win32") this.bindEvents(updater);
  }

  getState(): DesktopUpdateState {
    return structuredClone(this.state);
  }

  setChannel(channel: "stable" | "beta"): DesktopUpdateState {
    if (channel === this.state.channel) return this.getState();
    this.channelGeneration += 1;
    this.authorizedVersion = "";
    this.state = {
      ...this.state,
      phase: this.state.phase === "unsupported" ? "unsupported" : "idle",
      channel,
      targetVersion: "",
      checkedAtUnixMs: 0,
      reasonCode: this.state.phase === "unsupported" ? this.state.reasonCode : "",
    };
    return this.getState();
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
    if ((!this.updater && !this.linuxUpdater) || !this.authorizer || this.state.phase === "unsupported") {
      return this.getState();
    }
    if (this.state.phase === "checking" || this.state.phase === "available") return this.getState();
    this.state = { ...this.state, phase: "checking", reasonCode: "" };
    const generation = this.channelGeneration;
    void this.authorizer.authorize(this.state.channel).then((authorized) => {
      if (generation !== this.channelGeneration) return;
      if (!authorized.available) {
        this.state = {
          ...this.state,
          phase: "not_available",
          checkedAtUnixMs: Date.now(),
          targetVersion: "",
          reasonCode: "",
        };
        return;
      }
      this.authorizedVersion = authorized.version;
      this.state = { ...this.state, phase: "available", targetVersion: authorized.version, reasonCode: "" };
      if (this.linuxUpdater) {
        void this.linuxUpdater.download(authorized.artifact).then((changed) => {
          if (generation !== this.channelGeneration) return;
          this.state = {
            ...this.state,
            phase: changed ? "downloaded" : "not_available",
            checkedAtUnixMs: Date.now(),
            targetVersion: changed ? authorized.version : "",
            reasonCode: "",
          };
        }, () => {
          if (generation === this.channelGeneration) this.fail();
        });
        return;
      }
      try {
        this.updater?.setFeedURL({ url: authorized.artifact.url, allowAnyVersion: false });
        this.updater?.checkForUpdates();
      } catch {
        this.fail();
      }
    }, () => {
      if (generation === this.channelGeneration) this.fail();
    });
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
      const version = eventVersion(arguments_[0]);
      if (!version || version !== this.authorizedVersion) return this.fail();
      this.state = { ...this.state, phase: "available", targetVersion: version, reasonCode: "" };
    });
    updater.on("update-not-available", () => {
      this.state = { ...this.state, phase: "not_available", checkedAtUnixMs: Date.now(), targetVersion: "", reasonCode: "" };
    });
    updater.on("update-downloaded", (...arguments_) => {
      const version = eventVersion(arguments_[1]) || this.state.targetVersion;
      if (!version || version !== this.authorizedVersion) return this.fail();
      this.state = { ...this.state, phase: "downloaded", checkedAtUnixMs: Date.now(), targetVersion: version, reasonCode: "" };
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
