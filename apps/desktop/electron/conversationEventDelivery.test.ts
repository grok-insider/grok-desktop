// @vitest-environment node
import { afterEach, describe, expect, it, vi } from "vitest";
import type { DaemonConversationTurnEventBatch } from "../src/contracts/bridge.js";
import {
  DesktopConversationEventDeliveryError,
  DesktopConversationEventDeliveryTracker,
} from "./conversationEventDelivery.js";

const batch: DaemonConversationTurnEventBatch = {
  events: [{ sequence: 1, turnId: "turn-1", kind: "created" }],
  nextSequence: 1,
  hasMore: false,
};

afterEach(() => vi.useRealTimers());

describe("desktop conversation event delivery", () => {
  it("advances only after the intended renderer acknowledges the exact delivery", async () => {
    const tracker = new DesktopConversationEventDeliveryTracker();
    const send = vi.fn();
    const delivered = tracker.deliver(7, "turn-1", batch, send);

    expect(send).toHaveBeenCalledWith({ deliveryId: 1, turnId: "turn-1", batch });
    expect(tracker.acknowledge(8, 1)).toBe(false);
    expect(tracker.acknowledge(7, 2)).toBe(false);
    expect(tracker.acknowledge(7, 1)).toBe(true);
    await expect(delivered).resolves.toBeUndefined();
    expect(tracker.acknowledge(7, 1)).toBe(false);
  });

  it("rejects an in-flight delivery when its renderer becomes unavailable", async () => {
    const tracker = new DesktopConversationEventDeliveryTracker();
    const delivered = tracker.deliver(4, "turn-1", batch, vi.fn());

    tracker.markRendererUnavailable(5);
    tracker.markRendererUnavailable(4);
    await expect(delivered).rejects.toThrow("renderer became unavailable");
  });

  it("rejects send failures and acknowledgement timeouts without retaining an ack", async () => {
    vi.useFakeTimers();
    const tracker = new DesktopConversationEventDeliveryTracker(10);
    const sendFailure = tracker.deliver(2, "turn-1", batch, () => {
      throw new Error("send failed");
    });
    await expect(sendFailure).rejects.toBeInstanceOf(DesktopConversationEventDeliveryError);
    await expect(sendFailure).rejects.toMatchObject({ cause: expect.objectContaining({ message: "send failed" }) });

    const timedOut = tracker.deliver(2, "turn-1", batch, vi.fn());
    const timeoutExpectation = expect(timedOut).rejects.toThrow("acknowledgement timed out");
    await vi.advanceTimersByTimeAsync(10);
    await timeoutExpectation;
    await expect(timedOut).rejects.toBeInstanceOf(DesktopConversationEventDeliveryError);
    expect(tracker.acknowledge(2, 2)).toBe(false);
  });
});
