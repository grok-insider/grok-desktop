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

export interface PlatformUpdater {
  download(expected?: AuthorizedUpdate["artifact"]): Promise<boolean>;
  install(): void | Promise<void>;
}

const CHECK_INTERVAL_MS = 6 * 60 * 60 * 1_000;
const INITIAL_CHECK_DELAY_MS = 30 * 1_000;

export class UpdateCoordinator {
  private state: DesktopUpdateState;
  private checkTimer: ReturnType<typeof setInterval> | undefined;
  private initialCheckTimer: ReturnType<typeof setTimeout> | undefined;
  private readonly platformUpdater: PlatformUpdater | undefined;
  private readonly authorizer: UpdateAuthorizer | undefined;
  private channelGeneration = 0;
  private downloadedGeneration: number | undefined;
  private downloadQueue: Promise<void> = Promise.resolve();
  private installStarted = false;
  private readonly previewChannelLocked: boolean;

  constructor(
    options: {
      packaged: boolean;
      platform: NodeJS.Platform;
      architecture: string;
      version: string;
      platformUpdater?: PlatformUpdater;
      authorizer?: UpdateAuthorizer;
      channel?: "stable" | "beta";
    },
  ) {
    this.platformUpdater = options.platformUpdater;
    this.authorizer = options.authorizer;
    this.previewChannelLocked = /^0\.0\.[0-9]+$/.test(options.version);
    const supported = options.packaged && (
      (options.platform === "win32" || options.platform === "linux")
      && (options.architecture === "x64" || options.architecture === "arm64")
      && options.platformUpdater && options.authorizer
    );
    this.state = {
      phase: supported ? "idle" : "unsupported",
      currentVersion: options.version,
      targetVersion: "",
      channel: this.previewChannelLocked ? "beta" : (options.channel ?? "stable"),
      checkedAtUnixMs: 0,
      reasonCode: options.packaged ? "platform_unsupported" : "development_install",
    };
    if (!supported) return;
    this.state.reasonCode = "";
  }

  getState(): DesktopUpdateState {
    return structuredClone(this.state);
  }

  setChannel(channel: "stable" | "beta"): DesktopUpdateState {
    if (this.previewChannelLocked) channel = "beta";
    if (channel === this.state.channel) return this.getState();
    this.channelGeneration += 1;
    this.downloadedGeneration = undefined;
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
    if (!this.platformUpdater || this.state.phase === "unsupported"
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
    if (!this.platformUpdater || !this.authorizer || this.state.phase === "unsupported") {
      return this.getState();
    }
    if (this.installStarted || this.state.phase === "checking" || this.state.phase === "available") {
      return this.getState();
    }
    this.downloadedGeneration = undefined;
    this.state = { ...this.state, phase: "checking", reasonCode: "" };
    const generation = this.channelGeneration;
    const platformUpdater = this.platformUpdater;
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
      this.state = { ...this.state, phase: "available", targetVersion: authorized.version, reasonCode: "" };
      this.enqueueDownload(generation, authorized, platformUpdater);
    }, () => {
      if (generation === this.channelGeneration) this.fail();
    });
    return this.getState();
  }

  install(): boolean {
    if (this.state.phase !== "downloaded") return false;
    if (!this.platformUpdater) return false;
    if (this.installStarted || this.downloadedGeneration !== this.channelGeneration) return false;
    this.installStarted = true;
    try {
      void Promise.resolve(this.platformUpdater.install()).catch(() => this.fail());
    } catch {
      this.fail();
    }
    return true;
  }

  private enqueueDownload(
    generation: number,
    authorized: AuthorizedUpdate,
    platformUpdater: PlatformUpdater,
  ): void {
    const download = async () => {
      if (generation !== this.channelGeneration || !authorized.available) return;
      try {
        const changed = await platformUpdater.download(authorized.artifact);
        if (generation !== this.channelGeneration) return;
        this.downloadedGeneration = changed ? generation : undefined;
        this.state = {
          ...this.state,
          phase: changed ? "downloaded" : "not_available",
          checkedAtUnixMs: Date.now(),
          targetVersion: changed ? authorized.version : "",
          reasonCode: "",
        };
      } catch {
        if (generation === this.channelGeneration) this.fail();
      }
    };
    this.downloadQueue = this.downloadQueue.then(download, download);
  }

  private fail(): void {
    this.downloadedGeneration = undefined;
    this.state = { ...this.state, phase: "failed", checkedAtUnixMs: Date.now(), reasonCode: "check_failed" };
  }
}
