import type {
  DaemonConversationTurnEventBatch,
  DesktopConversationTurnEventDelivery,
} from "../src/contracts/bridge.js";

// Remain below the daemon client's five-second listener budget so the main
// process owns the retry reason and never races two independent timeouts.
const DEFAULT_ACK_TIMEOUT_MS = 4_000;

type PendingDelivery = {
  senderId: number;
  resolve(): void;
  reject(error: Error): void;
  timer: ReturnType<typeof setTimeout>;
};

/**
 * Identifies a renderer hand-off failure after the daemon batch itself passed
 * protocol validation. Main may safely recreate the replay-from-zero channel
 * for this class; daemon protocol/integrity failures must remain terminal.
 */
export class DesktopConversationEventDeliveryError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "DesktopConversationEventDeliveryError";
  }
}

/**
 * Bounded, acknowledged hand-off for durable Chat events. A caller must not
 * advance its daemon cursor until the returned promise resolves.
 */
export class DesktopConversationEventDeliveryTracker {
  private readonly pending = new Map<number, PendingDelivery>();
  private nextDeliveryId = 1;

  constructor(private readonly ackTimeoutMs = DEFAULT_ACK_TIMEOUT_MS) {
    if (!Number.isSafeInteger(ackTimeoutMs) || ackTimeoutMs < 1 || ackTimeoutMs > 60_000) {
      throw new TypeError("conversation event acknowledgement timeout is invalid");
    }
  }

  deliver(
    senderId: number,
    turnId: string,
    batch: DaemonConversationTurnEventBatch,
    send: (delivery: DesktopConversationTurnEventDelivery) => void,
  ): Promise<void> {
    const deliveryId = this.allocateId();
    return new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(deliveryId);
        reject(new DesktopConversationEventDeliveryError(
          "conversation event renderer acknowledgement timed out",
        ));
      }, this.ackTimeoutMs);
      this.pending.set(deliveryId, { senderId, resolve, reject, timer });
      try {
        send({ deliveryId, turnId, batch });
      } catch (error) {
        this.pending.delete(deliveryId);
        clearTimeout(timer);
        reject(new DesktopConversationEventDeliveryError(
          "conversation event delivery failed",
          { cause: error },
        ));
      }
    });
  }

  acknowledge(senderId: number, deliveryId: number): boolean {
    const pending = this.pending.get(deliveryId);
    if (!pending || pending.senderId !== senderId) return false;
    this.pending.delete(deliveryId);
    clearTimeout(pending.timer);
    pending.resolve();
    return true;
  }

  markRendererUnavailable(senderId: number): void {
    for (const [deliveryId, pending] of this.pending) {
      if (pending.senderId !== senderId) continue;
      this.pending.delete(deliveryId);
      clearTimeout(pending.timer);
      pending.reject(new DesktopConversationEventDeliveryError(
        "conversation event renderer became unavailable",
      ));
    }
  }

  private allocateId(): number {
    for (let attempts = 0; attempts < Number.MAX_SAFE_INTEGER; attempts += 1) {
      const candidate = this.nextDeliveryId;
      this.nextDeliveryId = candidate === Number.MAX_SAFE_INTEGER ? 1 : candidate + 1;
      if (!this.pending.has(candidate)) return candidate;
    }
    throw new Error("conversation event delivery identifiers are exhausted");
  }
}
