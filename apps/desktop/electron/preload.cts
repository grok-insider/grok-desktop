import { contextBridge, ipcRenderer } from "electron";
import type {
  BridgeRequest,
  DaemonStatus,
  DesktopBridge,
  DesktopConversationTurnEventDelivery,
  DesktopNavigationDelivery,
} from "../src/contracts/bridge.js";

function requestForIpc(request: BridgeRequest): BridgeRequest {
  if (request.kind === "daemon.importArtifact") {
    const keys = Object.keys(request);
    if (
      keys.length !== 3
      || !keys.includes("kind")
      || !keys.includes("projectId")
      || !keys.includes("idempotencyKey")
    ) {
      throw new TypeError("artifact import request contains unsupported fields");
    }
    return {
      kind: request.kind,
      projectId: request.projectId,
      idempotencyKey: request.idempotencyKey,
    };
  }
  if (request.kind === "daemon.openArtifact") {
    const keys = Object.keys(request);
    if (
      keys.length !== 4
      || !keys.includes("kind")
      || !keys.includes("artifactId")
      || !keys.includes("contentVersion")
      || !keys.includes("idempotencyKey")
    ) {
      throw new TypeError("artifact open request contains unsupported fields");
    }
    return {
      kind: request.kind,
      artifactId: request.artifactId,
      contentVersion: request.contentVersion,
      idempotencyKey: request.idempotencyKey,
    };
  }
  if (request.kind === "daemon.removeArtifact") {
    const keys = Object.keys(request);
    if (
      keys.length !== 5
      || !keys.includes("kind")
      || !keys.includes("artifactId")
      || !keys.includes("expectedRevision")
      || !keys.includes("expectedContentVersion")
      || !keys.includes("idempotencyKey")
    ) {
      throw new TypeError("artifact removal request contains unsupported fields");
    }
    return {
      kind: request.kind,
      artifactId: request.artifactId,
      expectedRevision: request.expectedRevision,
      expectedContentVersion: request.expectedContentVersion,
      idempotencyKey: request.idempotencyKey,
    };
  }
  return request;
}

const bridge: DesktopBridge = {
  request: async (request) => ipcRenderer.invoke("desktop:request", requestForIpc(request)),
  onDaemonStatus: (listener) => {
    const handler = (_event: Electron.IpcRendererEvent, status: DaemonStatus) => listener(status);
    ipcRenderer.on("daemon:status", handler);
    return () => ipcRenderer.removeListener("daemon:status", handler);
  },
  onConversationTurnEvents: (listener) => {
    const handler = (
      _event: Electron.IpcRendererEvent,
      delivery: DesktopConversationTurnEventDelivery,
    ) => {
      void Promise.resolve()
        .then(() => listener({ turnId: delivery.turnId, batch: delivery.batch }))
        .then(() => ipcRenderer.send("desktop:conversation-events-ack", delivery.deliveryId))
        .catch(() => undefined);
    };
    ipcRenderer.on("desktop:conversation-turn-events", handler);
    ipcRenderer.send("desktop:conversation-events-ready");
    return () => ipcRenderer.removeListener("desktop:conversation-turn-events", handler);
  },
  onNavigationRoute: (listener) => {
    const handler = (_event: Electron.IpcRendererEvent, delivery: DesktopNavigationDelivery) => {
      listener(delivery.route);
      ipcRenderer.send("desktop:navigation-ack", delivery.deliveryId);
    };
    ipcRenderer.on("desktop:navigation-route", handler);
    ipcRenderer.send("desktop:navigation-ready");
    return () => ipcRenderer.removeListener("desktop:navigation-route", handler);
  },
};

contextBridge.exposeInMainWorld("grokDesktop", bridge);
