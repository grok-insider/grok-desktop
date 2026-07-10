// @vitest-environment node
import { readFileSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import vm from "node:vm";
import { describe, expect, it, vi } from "vitest";
import type { DesktopBridge } from "../src/contracts/bridge.js";

const mocks = vi.hoisted(() => ({
  exposeInMainWorld: vi.fn(),
  invoke: vi.fn(() => Promise.resolve({ kind: "daemon.artifactImportCancelled" })),
  on: vi.fn(),
  removeListener: vi.fn(),
  send: vi.fn(),
}));

vi.mock("electron", () => ({
  contextBridge: { exposeInMainWorld: mocks.exposeInMainWorld },
  ipcRenderer: {
    invoke: mocks.invoke,
    on: mocks.on,
    removeListener: mocks.removeListener,
    send: mocks.send,
  },
}));

describe("isolated preload artifact bridge", () => {
  it("rejects renderer-selected paths before invoking Electron main", async () => {
    const source = readFileSync(new URL("./preload.cts", import.meta.url), "utf8");
    const compiled = stripTypeScriptTypes(
      source.replace(
        'import { contextBridge, ipcRenderer } from "electron";',
        "const { contextBridge, ipcRenderer } = globalThis.__electron;",
      ),
      { mode: "strip" },
    );
    vm.runInNewContext(compiled, {
      __electron: {
        contextBridge: { exposeInMainWorld: mocks.exposeInMainWorld },
        ipcRenderer: {
          invoke: mocks.invoke,
          on: mocks.on,
          removeListener: mocks.removeListener,
          send: mocks.send,
        },
      },
    });
    const bridge = mocks.exposeInMainWorld.mock.calls[0]?.[1] as DesktopBridge;
    const forged = {
      kind: "daemon.importArtifact",
      projectId: "project-1",
      idempotencyKey: "import-1",
      sourcePath: "/renderer/chosen/private.txt",
    };

    await expect(bridge.request(forged as never)).rejects.toThrow(
      "artifact import request contains unsupported fields",
    );
    expect(mocks.invoke).not.toHaveBeenCalled();

    await bridge.request({
      kind: "daemon.importArtifact",
      projectId: "project-1",
      idempotencyKey: "import-1",
    });
    expect(mocks.invoke).toHaveBeenCalledWith("desktop:request", {
      kind: "daemon.importArtifact",
      projectId: "project-1",
      idempotencyKey: "import-1",
    });

    await bridge.request({
      kind: "daemon.removeArtifact",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      idempotencyKey: "remove-1",
    });
    expect(mocks.invoke).toHaveBeenLastCalledWith("desktop:request", {
      kind: "daemon.removeArtifact",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      idempotencyKey: "remove-1",
    });
    await expect(bridge.request({
      kind: "daemon.removeArtifact",
      artifactId: "artifact-1",
      expectedRevision: 7,
      expectedContentVersion: 7,
      idempotencyKey: "remove-forged",
      storagePath: "/renderer/chosen/private.txt",
    } as never)).rejects.toThrow("artifact removal request contains unsupported fields");
    expect(mocks.invoke).toHaveBeenCalledTimes(2);
  });
});
