import { describe, expect, it } from "vitest";

import type { DesktopConversationTurnEventNotification } from "../contracts/bridge";
import { applyConversationEventNotification } from "./conversationEventProjection";

const turnId = "turn-1";

describe("conversation event projection", () => {
  it("projects contiguous UTF-8 text and terminal state exactly", () => {
    const applied = applyConversationEventNotification(undefined, notification([
      { sequence: 1, turnId, kind: "created" },
      {
        sequence: 2,
        turnId,
        kind: "state_changed",
        fromState: "reserved",
        toState: "provider_started",
      },
      { sequence: 3, turnId, kind: "text_appended", startUtf8Offset: 0, text: "é" },
      { sequence: 4, turnId, kind: "text_appended", startUtf8Offset: 2, text: "🙂 done" },
      {
        sequence: 5,
        turnId,
        kind: "state_changed",
        fromState: "provider_started",
        toState: "completed",
      },
    ], 5));

    expect(applied.reachedTerminal).toBe(true);
    expect(applied.projection).toMatchObject({
      state: "completed",
      revision: 2,
      text: "é🙂 done",
      textUtf8Bytes: 11,
      lastSequence: 5,
      deliveryCursor: 5,
    });
  });

  it("accepts only an identical replay-zero prefix before continuing", () => {
    const prefix = notification([
      { sequence: 1, turnId, kind: "created" },
      {
        sequence: 2,
        turnId,
        kind: "state_changed",
        fromState: "reserved",
        toState: "provider_started",
      },
      { sequence: 3, turnId, kind: "text_appended", startUtf8Offset: 0, text: "first" },
    ], 3);
    const initial = applyConversationEventNotification(undefined, prefix).projection;
    const replay = applyConversationEventNotification(initial, prefix);
    expect(replay.addedEvents).toEqual([]);
    expect(replay.projection.text).toBe("first");

    const continued = applyConversationEventNotification(replay.projection, notification([
      { sequence: 4, turnId, kind: "text_appended", startUtf8Offset: 5, text: " second" },
    ], 4));
    expect(continued.projection.text).toBe("first second");

    const changedReplay = notification([
      { sequence: 1, turnId, kind: "created" },
      {
        sequence: 2,
        turnId,
        kind: "state_changed",
        fromState: "reserved",
        toState: "provider_started",
      },
      { sequence: 3, turnId, kind: "text_appended", startUtf8Offset: 0, text: "forged" },
    ], 3);
    expect(() => applyConversationEventNotification(continued.projection, changedReplay)).toThrow(
      "replay changed retained history",
    );
  });

  it("surfaces an identical replayed terminal edge for canonical reconciliation", () => {
    const terminalBatch = notification([
      { sequence: 1, turnId, kind: "created" },
      {
        sequence: 2,
        turnId,
        kind: "state_changed",
        fromState: "reserved",
        toState: "provider_started",
      },
      { sequence: 3, turnId, kind: "text_appended", startUtf8Offset: 0, text: "partial" },
      {
        sequence: 4,
        turnId,
        kind: "state_changed",
        fromState: "provider_started",
        toState: "failed",
      },
    ], 4);
    const retained = applyConversationEventNotification(undefined, terminalBatch).projection;

    const replay = applyConversationEventNotification(retained, terminalBatch);

    expect(replay.addedEvents).toEqual([]);
    expect(replay.reachedTerminal).toBe(true);
    expect(replay.projection).toEqual(retained);
  });

  it.each([
    {
      name: "owner",
      value: notification([{ sequence: 1, turnId: "turn-other", kind: "created" }], 1),
    },
    {
      name: "sequence gap",
      value: notification([{ sequence: 2, turnId, kind: "created" }], 2),
    },
    {
      name: "illegal transition",
      value: notification([
        { sequence: 1, turnId, kind: "created" },
        {
          sequence: 2,
          turnId,
          kind: "state_changed",
          fromState: "reserved",
          toState: "completed",
        },
      ], 2),
    },
    {
      name: "UTF-8 offset",
      value: notification([
        { sequence: 1, turnId, kind: "created" },
        {
          sequence: 2,
          turnId,
          kind: "state_changed",
          fromState: "reserved",
          toState: "provider_started",
        },
        { sequence: 3, turnId, kind: "text_appended", startUtf8Offset: 1, text: "x" },
      ], 3),
    },
    {
      name: "unsupported control",
      value: notification([
        { sequence: 1, turnId, kind: "created" },
        {
          sequence: 2,
          turnId,
          kind: "state_changed",
          fromState: "reserved",
          toState: "provider_started",
        },
        { sequence: 3, turnId, kind: "text_appended", startUtf8Offset: 0, text: "bad\0text" },
      ], 3),
    },
  ])("rejects forged $name events", ({ value }) => {
    expect(() => applyConversationEventNotification(undefined, value)).toThrow();
  });

  it("rejects post-terminal events and malformed one-extra metadata", () => {
    const terminal = applyConversationEventNotification(undefined, notification([
      { sequence: 1, turnId, kind: "created" },
      {
        sequence: 2,
        turnId,
        kind: "state_changed",
        fromState: "reserved",
        toState: "cancelled",
      },
    ], 2)).projection;
    expect(() => applyConversationEventNotification(terminal, notification([
      { sequence: 3, turnId, kind: "text_appended", startUtf8Offset: 0, text: "late" },
    ], 3))).toThrow("followed a terminal state");

    expect(() => applyConversationEventNotification(undefined, {
      turnId,
      batch: { events: [], nextSequence: 0, hasMore: true },
    })).toThrow("empty conversation event batch");
  });
});

function notification(
  events: DesktopConversationTurnEventNotification["batch"]["events"],
  nextSequence: number,
): DesktopConversationTurnEventNotification {
  return { turnId, batch: { events, nextSequence, hasMore: false } };
}
