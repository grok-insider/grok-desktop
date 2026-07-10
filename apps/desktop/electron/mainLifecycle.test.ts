// @vitest-environment node
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

type Listener = (...arguments_: unknown[]) => unknown;

const mocks = vi.hoisted(() => {
  const events = {
    app: new Map<string, Listener[]>(),
    theme: new Map<string, Listener[]>(),
    tray: new Map<string, Listener[]>(),
    window: new Map<string, Listener[]>(),
  };

  const register = (target: keyof typeof events, name: string, listener: Listener) => {
    const listeners = events[target].get(name) ?? [];
    listeners.push(listener);
    events[target].set(name, listeners);
  };

  const frame = { url: "grok-desktop://app/index.html" };
  const webContents = {
    id: 42,
    mainFrame: frame,
    isDestroyed: vi.fn(() => false),
    on: vi.fn(),
    send: vi.fn(),
    setWindowOpenHandler: vi.fn(),
  };
  const window = {
    webContents,
    close: vi.fn(),
    getNativeWindowHandle: vi.fn(() => Buffer.alloc(8)),
    hide: vi.fn(),
    isDestroyed: vi.fn(() => false),
    isMaximized: vi.fn(() => false),
    loadURL: vi.fn(() => Promise.resolve()),
    maximize: vi.fn(),
    minimize: vi.fn(),
    on: vi.fn((name: string, listener: Listener) => register("window", name, listener)),
    once: vi.fn((name: string, listener: Listener) => register("window", name, listener)),
    show: vi.fn(),
    unmaximize: vi.fn(),
  };
  const BrowserWindow = Object.assign(vi.fn(function BrowserWindowMock() {
    return window;
  }), {
    fromWebContents: vi.fn((sender: unknown) => sender === webContents ? window : undefined),
    getAllWindows: vi.fn(() => [window]),
  });
  const tray = {
    isDestroyed: vi.fn(() => false),
    on: vi.fn((name: string, listener: Listener) => register("tray", name, listener)),
    setContextMenu: vi.fn(),
    setImage: vi.fn(),
    setToolTip: vi.fn(),
  };
  const app = {
    exit: vi.fn(),
    getAppPath: vi.fn(() => "/app"),
    getPath: vi.fn(() => "/tmp"),
    getVersion: vi.fn(() => "0.1.0"),
    isPackaged: true,
    on: vi.fn((name: string, listener: Listener) => register("app", name, listener)),
    quit: vi.fn(),
    requestSingleInstanceLock: vi.fn(() => true),
    whenReady: vi.fn(() => Promise.resolve()),
  };
  const daemon = {
    getDesktopPreferences: vi.fn(() => Promise.resolve({
      keepRunningInNotificationArea: true,
      revision: 0,
      updatedAtUnixMs: 0,
    })),
    stop: vi.fn(() => Promise.resolve()),
    subscribe: vi.fn(() => vi.fn()),
  };
  const Menu = { buildFromTemplate: vi.fn((template: unknown) => ({ template })) };
  const nativeTheme = {
    on: vi.fn((name: string, listener: Listener) => register("theme", name, listener)),
    shouldUseDarkColors: false,
  };

  return {
    app,
    appEvents: events.app,
    BrowserWindow,
    daemon,
    frame,
    Menu,
    nativeTheme,
    tray,
    trayEvents: events.tray,
    window,
    windowEvents: events.window,
    webContents,
    electron: {
      app,
      BrowserWindow,
      dialog: { showOpenDialog: vi.fn() },
      ipcMain: { handle: vi.fn(), on: vi.fn() },
      Menu,
      nativeImage: { createFromPath: vi.fn(() => ({ isEmpty: () => false })) },
      nativeTheme,
      protocol: {
        handle: vi.fn(() => Promise.resolve()),
        registerSchemesAsPrivileged: vi.fn(),
      },
      session: {
        defaultSession: {
          setPermissionCheckHandler: vi.fn(),
          setPermissionRequestHandler: vi.fn(),
        },
      },
      shell: { openExternal: vi.fn(() => Promise.resolve()) },
      Tray: vi.fn(function TrayMock() {
        return tray;
      }),
    },
    DaemonSupervisor: vi.fn(function DaemonSupervisorMock() {
      return daemon;
    }),
    resolveTrayIconPath: vi.fn(() => "/tray.png"),
  };
});

vi.mock("electron", () => mocks.electron);
vi.mock("./daemon/DaemonSupervisor.js", () => ({
  DaemonSupervisor: mocks.DaemonSupervisor,
}));
vi.mock("./trayIcon.js", () => ({
  resolveTrayIconPath: mocks.resolveTrayIconPath,
}));

const originalSignalListeners = {
  SIGINT: new Set(process.listeners("SIGINT")),
  SIGTERM: new Set(process.listeners("SIGTERM")),
};
const resourcesPathDescriptor = Object.getOwnPropertyDescriptor(process, "resourcesPath");

beforeEach(() => {
  vi.resetModules();
  vi.clearAllMocks();
  mocks.appEvents.clear();
  mocks.trayEvents.clear();
  mocks.windowEvents.clear();
  mocks.daemon.getDesktopPreferences.mockResolvedValue({
    keepRunningInNotificationArea: true,
    revision: 0,
    updatedAtUnixMs: 0,
  });
  mocks.daemon.stop.mockResolvedValue();
  mocks.daemon.subscribe.mockReturnValue(vi.fn());
  Object.defineProperty(process, "resourcesPath", {
    configurable: true,
    value: "/resources",
  });
});

afterEach(() => {
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    for (const listener of process.listeners(signal)) {
      if (!originalSignalListeners[signal].has(listener)) process.removeListener(signal, listener);
    }
  }
  if (resourcesPathDescriptor) Object.defineProperty(process, "resourcesPath", resourcesPathDescriptor);
  else Reflect.deleteProperty(process, "resourcesPath");
});

describe("main process window and quit lifecycle", () => {
  it("hides a normal close and lets tray Quit complete one graceful daemon shutdown", async () => {
    const stop = deferred();
    mocks.daemon.stop.mockReturnValue(stop.promise);
    await bootMain();

    const close = registered(mocks.windowEvents, "close");
    const normalClose = { preventDefault: vi.fn() };
    close(normalClose);
    expect(normalClose.preventDefault).toHaveBeenCalledOnce();
    expect(mocks.window.hide).toHaveBeenCalledOnce();

    const template = mocks.Menu.buildFromTemplate.mock.calls[0]?.[0] as Array<{
      label?: string;
      click?: () => void;
    }>;
    const quit = template.find((item) => item.label === "Quit");
    expect(quit?.click).toBeTypeOf("function");
    quit?.click?.();
    expect(mocks.app.quit).toHaveBeenCalledOnce();

    const beforeQuit = registered(mocks.appEvents, "before-quit");
    const firstQuit = { preventDefault: vi.fn() };
    const repeatedQuit = { preventDefault: vi.fn() };
    beforeQuit(firstQuit);
    beforeQuit(repeatedQuit);

    expect(firstQuit.preventDefault).toHaveBeenCalledOnce();
    expect(repeatedQuit.preventDefault).toHaveBeenCalledOnce();
    expect(mocks.daemon.stop).toHaveBeenCalledOnce();

    const closeDuringShutdown = { preventDefault: vi.fn() };
    close(closeDuringShutdown);
    expect(closeDuringShutdown.preventDefault).not.toHaveBeenCalled();
    expect(mocks.window.hide).toHaveBeenCalledOnce();

    stop.resolve();
    await vi.waitFor(() => expect(mocks.app.quit).toHaveBeenCalledTimes(2));
  });

  it("stops the daemon before a signal exits Electron", async () => {
    const stop = deferred();
    mocks.daemon.stop.mockReturnValue(stop.promise);
    await bootMain();

    const signal = addedSignalListener("SIGTERM");
    signal("SIGTERM");

    expect(mocks.daemon.stop).toHaveBeenCalledOnce();
    expect(mocks.app.exit).not.toHaveBeenCalled();

    stop.resolve();
    await vi.waitFor(() => expect(mocks.app.exit).toHaveBeenCalledWith(0));
  });

  it("still exits after signal cleanup rejects", async () => {
    mocks.daemon.stop.mockRejectedValue(new Error("daemon stop failed"));
    await bootMain();

    addedSignalListener("SIGINT")("SIGINT");

    await vi.waitFor(() => expect(mocks.app.exit).toHaveBeenCalledWith(0));
  });
});

async function bootMain(): Promise<void> {
  await import("./main.js");
  await vi.waitFor(() => expect(mocks.electron.Tray).toHaveBeenCalledOnce());
}

function registered(events: Map<string, Listener[]>, name: string): Listener {
  const listeners = events.get(name) ?? [];
  expect(listeners).toHaveLength(1);
  const listener = listeners[0];
  if (!listener) throw new Error(`${name} listener was not registered`);
  return listener;
}

function addedSignalListener(signal: "SIGINT" | "SIGTERM"): NodeJS.SignalsListener {
  const listener = process.listeners(signal).find((candidate) => !originalSignalListeners[signal].has(candidate));
  if (!listener) throw new Error(`${signal} listener was not registered`);
  return listener as NodeJS.SignalsListener;
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let completePromise: (() => void) | undefined;
  const promise = new Promise<void>((complete) => {
    completePromise = complete;
  });
  return { promise, resolve: () => completePromise?.() };
}
