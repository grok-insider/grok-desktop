import type { DesktopNavigationDelivery, DesktopNavigationRoute } from "../src/contracts/bridge.js";

/**
 * Bounded activation hand-off between Electron startup events and the renderer.
 * Only the latest validated route is retained while the renderer is unavailable.
 */
export class DesktopDeepLinkDelivery {
  private pending: DesktopNavigationDelivery | undefined;
  private readyWebContentsId: number | undefined;
  private lastSent: { deliveryId: number; webContentsId: number } | undefined;
  private nextDeliveryId = 1;

  queue(route: DesktopNavigationRoute): number {
    const deliveryId = this.nextDeliveryId;
    this.nextDeliveryId = this.nextDeliveryId === Number.MAX_SAFE_INTEGER ? 1 : this.nextDeliveryId + 1;
    this.pending = { deliveryId, route };
    this.lastSent = undefined;
    return deliveryId;
  }

  markRendererReady(webContentsId: number): void {
    if (this.readyWebContentsId !== webContentsId) this.lastSent = undefined;
    this.readyWebContentsId = webContentsId;
  }

  markRendererUnavailable(webContentsId: number): void {
    if (this.readyWebContentsId === webContentsId) {
      this.readyWebContentsId = undefined;
      this.lastSent = undefined;
    }
  }

  deliver(webContentsId: number, send: (delivery: DesktopNavigationDelivery) => void): boolean {
    if (this.readyWebContentsId !== webContentsId || !this.pending) return false;
    if (
      this.lastSent?.deliveryId === this.pending.deliveryId
      && this.lastSent.webContentsId === webContentsId
    ) {
      return false;
    }
    this.lastSent = { deliveryId: this.pending.deliveryId, webContentsId };
    send(this.pending);
    return true;
  }

  acknowledge(webContentsId: number, deliveryId: number): boolean {
    if (
      this.readyWebContentsId !== webContentsId
      || this.pending?.deliveryId !== deliveryId
      || this.lastSent?.deliveryId !== deliveryId
      || this.lastSent.webContentsId !== webContentsId
    ) {
      return false;
    }
    this.pending = undefined;
    this.lastSent = undefined;
    return true;
  }
}
