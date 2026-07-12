// @vitest-environment node
import { afterAll, describe, expect, it, vi } from "vitest";
import { DaemonResponseError, DaemonTransportError } from "./daemon/DaemonRpcClient.js";
import { ErrorCode } from "./generated/daemon/v1/daemon.js";

const mocks = vi.hoisted(() => {
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
    on: vi.fn(),
    once: vi.fn(),
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
    on: vi.fn(),
    setContextMenu: vi.fn(),
    setImage: vi.fn(),
    setToolTip: vi.fn(),
  };
  const ipcMain = {
    handle: vi.fn(),
    on: vi.fn(),
  };
  const dialog = {
    showOpenDialog: vi.fn(),
  };
  const daemon = {
    acknowledgeConversationForkDelivery: vi.fn(),
    branchConversationThread: vi.fn(),
    editAndBranchConversationTurn: vi.fn(),
    getConversationForkMetadata: vi.fn(),
    getDesktopPreferences: vi.fn(() => Promise.resolve({
      keepRunningInNotificationArea: true,
      revision: 0,
      updatedAtUnixMs: 0,
    })),
    regenerateConversationTurn: vi.fn(),
    importArtifact: vi.fn(),
    openArtifact: vi.fn(),
    removeArtifact: vi.fn(),
    subscribe: vi.fn(() => vi.fn()),
    subscribeConversationTurnEvents: vi.fn(() => Promise.resolve(vi.fn())),
  };
  return {
    BrowserWindow,
    daemon,
    dialog,
    frame,
    ipcMain,
    window,
    webContents,
    electron: {
      app: {
        commandLine: { appendSwitch: vi.fn() },
        disableHardwareAcceleration: vi.fn(),
        exit: vi.fn(),
        getAppPath: vi.fn(() => "/app"),
        getPath: vi.fn(() => "/tmp"),
        getVersion: vi.fn(() => "0.1.0"),
        isPackaged: true,
        on: vi.fn(),
        quit: vi.fn(),
        relaunch: vi.fn(),
        requestSingleInstanceLock: vi.fn(() => true),
        whenReady: vi.fn(() => Promise.resolve()),
      },
      autoUpdater: {
        on: vi.fn(),
        setFeedURL: vi.fn(),
        checkForUpdates: vi.fn(),
        quitAndInstall: vi.fn(),
      },
      BrowserWindow,
      dialog,
      ipcMain,
      Menu: { buildFromTemplate: vi.fn(() => ({})) },
      nativeImage: { createFromPath: vi.fn(() => ({ isEmpty: () => false })) },
      nativeTheme: { on: vi.fn(), shouldUseDarkColors: false },
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
  };
});

vi.mock("electron", () => mocks.electron);
vi.mock("./daemon/DaemonSupervisor.js", () => ({
  DaemonSupervisor: mocks.DaemonSupervisor,
}));
vi.mock("./trayIcon.js", () => ({
  resolveTrayIconPath: vi.fn(() => "/tray.png"),
}));

const originalSignalListeners = {
  SIGINT: new Set(process.listeners("SIGINT")),
  SIGTERM: new Set(process.listeners("SIGTERM")),
};
const resourcesPathDescriptor = Object.getOwnPropertyDescriptor(process, "resourcesPath");

afterAll(() => {
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    for (const listener of process.listeners(signal)) {
      if (!originalSignalListeners[signal].has(listener)) process.removeListener(signal, listener);
    }
  }
  if (resourcesPathDescriptor) Object.defineProperty(process, "resourcesPath", resourcesPathDescriptor);
  else Reflect.deleteProperty(process, "resourcesPath");
});

describe("main fork bridge registration", () => {
  it("dispatches exact fork requests and registers watches only for started tasks", async () => {
    Object.defineProperty(process, "resourcesPath", {
      configurable: true,
      value: "/resources",
    });
    await import("./main.js");
    await vi.waitFor(() => expect(mocks.ipcMain.handle).toHaveBeenCalledWith(
      "desktop:request",
      expect.any(Function),
    ));
    const registration = mocks.ipcMain.handle.mock.calls.find(([channel]) =>
      channel === "desktop:request"
    );
    const handler = registration?.[1] as (
      event: unknown,
      request: unknown,
    ) => Promise<unknown>;
    const event = { sender: mocks.webContents, senderFrame: mocks.frame };

    const branchFork = { childThread: { id: "thread-branch" }, startedTurn: undefined };
    mocks.daemon.branchConversationThread.mockResolvedValueOnce(branchFork);
    await expect(handler(event, {
      kind: "daemon.branchConversationThread",
      sourceTurnId: "turn-source",
      expectedRevision: 7,
      idempotencyKey: "branch-command-1",
    })).resolves.toEqual({ kind: "daemon.conversationFork", fork: branchFork });
    expect(mocks.daemon.branchConversationThread).toHaveBeenCalledWith(
      "turn-source",
      7,
      "branch-command-1",
    );
    expect(mocks.daemon.subscribeConversationTurnEvents).not.toHaveBeenCalled();

    const editFork = {
      childThread: { id: "thread-edit" },
      startedTurn: { turnId: "turn-edit", state: "reserved" },
    };
    mocks.daemon.editAndBranchConversationTurn.mockResolvedValueOnce(editFork);
    await expect(handler(event, {
      kind: "daemon.editAndBranchConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: 7,
      content: "Edited question",
      idempotencyKey: "edit-command-1",
    })).resolves.toEqual({ kind: "daemon.conversationFork", fork: editFork });
    expect(mocks.daemon.editAndBranchConversationTurn).toHaveBeenCalledWith(
      "turn-source",
      7,
      "Edited question",
      "edit-command-1",
    );
    await vi.waitFor(() => expect(mocks.daemon.subscribeConversationTurnEvents)
      .toHaveBeenCalledWith("turn-edit", expect.any(Function), expect.any(Function)));

    const regenerateFork = {
      childThread: { id: "thread-regenerate" },
      startedTurn: { turnId: "turn-regenerate", state: "provider_started" },
    };
    mocks.daemon.regenerateConversationTurn.mockResolvedValueOnce(regenerateFork);
    await expect(handler(event, {
      kind: "daemon.regenerateConversationTurn",
      sourceTurnId: "turn-source",
      expectedRevision: 7,
      idempotencyKey: "regenerate-command-1",
    })).resolves.toEqual({ kind: "daemon.conversationFork", fork: regenerateFork });
    expect(mocks.daemon.regenerateConversationTurn).toHaveBeenCalledWith(
      "turn-source",
      7,
      "regenerate-command-1",
    );
    await vi.waitFor(() => expect(mocks.daemon.subscribeConversationTurnEvents)
      .toHaveBeenCalledWith("turn-regenerate", expect.any(Function), expect.any(Function)));

    const metadata = { lineage: { origin: "original" }, inheritedAssistantOutcomes: [], familyThreads: [] };
    mocks.daemon.getConversationForkMetadata.mockResolvedValueOnce(metadata);
    await expect(handler(event, {
      kind: "daemon.getConversationForkMetadata",
      threadId: "thread-child",
    })).resolves.toEqual({ kind: "daemon.conversationForkMetadata", metadata });
    expect(mocks.daemon.getConversationForkMetadata).toHaveBeenCalledWith("thread-child");

    const delivery = {
      childThreadId: "thread-child",
      state: "acknowledged",
      revision: 1,
    };
    mocks.daemon.acknowledgeConversationForkDelivery.mockResolvedValueOnce(delivery);
    await expect(handler(event, {
      kind: "daemon.acknowledgeConversationForkDelivery",
      childThreadId: "thread-child",
      expectedRevision: 0,
      idempotencyKey: "fork-delivery-ack-1",
    })).resolves.toEqual({ kind: "daemon.conversationForkDelivery", delivery });
    expect(mocks.daemon.acknowledgeConversationForkDelivery).toHaveBeenCalledWith(
      "thread-child",
      0,
      "fork-delivery-ack-1",
    );

    const ambiguous = new Error("daemon stream closed after mutation dispatch");
    mocks.daemon.branchConversationThread.mockRejectedValueOnce(ambiguous);
    await expect(handler(event, {
      kind: "daemon.branchConversationThread",
      sourceTurnId: "turn-source",
      expectedRevision: 7,
      idempotencyKey: "branch-command-ambiguous",
    })).rejects.toBe(ambiguous);
  });

  it("owns native artifact selection and forwards only the selected exact intent", async () => {
    Object.defineProperty(process, "resourcesPath", {
      configurable: true,
      value: "/resources",
    });
    await import("./main.js");
    await vi.waitFor(() => expect(mocks.ipcMain.handle).toHaveBeenCalledWith(
      "desktop:request",
      expect.any(Function),
    ));
    const registration = mocks.ipcMain.handle.mock.calls.find(([channel]) =>
      channel === "desktop:request"
    );
    const handler = registration?.[1] as (
      event: unknown,
      request: unknown,
    ) => Promise<unknown>;
    const event = { sender: mocks.webContents, senderFrame: mocks.frame };

    mocks.daemon.importArtifact.mockClear();
    mocks.dialog.showOpenDialog.mockResolvedValueOnce({ canceled: true, filePaths: [] });
    await expect(handler(event, {
      kind: "daemon.importArtifact",
      projectId: "project-1",
      idempotencyKey: "import-cancelled",
    })).resolves.toEqual({ kind: "daemon.artifactImportCancelled" });
    expect(mocks.daemon.importArtifact).not.toHaveBeenCalled();

    await expect(handler(event, {
      kind: "daemon.importArtifact",
      projectId: "project-1",
      idempotencyKey: "import-forged",
      sourcePath: "/renderer/private.txt",
    })).rejects.toThrow("unsupported fields");
    expect(mocks.dialog.showOpenDialog).toHaveBeenCalledTimes(1);

    for (const filePaths of [["/tmp/one.txt", "/tmp/two.txt"], ["relative/private.txt"]]) {
      mocks.dialog.showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths });
      await expect(handler(event, {
        kind: "daemon.importArtifact",
        projectId: "project-1",
        idempotencyKey: `import-invalid-${filePaths.length}`,
      })).rejects.toThrow("native artifact selection is invalid");
    }
    expect(mocks.daemon.importArtifact).not.toHaveBeenCalled();

    const sourcePath = "/home/person/private/source-canary.pdf";
    const artifact = {
      id: "artifact-1",
      projectId: "project-1",
      name: "source-canary.pdf",
      mediaType: "application/pdf",
      byteSize: 42,
      contentVersion: 1,
      state: "available",
      revision: 1,
      createdAtUnixMs: 1,
      updatedAtUnixMs: 2,
    };
    mocks.dialog.showOpenDialog.mockResolvedValueOnce({ canceled: false, filePaths: [sourcePath] });
    mocks.daemon.importArtifact.mockResolvedValueOnce(artifact);
    const response = await handler(event, {
      kind: "daemon.importArtifact",
      projectId: "project-1",
      idempotencyKey: "import-valid",
    });
    expect(mocks.dialog.showOpenDialog).toHaveBeenLastCalledWith(mocks.window, {
      title: "Import a file to Grok Desktop",
      buttonLabel: "Import",
      properties: ["openFile"],
    });
    expect(mocks.daemon.importArtifact).toHaveBeenCalledWith(
      "project-1",
      "source-canary.pdf",
      "application/pdf",
      sourcePath,
      "import-valid",
    );
    expect(response).toEqual({ kind: "daemon.artifactImported", artifact });
    expect(JSON.stringify(response)).not.toContain(sourcePath);

    const receipt = { artifactId: "artifact-1", contentVersion: 7, status: "opened" };
    mocks.daemon.openArtifact.mockResolvedValueOnce(receipt);
    await expect(handler(event, {
      kind: "daemon.openArtifact",
      artifactId: "artifact-1",
      contentVersion: 7,
      idempotencyKey: "open-exact",
    })).resolves.toEqual({ kind: "daemon.artifactOpened", receipt });
    expect(mocks.daemon.openArtifact).toHaveBeenCalledWith(
      "artifact-1",
      7,
      "open-exact",
    );

    const removedArtifact = {
      ...artifact,
      mediaType: undefined,
      byteSize: undefined,
      contentVersion: undefined,
      state: "deleted",
      revision: 8,
      updatedAtUnixMs: 3,
    };
    mocks.daemon.removeArtifact.mockResolvedValueOnce({ status: "removed", artifact: removedArtifact });
    await expect(handler(event, {
      kind: "daemon.removeArtifact",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      idempotencyKey: "remove-exact",
    })).resolves.toEqual({ kind: "daemon.artifactRemoved", artifact: removedArtifact });
    expect(mocks.daemon.removeArtifact).toHaveBeenCalledWith(
      "artifact-1",
      7,
      7,
      "remove-exact",
    );

    mocks.daemon.removeArtifact.mockResolvedValueOnce({
      status: "pending",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      tombstone: removedArtifact,
    });
    await expect(handler(event, {
      kind: "daemon.removeArtifact",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      idempotencyKey: "remove-pending",
    })).resolves.toEqual({
      kind: "daemon.artifactRemovalPending",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      tombstone: removedArtifact,
    });

    mocks.daemon.removeArtifact.mockRejectedValueOnce(new DaemonResponseError(
      "/private/source-canary conflict",
      ErrorCode.ERROR_CODE_CONFLICT,
      false,
    ));
    const rejection = await handler(event, {
      kind: "daemon.removeArtifact",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      idempotencyKey: "remove-rejected",
    });
    expect(rejection).toEqual({
      kind: "daemon.artifactRemovalRejected",
      reason: "conflict",
    });
    expect(JSON.stringify(rejection)).not.toContain("source-canary");

    for (const code of [
      ErrorCode.ERROR_CODE_INTERNAL,
      ErrorCode.ERROR_CODE_INTEGRITY_FAILURE,
    ]) {
      const daemonAmbiguity = new DaemonResponseError(
        "daemon response cannot prove pre-reservation rejection",
        code,
        false,
      );
      mocks.daemon.removeArtifact.mockRejectedValueOnce(daemonAmbiguity);
      await expect(handler(event, {
        kind: "daemon.removeArtifact",
        artifactId: "artifact-1",
        expectedRevision: 7,
        expectedContentVersion: 7,
        idempotencyKey: `remove-ambiguous-${code}`,
      })).rejects.toBe(daemonAmbiguity);
    }

    const removalAmbiguity = new DaemonTransportError("daemon stream closed after dispatch");
    mocks.daemon.removeArtifact.mockRejectedValueOnce(removalAmbiguity);
    await expect(handler(event, {
      kind: "daemon.removeArtifact",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      idempotencyKey: "remove-ambiguous",
    })).rejects.toBe(removalAmbiguity);
  });
});
