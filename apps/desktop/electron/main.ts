import { app, autoUpdater, BrowserWindow, dialog, ipcMain, Menu, nativeImage, nativeTheme, protocol, session, shell, Tray, type WebContents } from "electron";
import { existsSync } from "node:fs";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import type {
  BridgeRequest,
  BridgeResponse,
  DaemonConversationTurnEventBatch,
  DesktopNavigationRoute,
} from "../src/contracts/bridge.js";
import { parseBridgeRequest } from "./bridgeValidation.js";
import { parseArtifactImportDialogResult } from "./artifactImportSelection.js";
import { artifactRemovalFailure } from "./artifactRemovalFailure.js";
import { credentialEnrollmentFailure } from "./credentialEnrollmentFailure.js";
import {
  DesktopConversationEventDeliveryError,
  DesktopConversationEventDeliveryTracker,
} from "./conversationEventDelivery.js";
import { DaemonSupervisor } from "./daemon/DaemonSupervisor.js";
import { PROTOCOL_VERSION } from "./daemon/DaemonRpcClient.js";
import { DesktopDeepLinkDelivery } from "./deepLinkDelivery.js";
import { ExternalUrlLaunchLimiter } from "./externalUrlLaunchLimiter.js";
import {
  hasDesktopDeepLinkArgument,
  parseDesktopDeepLink,
  parseDesktopDeepLinkFromArgv,
} from "./deepLinkPolicy.js";
import { denyRendererPermission, isAllowedAppNavigation } from "./navigationPolicy.js";
import { focusPrimaryWindow } from "./instancePolicy.js";
import { credentialEnrollmentParentWindowToken } from "./nativeWindowHandle.js";
import { resolveProtocolAsset } from "./protocolAssets.js";
import { resolveDevelopmentServerUrl } from "./developmentServer.js";
import { resolveTrayIconPath } from "./trayIcon.js";
import { isTrustedTopLevelAppSender } from "./trustedSenderPolicy.js";
import { shouldDeferAppQuit, shouldHideWindowOnClose } from "./windowClosePolicy.js";
import { withStartupDeadline } from "./startupDeadline.js";
import { UpdateCoordinator } from "./updateCoordinator.js";
import { resolveLinuxUpdateRunner } from "./linuxAppImageUpdater.js";
import { loadUpdateTrust, SignedUpdateManifestAuthorizer } from "./updateManifestVerifier.js";
import {
  applyGraphicsPolicy,
  DEVELOPMENT_GRAPHICS_FALLBACK_EXIT_CODE,
  graphicsRelaunchArguments,
  resolveGraphicsPolicy,
} from "./graphicsPolicy.js";

const directory = path.dirname(fileURLToPath(import.meta.url));
let supervisor: DaemonSupervisor | undefined;
let tray: Tray | undefined;
let primaryWindow: BrowserWindow | undefined;
let shutdownStarted = false;
let shutdownCompleted = false;
let keepRunningInNotificationArea = true;
let applicationDocumentForWindow: string | undefined;
let primaryWindowUsable = false;
let graphicsFallbackStarting = false;
let updateCoordinator: UpdateCoordinator | undefined;
const productionDocument = "grok-desktop://app/index.html";
const deepLinkDelivery = new DesktopDeepLinkDelivery();
const conversationWatches = new Map<number, Map<string, ConversationWatch>>();
const conversationEventDelivery = new DesktopConversationEventDeliveryTracker();
const externalUrlLaunchLimiter = new ExternalUrlLaunchLimiter();
const conversationReplayTokens = new Map<number, object>();
const conversationReplayTails = new Map<number, Promise<void>>();
const pendingConversationReplays = new Map<number, Set<string>>();
const pendingActiveConversationWatches = new Map<number, Set<string>>();

type ConversationWatch = {
  closed: boolean;
  close?: () => void;
  completion: Promise<void>;
  resolveCompletion(): void;
  rejectCompletion(error: Error): void;
};
class ConversationWatchSetupError extends Error {
  constructor(cause: unknown) {
    super("conversation event watch could not be established", { cause });
    this.name = "ConversationWatchSetupError";
  }
}
const noopConversationWatchResolve = () => undefined;
const noopConversationWatchReject = (_error: Error) => undefined;
const TERMINAL_CONVERSATION_STATES = new Set([
  "completed",
  "failed",
  "cancelled",
  "interrupted_needs_review",
]);
const MAX_RENDERER_DELIVERY_RETRIES = 2;
const MAX_CONVERSATION_SETUP_RETRY_MS = 2_000;
const STARTUP_PREFERENCES_DEADLINE_MS = 2_500;

protocol.registerSchemesAsPrivileged([{
  scheme: "grok-desktop",
  privileges: { standard: true, secure: true, corsEnabled: false, supportFetchAPI: false, stream: true },
}]);

const graphicsPolicy = resolveGraphicsPolicy({
  platform: process.platform,
  argv: process.argv.slice(1),
  waylandDisplay: process.env.WAYLAND_DISPLAY,
  x11Display: process.env.DISPLAY,
  glxVendor: process.env["__GLX_VENDOR_LIBRARY_NAME"],
  gbmBackend: process.env.GBM_BACKEND,
  nvidiaDriverPresent: process.platform === "linux" && existsSync("/proc/driver/nvidia/version"),
  nixGraphicsEnvironment: Boolean(process.env.IN_NIX_SHELL || process.env.NIX_LD),
});
applyGraphicsPolicy(graphicsPolicy, app);
if (graphicsPolicy.warning) {
  console.warn(`graphics policy ignored a ${graphicsPolicy.warning} argument set`);
}
console.info(`graphics backend=${graphicsPolicy.backend} reason=${graphicsPolicy.reason}`);

async function restartWithSoftwareGraphics(): Promise<void> {
  if (graphicsFallbackStarting) return;
  graphicsFallbackStarting = true;
  console.warn("GPU startup failed; restarting once with software rendering");
  if (app.isPackaged) {
    app.relaunch({ args: graphicsRelaunchArguments(process.argv.slice(1)) });
  }
  const activeSupervisor = supervisor;
  if (activeSupervisor) {
    try {
      await activeSupervisor.stop();
    } catch {
      // A graphics fallback must not strand the current process indefinitely.
    }
  }
  app.exit(app.isPackaged ? 0 : DEVELOPMENT_GRAPHICS_FALLBACK_EXIT_CODE);
}

async function initialDesktopPreferences(
  daemon: DaemonSupervisor,
): Promise<Awaited<ReturnType<DaemonSupervisor["getDesktopPreferences"]>>> {
  return withStartupDeadline(daemon.getDesktopPreferences(), STARTUP_PREFERENCES_DEADLINE_MS);
}

app.on("child-process-gone", (_event, details) => {
  if (
    details.type === "GPU"
    && !primaryWindowUsable
    && !graphicsPolicy.fallbackAttempted
    && graphicsPolicy.backend !== "software"
  ) {
    void restartWithSoftwareGraphics();
  }
});

function createWindow(applicationDocument: string): BrowserWindow {
  const window = new BrowserWindow({
    width: 1440,
    height: 920,
    minWidth: 860,
    minHeight: 640,
    // Software compositors need a mapped native surface before their first
    // frame; the neutral background prevents an unstyled flash.
    show: graphicsPolicy.backend === "software",
    titleBarStyle: process.platform === "darwin" ? "hiddenInset" : "default",
    backgroundColor: "#f5f5f2",
    webPreferences: {
      preload: path.join(directory, "preload.cjs"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      webSecurity: true,
      devTools: !app.isPackaged,
    },
  });
  const webContentsId = window.webContents.id;

  window.webContents.setWindowOpenHandler(() => ({ action: "deny" }));
  if (!app.isPackaged) {
    // With no application menu there are no menu accelerators; keep the two
    // development shortcuts the default menu used to provide.
    window.webContents.on("before-input-event", (event, input) => {
      if (input.type !== "keyDown") return;
      const key = input.key.toLowerCase();
      if (input.key === "F12" || (input.control && input.shift && key === "i")) {
        event.preventDefault();
        window.webContents.toggleDevTools();
      } else if (input.key === "F5" || (input.control && !input.shift && key === "r")) {
        event.preventDefault();
        window.webContents.reload();
      }
    });
  }
  window.webContents.on("did-start-navigation", (_event, _url, isInPlace, isMainFrame) => {
    if (isMainFrame && !isInPlace) {
      deepLinkDelivery.markRendererUnavailable(window.webContents.id);
      clearConversationWatches(window.webContents.id);
    }
  });
  window.webContents.on("will-navigate", (event, url) => {
    if (!isAllowedAppNavigation(url, applicationDocument)) event.preventDefault();
  });
  window.on("close", (event) => {
    if (!shouldHideWindowOnClose(keepRunningInNotificationArea, shutdownStarted)) return;
    event.preventDefault();
    window.hide();
  });
  window.on("closed", () => {
    deepLinkDelivery.markRendererUnavailable(webContentsId);
    clearConversationWatches(webContentsId);
    if (primaryWindow === window) primaryWindow = undefined;
  });
  window.once("ready-to-show", () => {
    primaryWindowUsable = true;
    window.show();
  });

  void window.loadURL(applicationDocument).catch(() => {
    console.warn("desktop renderer document failed to load");
  });
  primaryWindow = window;
  return window;
}

function showPrimaryWindow(applicationDocument: string): void {
  const window = primaryWindow && !primaryWindow.isDestroyed()
    ? primaryWindow
    : createWindow(applicationDocument);
  focusPrimaryWindow([window]);
}

function revealPrimaryWindowForNavigation(): void {
  if (applicationDocumentForWindow) {
    showPrimaryWindow(applicationDocumentForWindow);
    return;
  }
  if (primaryWindow && !primaryWindow.isDestroyed()) focusPrimaryWindow([primaryWindow]);
}

function deliverPendingNavigationRoute(): boolean {
  const window = primaryWindow;
  if (!window || window.isDestroyed() || window.webContents.isDestroyed()) return false;
  try {
    return deepLinkDelivery.deliver(window.webContents.id, (delivery) => {
      window.webContents.send("desktop:navigation-route", delivery);
    });
  } catch {
    deepLinkDelivery.markRendererUnavailable(window.webContents.id);
    return false;
  }
}

function activateNavigationRoute(route: DesktopNavigationRoute): void {
  // Reveal the existing notification-area window before any renderer route is delivered.
  revealPrimaryWindowForNavigation();
  deepLinkDelivery.queue(route);
  deliverPendingNavigationRoute();
}

function deliverConversationEvents(
  sender: WebContents,
  turnId: string,
  batch: DaemonConversationTurnEventBatch,
): Promise<void> {
  if (sender.isDestroyed()) {
    return Promise.reject(new DesktopConversationEventDeliveryError(
      "conversation event renderer is unavailable",
    ));
  }
  return conversationEventDelivery.deliver(sender.id, turnId, batch, (delivery) => {
    sender.send("desktop:conversation-turn-events", delivery);
  });
}

function closeConversationWatch(senderId: number, turnId: string, error?: Error): void {
  const watches = conversationWatches.get(senderId);
  const watch = watches?.get(turnId);
  if (!watches || !watch) return;
  watches.delete(turnId);
  if (watches.size === 0) conversationWatches.delete(senderId);
  watch.closed = true;
  watch.close?.();
  if (error) watch.rejectCompletion(error);
  else watch.resolveCompletion();
}

function clearConversationWatches(senderId: number): void {
  const watches = conversationWatches.get(senderId);
  // Deleting the identity token invalidates every pending loop while allowing
  // a destroyed WebContents id to be collected instead of accumulating a
  // permanent epoch entry across window recreation.
  conversationReplayTokens.delete(senderId);
  conversationReplayTails.delete(senderId);
  pendingConversationReplays.delete(senderId);
  pendingActiveConversationWatches.delete(senderId);
  for (const turnId of watches?.keys() ?? []) closeConversationWatch(senderId, turnId);
  conversationEventDelivery.markRendererUnavailable(senderId);
}

function conversationReplayToken(senderId: number): object {
  const existing = conversationReplayTokens.get(senderId);
  if (existing) return existing;
  const token = {};
  conversationReplayTokens.set(senderId, token);
  return token;
}

function newConversationWatch(): ConversationWatch {
  let resolveCompletion: () => void = noopConversationWatchResolve;
  let rejectCompletion: (error: Error) => void = noopConversationWatchReject;
  const completion = new Promise<void>((resolve, reject) => {
    resolveCompletion = resolve;
    rejectCompletion = reject;
  });
  return { closed: false, completion, resolveCompletion, rejectCompletion };
}

async function ensureConversationWatch(
  daemon: DaemonSupervisor,
  sender: WebContents,
  turnId: string,
): Promise<void> {
  if (sender.isDestroyed()) throw new Error("conversation event renderer is unavailable");
  let watches = conversationWatches.get(sender.id);
  if (!watches) {
    watches = new Map();
    conversationWatches.set(sender.id, watches);
  }
  const existing = watches.get(turnId);
  if (existing) return existing.completion;

  const watch = newConversationWatch();
  watches.set(turnId, watch);
  try {
    const close = await daemon.subscribeConversationTurnEvents(
      turnId,
      async (batch) => {
        if (watch.closed || sender.isDestroyed()) {
          throw new Error("conversation event renderer is unavailable");
        }
        await deliverConversationEvents(sender, turnId, batch);
        const terminal = batch.events.some(
          (event) => event.kind === "state_changed" && TERMINAL_CONVERSATION_STATES.has(event.toState),
        );
        if (terminal) closeConversationWatch(sender.id, turnId);
      },
      (error) => closeConversationWatch(sender.id, turnId, error),
    );
    if (watch.closed || sender.isDestroyed()) close();
    else watch.close = close;
  } catch (error) {
    closeConversationWatch(sender.id, turnId, new ConversationWatchSetupError(error));
  }
  return watch.completion;
}

function watchConversationWithoutBlocking(
  daemon: DaemonSupervisor,
  sender: WebContents,
  turnId: string,
): void {
  if (sender.isDestroyed()) return;
  const senderId = sender.id;
  const replayToken = conversationReplayToken(senderId);
  const pending = pendingActiveConversationWatches.get(senderId) ?? new Set<string>();
  if (pending.has(turnId)) return;
  pending.add(turnId);
  pendingActiveConversationWatches.set(senderId, pending);
  void (async () => {
    let setupRetryMs = 250;
    let deliveryRetries = 0;
    try {
      while (!sender.isDestroyed() && conversationReplayTokens.get(senderId) === replayToken) {
        try {
          await ensureConversationWatch(daemon, sender, turnId);
          return;
        } catch (error) {
          if (error instanceof ConversationWatchSetupError) {
            await new Promise((resolve) => setTimeout(resolve, setupRetryMs));
            setupRetryMs = Math.min(setupRetryMs * 2, MAX_CONVERSATION_SETUP_RETRY_MS);
            continue;
          }
          if (
            !(error instanceof DesktopConversationEventDeliveryError)
            || deliveryRetries >= MAX_RENDERER_DELIVERY_RETRIES
          ) {
            return;
          }
          const retryMs = 500 * (2 ** deliveryRetries);
          deliveryRetries += 1;
          await new Promise((resolve) => setTimeout(resolve, retryMs));
        }
      }
    } finally {
      pending.delete(turnId);
      if (pending.size === 0 && pendingActiveConversationWatches.get(senderId) === pending) {
        pendingActiveConversationWatches.delete(senderId);
      }
    }
  })();
}

function enqueueTerminalConversationReplays(
  daemon: DaemonSupervisor,
  sender: WebContents,
  turnIds: string[],
): void {
  if (sender.isDestroyed() || turnIds.length === 0) return;
  const senderId = sender.id;
  const replayToken = conversationReplayToken(senderId);
  const pending = pendingConversationReplays.get(senderId) ?? new Set<string>();
  pendingConversationReplays.set(senderId, pending);
  const queued = turnIds.filter((turnId) => {
    if (pending.has(turnId)) return false;
    pending.add(turnId);
    return true;
  });
  if (queued.length === 0) return;

  const prior = conversationReplayTails.get(senderId) ?? Promise.resolve();
  let tracked: Promise<void>;
  const run = prior.catch(() => undefined).then(async () => {
    for (const turnId of queued) {
      if (sender.isDestroyed() || conversationReplayTokens.get(senderId) !== replayToken) return;
      let setupRetryMs = 500;
      let deliveryRetries = 0;
      try {
        while (!sender.isDestroyed() && conversationReplayTokens.get(senderId) === replayToken) {
          try {
            await ensureConversationWatch(daemon, sender, turnId);
            break;
          } catch (error) {
            if (error instanceof ConversationWatchSetupError) {
              await new Promise((resolve) => setTimeout(resolve, setupRetryMs));
              setupRetryMs = Math.min(setupRetryMs * 2, MAX_CONVERSATION_SETUP_RETRY_MS);
              continue;
            }
            if (
              !(error instanceof DesktopConversationEventDeliveryError)
              || deliveryRetries >= MAX_RENDERER_DELIVERY_RETRIES
            ) {
              break;
            }
            const retryMs = 500 * (2 ** deliveryRetries);
            deliveryRetries += 1;
            await new Promise((resolve) => setTimeout(resolve, retryMs));
          }
        }
      } finally {
        pending.delete(turnId);
      }
    }
  });
  tracked = run.finally(() => {
    if (conversationReplayTails.get(senderId) === tracked) {
      conversationReplayTails.delete(senderId);
      if (pending.size === 0) pendingConversationReplays.delete(senderId);
    }
  });
  conversationReplayTails.set(senderId, tracked);
  void tracked.catch(() => undefined);
}

function trayImage() {
  const theme = nativeTheme.shouldUseDarkColors ? "dark" : "light";
  const iconPath = resolveTrayIconPath(
    app.getAppPath(),
    process.resourcesPath,
    process.platform,
    theme,
    existsSync,
  );
  const image = nativeImage.createFromPath(iconPath);
  if (image.isEmpty()) throw new Error(`canonical tray icon could not be decoded (${path.basename(iconPath)})`);
  return image;
}

function createTray(applicationDocument: string): void {
  tray = new Tray(trayImage());
  tray.setToolTip("Grok Desktop");
  tray.setContextMenu(Menu.buildFromTemplate([
    { label: "Open Grok Desktop", click: () => showPrimaryWindow(applicationDocument) },
    { type: "separator" },
    { label: "Quit", click: () => app.quit() },
  ]));
  tray.on("click", () => showPrimaryWindow(applicationDocument));
  tray.on("double-click", () => showPrimaryWindow(applicationDocument));
  nativeTheme.on("updated", () => {
    if (tray && !tray.isDestroyed()) tray.setImage(trayImage());
  });
}

function registerBridge(daemon: DaemonSupervisor, applicationDocument: string): void {
  let artifactImportPickerActive = false;
  ipcMain.on("desktop:conversation-events-ready", (event) => {
    if (!isTrustedNavigationSender(event, applicationDocument)) return;
    clearConversationWatches(event.sender.id);
  });
  ipcMain.on("desktop:conversation-events-ack", (event, deliveryId: unknown) => {
    if (
      !isTrustedNavigationSender(event, applicationDocument)
      || !Number.isSafeInteger(deliveryId)
      || (deliveryId as number) < 1
    ) {
      return;
    }
    conversationEventDelivery.acknowledge(event.sender.id, deliveryId as number);
  });
  ipcMain.on("desktop:navigation-ready", (event) => {
    if (!isTrustedNavigationSender(event, applicationDocument)) return;
    deepLinkDelivery.markRendererReady(event.sender.id);
    deliverPendingNavigationRoute();
  });
  ipcMain.on("desktop:navigation-ack", (event, deliveryId: unknown) => {
    if (
      !isTrustedNavigationSender(event, applicationDocument)
      || !Number.isSafeInteger(deliveryId)
      || (deliveryId as number) < 1
    ) {
      return;
    }
    deepLinkDelivery.acknowledge(event.sender.id, deliveryId as number);
  });

  ipcMain.handle("desktop:request", async (event, rawRequest: unknown): Promise<BridgeResponse> => {
    if (!isTrustedNavigationSender(event, applicationDocument)) {
      throw new Error("untrusted renderer frame");
    }
    let request: BridgeRequest;
    try {
      request = parseBridgeRequest(rawRequest);
    } catch (error) {
      if (isExternalUrlIntent(rawRequest)) {
        return { kind: "desktop.externalUrlOpenFailed", reason: "rejected" };
      }
      throw error;
    }
    if (request.kind === "runtime.info") {
      return { kind: "runtime.info", platform: process.platform, version: app.getVersion() };
    }
    if (request.kind === "desktop.openExternalUrl") {
      const release = externalUrlLaunchLimiter.tryAcquire();
      if (!release) return { kind: "desktop.externalUrlOpenFailed", reason: "busy" };
      try {
        await shell.openExternal(request.url, { activate: true });
        return { kind: "desktop.externalUrlOpened", accepted: true };
      } catch {
        return { kind: "desktop.externalUrlOpenFailed", reason: "unavailable" };
      } finally {
        release();
      }
    }
    if (request.kind === "desktop.getUpdateState") {
      if (!updateCoordinator) throw new Error("update coordinator is unavailable");
      return { kind: "desktop.updateState", state: updateCoordinator.getState() };
    }
    if (request.kind === "desktop.checkForUpdates") {
      if (!updateCoordinator) throw new Error("update coordinator is unavailable");
      return { kind: "desktop.updateState", state: updateCoordinator.check() };
    }
    if (request.kind === "desktop.installUpdate") {
      return { kind: "desktop.updateInstallAccepted", accepted: updateCoordinator?.install() ?? false };
    }

    const owner = BrowserWindow.fromWebContents(event.sender);
    if (request.kind === "window.minimize") {
      owner?.minimize();
      return { kind: "window.action", accepted: true };
    }
    if (request.kind === "window.maximize") {
      if (owner?.isMaximized()) owner.unmaximize();
      else owner?.maximize();
      return { kind: "window.action", accepted: true };
    }
    if (request.kind === "window.close") {
      owner?.close();
      return { kind: "window.action", accepted: true };
    }
    if (request.kind === "daemon.bootstrap") {
      const result = await daemon.bootstrap();
      return { kind: "daemon.bootstrap", ...result };
    }
    if (request.kind === "daemon.getAccountState") {
      const accountState = await daemon.getAccountState();
      return { kind: "daemon.accountState", accountState };
    }
    if (request.kind === "daemon.startGrokBuildAuth") {
      const status = await daemon.startGrokBuildAuth(request.idempotencyKey);
      return { kind: "daemon.grokBuildAuthStatus", ...status };
    }
    if (request.kind === "daemon.getGrokBuildAuthStatus") {
      const status = await daemon.getGrokBuildAuthStatus();
      return { kind: "daemon.grokBuildAuthStatus", ...status };
    }
    if (request.kind === "daemon.getManagedIntegration") {
      const integration = await daemon.getManagedIntegration(request.integrationId);
      return { kind: "daemon.managedIntegration", integration };
    }
    if (request.kind === "daemon.changeManagedIntegration") {
      const integration = await daemon.changeManagedIntegration(
        request.integrationId,
        request.action,
        request.expectedRevision,
        request.idempotencyKey,
      );
      return { kind: "daemon.managedIntegration", integration };
    }
    if (request.kind === "daemon.getDesktopPreferences") {
      const preferences = await daemon.getDesktopPreferences();
      keepRunningInNotificationArea = preferences.keepRunningInNotificationArea;
      return { kind: "daemon.desktopPreferences", preferences };
    }
    if (request.kind === "daemon.updateDesktopPreferences") {
      const preferences = await daemon.updateDesktopPreferences(
        request.expectedRevision,
        request.keepRunningInNotificationArea,
        request.idempotencyKey,
      );
      keepRunningInNotificationArea = preferences.keepRunningInNotificationArea;
      return { kind: "daemon.desktopPreferences", preferences };
    }
    if (request.kind === "daemon.getChatModelCatalog") {
      const catalog = await daemon.getChatModelCatalog();
      return { kind: "daemon.chatModelCatalog", catalog };
    }
    if (request.kind === "daemon.getUsageSummary") {
      const summary = await daemon.getUsageSummary(
        request.scopeKind,
        request.scopeId,
        request.window,
      );
      return { kind: "daemon.usageSummary", summary };
    }
    if (request.kind === "daemon.beginSuperGrokDeviceEnrollment") {
      const status = await daemon.beginSuperGrokDeviceEnrollment(request.idempotencyKey);
      return { kind: "daemon.superGrokEnrollmentStatus", status };
    }
    if (request.kind === "daemon.getSuperGrokEnrollmentStatus") {
      const status = await daemon.getSuperGrokEnrollmentStatus();
      return { kind: "daemon.superGrokEnrollmentStatus", status };
    }
    if (request.kind === "daemon.cancelSuperGrokEnrollment") {
      const status = await daemon.cancelSuperGrokEnrollment(request.idempotencyKey);
      return { kind: "daemon.superGrokEnrollmentStatus", status };
    }
    if (request.kind === "daemon.disconnectSuperGrok") {
      const status = await daemon.disconnectSuperGrok(request.idempotencyKey);
      return { kind: "daemon.superGrokEnrollmentStatus", status };
    }
    if (request.kind === "daemon.selectChatModel") {
      const preference = await daemon.selectChatModel(
        request.expectedRevision,
        request.modelId,
        request.idempotencyKey,
      );
      return { kind: "daemon.chatModelPreference", preference };
    }
    if (request.kind === "daemon.enrollXaiApiKey") {
      if (!owner || owner.isDestroyed()) throw new Error("credential enrollment requires an owning window");
      const parentWindowToken = credentialEnrollmentParentWindowToken(
        () => owner.getNativeWindowHandle(),
      );
      try {
        const accountState = await daemon.enrollXaiApiKey(parentWindowToken, request.idempotencyKey);
        return { kind: "daemon.accountState", accountState };
      } catch (error) {
        const failure = credentialEnrollmentFailure(error);
        if (failure) return failure;
        throw error;
      }
    }
    if (request.kind === "daemon.deleteXaiApiKey") {
      const accountState = await daemon.deleteXaiApiKey(request.idempotencyKey);
      return { kind: "daemon.accountState", accountState };
    }
    if (request.kind === "daemon.createProject") {
      const project = await daemon.createProject(request.name, request.description, request.idempotencyKey);
      return { kind: "daemon.project", project };
    }
    if (request.kind === "daemon.createThread") {
      const thread = await daemon.createThread(request.projectId, request.title, request.idempotencyKey);
      return { kind: "daemon.thread", thread };
    }
    if (request.kind === "daemon.importArtifact") {
      if (!owner || owner.isDestroyed()) {
        throw new Error("artifact import requires an owning window");
      }
      if (artifactImportPickerActive) {
        throw new Error("an artifact import picker is already active");
      }
      artifactImportPickerActive = true;
      try {
        const selection = parseArtifactImportDialogResult(await dialog.showOpenDialog(owner, {
          title: "Import a file to Grok Desktop",
          buttonLabel: "Import",
          properties: ["openFile"],
        }));
        if (selection.kind === "cancelled") {
          return { kind: "daemon.artifactImportCancelled" };
        }
        const artifact = await daemon.importArtifact(
          request.projectId,
          selection.displayName,
          selection.mediaType,
          selection.sourcePath,
          request.idempotencyKey,
        );
        return { kind: "daemon.artifactImported", artifact };
      } finally {
        artifactImportPickerActive = false;
      }
    }
    if (request.kind === "daemon.openArtifact") {
      const receipt = await daemon.openArtifact(
        request.artifactId,
        request.contentVersion,
        request.idempotencyKey,
      );
      return { kind: "daemon.artifactOpened", receipt };
    }
    if (request.kind === "daemon.removeArtifact") {
      try {
        const outcome = await daemon.removeArtifact(
          request.artifactId,
          request.expectedRevision,
          request.expectedContentVersion,
          request.idempotencyKey,
        );
        return outcome.status === "pending"
          ? {
              kind: "daemon.artifactRemovalPending",
              artifactId: outcome.artifactId,
              expectedRevision: outcome.expectedRevision,
              expectedContentVersion: outcome.expectedContentVersion,
              tombstone: outcome.tombstone,
            }
          : { kind: "daemon.artifactRemoved", artifact: outcome.artifact };
      } catch (error) {
        const failure = artifactRemovalFailure(error);
        if (failure) return failure;
        throw error;
      }
    }
    if (request.kind === "daemon.getConversation") {
      const requestReplayToken = conversationReplayToken(event.sender.id);
      const conversation = await daemon.getConversation(request.threadId);
      // Active work and terminal outcomes that may own a durable partial prefix
      // need replay. Completed text is already canonical on the turn snapshot;
      // pre-dispatch cancellation can never own provider text. Newest-first
      // preserves the most relevant recovery evidence under the global channel
      // bound when a long conversation contains many historical failures.
      if (conversationReplayTokens.get(event.sender.id) === requestReplayToken) {
        const terminalReplays: string[] = [];
        for (let index = conversation.turns.length - 1; index >= 0; index -= 1) {
          const turn = conversation.turns[index];
          if (
            turn.state === "reserved"
            || turn.state === "provider_started"
          ) {
            watchConversationWithoutBlocking(daemon, event.sender, turn.turnId);
          } else if (turn.state === "failed" || turn.state === "interrupted_needs_review") {
            terminalReplays.push(turn.turnId);
          }
        }
        enqueueTerminalConversationReplays(daemon, event.sender, terminalReplays);
      }
      return { kind: "daemon.conversation", ...conversation };
    }
    if (request.kind === "daemon.searchWorkspace") {
      const results = await daemon.searchWorkspace(
        request.projectId,
        request.query,
        request.offset,
        request.limit,
      );
      return { kind: "daemon.workspaceSearchResults", results };
    }
    if (request.kind === "daemon.startConversationTurn") {
      const requestReplayToken = conversationReplayToken(event.sender.id);
      const turn = await daemon.startConversationTurn(
        request.threadId,
        request.content,
        request.idempotencyKey,
        request.modelId,
        request.searchEnabled,
      );
      if (conversationReplayTokens.get(event.sender.id) === requestReplayToken) {
        if (turn.state === "reserved" || turn.state === "provider_started") {
          watchConversationWithoutBlocking(daemon, event.sender, turn.turnId);
        } else if (turn.state === "failed" || turn.state === "interrupted_needs_review") {
          enqueueTerminalConversationReplays(daemon, event.sender, [turn.turnId]);
        }
      }
      return { kind: "daemon.conversationTurn", turn };
    }
    if (request.kind === "daemon.retryConversationTurn") {
      const requestReplayToken = conversationReplayToken(event.sender.id);
      const turn = await daemon.retryConversationTurn(
        request.sourceTurnId,
        request.expectedRevision,
        request.idempotencyKey,
      );
      if (conversationReplayTokens.get(event.sender.id) === requestReplayToken) {
        if (turn.state === "reserved" || turn.state === "provider_started") {
          watchConversationWithoutBlocking(daemon, event.sender, turn.turnId);
        } else if (turn.state === "failed" || turn.state === "interrupted_needs_review") {
          enqueueTerminalConversationReplays(daemon, event.sender, [turn.turnId]);
        }
      }
      return { kind: "daemon.conversationTurn", turn };
    }
    if (request.kind === "daemon.branchConversationThread") {
      const fork = await daemon.branchConversationThread(
        request.sourceTurnId,
        request.expectedRevision,
        request.idempotencyKey,
      );
      return { kind: "daemon.conversationFork", fork };
    }
    if (request.kind === "daemon.editAndBranchConversationTurn") {
      const requestReplayToken = conversationReplayToken(event.sender.id);
      const fork = await daemon.editAndBranchConversationTurn(
        request.sourceTurnId,
        request.expectedRevision,
        request.content,
        request.idempotencyKey,
      );
      const turn = fork.startedTurn;
      if (turn && conversationReplayTokens.get(event.sender.id) === requestReplayToken) {
        if (turn.state === "reserved" || turn.state === "provider_started") {
          watchConversationWithoutBlocking(daemon, event.sender, turn.turnId);
        } else if (turn.state === "failed" || turn.state === "interrupted_needs_review") {
          enqueueTerminalConversationReplays(daemon, event.sender, [turn.turnId]);
        }
      }
      return { kind: "daemon.conversationFork", fork };
    }
    if (request.kind === "daemon.regenerateConversationTurn") {
      const requestReplayToken = conversationReplayToken(event.sender.id);
      const fork = await daemon.regenerateConversationTurn(
        request.sourceTurnId,
        request.expectedRevision,
        request.idempotencyKey,
      );
      const turn = fork.startedTurn;
      if (turn && conversationReplayTokens.get(event.sender.id) === requestReplayToken) {
        if (turn.state === "reserved" || turn.state === "provider_started") {
          watchConversationWithoutBlocking(daemon, event.sender, turn.turnId);
        } else if (turn.state === "failed" || turn.state === "interrupted_needs_review") {
          enqueueTerminalConversationReplays(daemon, event.sender, [turn.turnId]);
        }
      }
      return { kind: "daemon.conversationFork", fork };
    }
    if (request.kind === "daemon.getConversationForkMetadata") {
      const metadata = await daemon.getConversationForkMetadata(request.threadId);
      return { kind: "daemon.conversationForkMetadata", metadata };
    }
    if (request.kind === "daemon.acknowledgeConversationForkDelivery") {
      const delivery = await daemon.acknowledgeConversationForkDelivery(
        request.childThreadId,
        request.expectedRevision,
        request.idempotencyKey,
      );
      return { kind: "daemon.conversationForkDelivery", delivery };
    }
    if (request.kind === "daemon.cancelConversationTurn") {
      const requestReplayToken = conversationReplayToken(event.sender.id);
      const turn = await daemon.cancelConversationTurn(
        request.turnId,
        request.expectedRevision,
        request.idempotencyKey,
      );
      if (
        conversationReplayTokens.get(event.sender.id) === requestReplayToken
        && (turn.state === "failed" || turn.state === "interrupted_needs_review")
      ) {
        enqueueTerminalConversationReplays(daemon, event.sender, [turn.turnId]);
      }
      return { kind: "daemon.conversationTurn", turn };
    }
    if (request.kind === "daemon.createAutomation") {
      const automation = await daemon.createAutomation(request, request.idempotencyKey);
      return { kind: "daemon.automation", automation };
    }
    if (request.kind === "daemon.updateAutomation") {
      const automation = await daemon.updateAutomation(
        request.automationId,
        request.expectedRevision,
        request,
        request.idempotencyKey,
      );
      return { kind: "daemon.automation", automation };
    }
    const approval = await daemon.decideApproval(
      request.approvalId,
      request.expectedRevision,
      request.approved,
      request.idempotencyKey,
    );
    return { kind: "daemon.approval", approval };
  });
}

function isTrustedNavigationSender(
  event: Electron.IpcMainEvent | Electron.IpcMainInvokeEvent,
  applicationDocument: string,
): boolean {
  const owner = BrowserWindow.fromWebContents(event.sender);
  return isTrustedTopLevelAppSender({
    ownsPrimaryWindow: owner === primaryWindow,
    hasSenderFrame: Boolean(event.senderFrame),
    isTopLevelFrame: event.senderFrame === event.sender.mainFrame,
    frameUrl: event.senderFrame?.url ?? "",
  }, applicationDocument);
}

function isExternalUrlIntent(value: unknown): boolean {
  return Boolean(
    value
    && typeof value === "object"
    && !Array.isArray(value)
    && "kind" in value
    && value.kind === "desktop.openExternalUrl",
  );
}

const primaryInstance = app.requestSingleInstanceLock();
if (!primaryInstance) app.quit();
if (primaryInstance) {
  const coldStartRoute = parseDesktopDeepLinkFromArgv(process.argv);
  if (coldStartRoute) deepLinkDelivery.queue(coldStartRoute);
}

app.on("open-url", (event, url) => {
  event.preventDefault();
  const route = parseDesktopDeepLink(url);
  if (route) activateNavigationRoute(route);
});

if (primaryInstance) app.whenReady().then(async () => {
  // The renderer owns all chrome; the default File/Edit/View menu bar is
  // dead weight on Windows/Linux. macOS keeps the default menu because the
  // system menu bar carries required app/window commands and edit shortcuts.
  if (process.platform !== "darwin") Menu.setApplicationMenu(null);
  session.defaultSession.setPermissionCheckHandler(() => denyRendererPermission());
  session.defaultSession.setPermissionRequestHandler((_webContents, _permission, callback) => callback(denyRendererPermission()));
  const development = !app.isPackaged;
  const developmentServer = resolveDevelopmentServerUrl(app.isPackaged, process.env.VITE_DEV_SERVER_URL);
  const distributionRoot = path.join(directory, "../../dist");
  const applicationDocument = developmentServer ?? productionDocument;
  let updateAuthorizer;
  if (app.isPackaged && (process.platform === "linux" || process.platform === "win32")
      && (process.arch === "x64" || process.arch === "arm64")) {
    try {
      const trustedKeys = await loadUpdateTrust(path.join(process.resourcesPath, "update-trusted-keys.json"));
      updateAuthorizer = new SignedUpdateManifestAuthorizer({
        platform: process.platform,
        architecture: process.arch,
        currentVersion: app.getVersion(),
        protocolVersion: PROTOCOL_VERSION,
        schemaVersion: 24,
        trustedKeys,
      });
    } catch {
      updateAuthorizer = undefined;
    }
  }
  updateCoordinator = new UpdateCoordinator(autoUpdater, {
    packaged: app.isPackaged,
    platform: process.platform,
    architecture: process.arch,
    version: app.getVersion(),
    linuxUpdater: resolveLinuxUpdateRunner({
      packaged: app.isPackaged,
      platform: process.platform,
      resourcesPath: process.resourcesPath,
      appImagePath: process.env.APPIMAGE,
    }),
    restart: () => {
      app.relaunch();
      app.quit();
    },
    authorizer: updateAuthorizer,
  });
  updateCoordinator.start();
  if (!developmentServer) {
    await protocol.handle("grok-desktop", async (request) => {
      if (request.method !== "GET") return new Response("Method not allowed", { status: 405, headers: { "X-Content-Type-Options": "nosniff" } });
      const asset = resolveProtocolAsset(distributionRoot, request.url);
      if (!asset) return new Response("Not found", { status: 404, headers: { "X-Content-Type-Options": "nosniff" } });
      return new Response(await readFile(asset.file), {
        status: 200,
        headers: { "Content-Type": asset.contentType, "Cache-Control": asset.contentType.startsWith("text/html") ? "no-store" : "public, max-age=31536000, immutable", "X-Content-Type-Options": "nosniff" },
      });
    });
  }
  supervisor = new DaemonSupervisor({
    appPath: app.getAppPath(),
    resourcesPath: process.resourcesPath,
    runtimeDirectory: app.getPath("temp"),
    allowDevelopmentBinary: development,
    inheritDaemonStderr: development,
  });
  supervisor.subscribe((status) => {
    for (const window of BrowserWindow.getAllWindows()) {
      if (!window.isDestroyed()) window.webContents.send("daemon:status", status);
    }
  });
  registerBridge(supervisor, applicationDocument);
  try {
    const preferences = await initialDesktopPreferences(supervisor);
    keepRunningInNotificationArea = preferences.keepRunningInNotificationArea;
  } catch {
    // The product default remains active while the daemon reports Limited Mode.
    console.warn("desktop preferences were unavailable during bounded startup; using the product default");
  }
  // Window creation is enabled only after IPC handlers and daemon-owned close
  // behavior are ready, so an activation cannot outrun the preload handshake.
  applicationDocumentForWindow = applicationDocument;
  createTray(applicationDocument);
  createWindow(applicationDocument);
  app.on("activate", () => {
    showPrimaryWindow(applicationDocument);
  });
});

app.on("second-instance", (_event, argv) => {
  const route = parseDesktopDeepLinkFromArgv(argv);
  if (route) {
    revealPrimaryWindowForNavigation();
    deepLinkDelivery.queue(route);
    deliverPendingNavigationRoute();
  } else if (!hasDesktopDeepLinkArgument(argv)) {
    revealPrimaryWindowForNavigation();
  }
});

app.on("before-quit", (event) => {
  updateCoordinator?.stop();
  const activeSupervisor = supervisor;
  if (!shouldDeferAppQuit(Boolean(activeSupervisor), shutdownCompleted) || !activeSupervisor) return;
  event.preventDefault();
  // A second Quit while stop() is in flight must remain deferred. Allowing it
  // through would tear down Electron before the daemon has released durable
  // state and its database lock.
  if (shutdownStarted) return;
  shutdownStarted = true;
  const completeShutdown = () => {
    shutdownCompleted = true;
    app.quit();
  };
  // Shutdown errors cannot leave the process trapped, and the rejection must
  // be consumed rather than becoming an unhandled promise rejection.
  void activeSupervisor.stop().then(completeShutdown, completeShutdown);
});

// SIGTERM/SIGINT (e.g. Ctrl+C stopping `pnpm dev`) bypass before-quit and would
// orphan the daemon, which keeps the database lock and breaks the next launch.
// Stop the daemon directly and exit — app.quit()'s window-close negotiation can
// stall under a signal, and a stalled shutdown is exactly what orphans daemons.
const exitAfterShutdown = () => app.exit(0);
for (const signal of ["SIGTERM", "SIGINT"] as const) {
  process.on(signal, () => {
    if (shutdownStarted) {
      app.exit(0);
      return;
    }
    shutdownStarted = true;
    if (supervisor) void supervisor.stop().then(exitAfterShutdown, exitAfterShutdown);
    else exitAfterShutdown();
  });
}

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
