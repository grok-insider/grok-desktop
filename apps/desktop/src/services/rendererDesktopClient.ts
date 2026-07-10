import type { DesktopBridge } from "../contracts/bridge";
import type { DesktopClient } from "./desktopClient";
import { ElectronDesktopClient } from "./electronDesktopClient";
import { MockDesktopClient } from "./mockDesktopClient";

export type RendererDesktopClient =
  | { kind: "ready"; client: DesktopClient }
  | { kind: "bridge_unavailable" };

/**
 * Selects the renderer boundary explicitly. Sample data is available only for
 * the browser-preview command; a missing Electron preload bridge fails closed.
 */
export function createRendererDesktopClient(
  bridge: DesktopBridge | undefined,
  browserPreviewEnabled: boolean,
): RendererDesktopClient {
  if (bridge) return { kind: "ready", client: new ElectronDesktopClient(bridge) };
  if (browserPreviewEnabled) return { kind: "ready", client: new MockDesktopClient() };
  return { kind: "bridge_unavailable" };
}
