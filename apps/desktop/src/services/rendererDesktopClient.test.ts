import { describe, expect, it, vi } from "vitest";
import type { DesktopBridge } from "../contracts/bridge";
import { ElectronDesktopClient } from "./electronDesktopClient";
import { MockDesktopClient } from "./mockDesktopClient";
import { createRendererDesktopClient } from "./rendererDesktopClient";

const bridge: DesktopBridge = {
  request: vi.fn(),
  onDaemonStatus: vi.fn(() => () => undefined),
  onConversationTurnEvents: vi.fn(() => () => undefined),
  onNavigationRoute: vi.fn(() => () => undefined),
};

describe("createRendererDesktopClient", () => {
  it("uses the isolated Electron bridge when it is present", () => {
    const selection = createRendererDesktopClient(bridge, false);
    expect(selection.kind).toBe("ready");
    if (selection.kind === "ready") expect(selection.client).toBeInstanceOf(ElectronDesktopClient);
  });

  it("allows sample data only behind the explicit browser-preview flag", () => {
    const selection = createRendererDesktopClient(undefined, true);
    expect(selection.kind).toBe("ready");
    if (selection.kind === "ready") expect(selection.client).toBeInstanceOf(MockDesktopClient);
  });

  it("fails closed when preload is absent outside browser preview", () => {
    expect(createRendererDesktopClient(undefined, false)).toEqual({ kind: "bridge_unavailable" });
  });
});
