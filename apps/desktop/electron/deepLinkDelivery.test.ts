// @vitest-environment node
import { describe, expect, it, vi } from "vitest";
import { DesktopDeepLinkDelivery } from "./deepLinkDelivery.js";

describe("desktop deep-link delivery", () => {
  it("retains a cold activation until the intended renderer is ready", () => {
    const delivery = new DesktopDeepLinkDelivery();
    const send = vi.fn();
    delivery.queue({ version: 1, route: "settings" });

    expect(delivery.deliver(7, send)).toBe(false);
    delivery.markRendererReady(7);
    expect(delivery.deliver(8, send)).toBe(false);
    expect(delivery.deliver(7, send)).toBe(true);
    expect(send).toHaveBeenCalledWith({ deliveryId: 1, route: { version: 1, route: "settings" } });
    expect(delivery.deliver(7, send)).toBe(false);
    expect(delivery.acknowledge(8, 1)).toBe(false);
    expect(delivery.acknowledge(7, 1)).toBe(true);
  });

  it("keeps only the latest validated activation while unavailable", () => {
    const delivery = new DesktopDeepLinkDelivery();
    const send = vi.fn();
    delivery.queue({ version: 1, route: "home" });
    delivery.queue({ version: 1, route: "conversation", threadId: "thread-latest" });
    delivery.markRendererReady(2);

    expect(delivery.deliver(2, send)).toBe(true);
    expect(send).toHaveBeenCalledOnce();
    expect(send).toHaveBeenCalledWith({
      deliveryId: 2,
      route: { version: 1, route: "conversation", threadId: "thread-latest" },
    });
  });

  it("requires a fresh readiness handshake after a renderer reload", () => {
    const delivery = new DesktopDeepLinkDelivery();
    const send = vi.fn();
    delivery.queue({ version: 1, route: "library" });
    delivery.markRendererReady(3);
    expect(delivery.deliver(3, send)).toBe(true);
    delivery.markRendererUnavailable(3);

    expect(delivery.deliver(3, send)).toBe(false);
    delivery.markRendererReady(3);
    expect(delivery.deliver(3, send)).toBe(true);
    expect(send).toHaveBeenCalledTimes(2);
    expect(delivery.acknowledge(3, 1)).toBe(true);
  });
});
