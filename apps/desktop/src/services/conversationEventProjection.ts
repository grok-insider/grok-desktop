import type {
  DaemonConversationTurnEvent,
  DaemonConversationTurnState,
  DesktopConversationTurnEventNotification,
} from "../contracts/bridge";

const MAX_BATCH_SIZE = 100;
const MAX_TEXT_CHUNK_BYTES = 16 * 1024;
const MAX_TEXT_BYTES = 1024 * 1024;
const MAX_TEXT_EVENTS = 4_097;
const TERMINAL_STATES = new Set<DaemonConversationTurnState>([
  "completed",
  "failed",
  "cancelled",
  "interrupted_needs_review",
]);
const STATES = new Set<DaemonConversationTurnState>([
  "reserved",
  "provider_started",
  "completed",
  "failed",
  "cancelled",
  "interrupted_needs_review",
]);
const IDENTIFIER = /^[A-Za-z0-9][A-Za-z0-9._:-]{0,127}$/u;
const encoder = new TextEncoder();

export interface ConversationEventProjection {
  turnId: string;
  state?: DaemonConversationTurnState;
  revision: number;
  text: string;
  textUtf8Bytes: number;
  textEventCount: number;
  lastSequence: number;
  deliveryCursor: number;
  events: DaemonConversationTurnEvent[];
}

export interface AppliedConversationEventBatch {
  projection: ConversationEventProjection;
  addedEvents: DaemonConversationTurnEvent[];
  reachedTerminal: boolean;
}

export function applyConversationEventNotification(
  current: ConversationEventProjection | undefined,
  notification: DesktopConversationTurnEventNotification,
): AppliedConversationEventBatch {
  validateNotificationShape(notification);
  const projection = cloneProjection(current ?? emptyProjection(notification.turnId));
  if (projection.turnId !== notification.turnId) {
    throw new Error("conversation event projection owner mismatch");
  }

  const { batch } = notification;
  if (batch.events.length === 0 && batch.hasMore) {
    throw new Error("empty conversation event batch cannot report more events");
  }
  if (batch.events.length > MAX_BATCH_SIZE || (batch.hasMore && batch.events.length !== MAX_BATCH_SIZE)) {
    throw new Error("conversation event batch exceeded the supported limit");
  }

  // Main-process watches always reconnect from zero. Accept that replay only
  // when every overlapping event is byte-for-byte identical to retained history.
  let deliveryCursor = projection.deliveryCursor;
  if (batch.events[0]?.sequence === 1 && deliveryCursor !== 0) deliveryCursor = 0;

  const addedEvents: DaemonConversationTurnEvent[] = [];
  let reachedTerminal = false;
  for (const rawEvent of batch.events) {
    const event = validateEventShape(rawEvent, notification.turnId);
    if (event.sequence !== deliveryCursor + 1) {
      throw new Error("conversation event delivery sequence is not contiguous");
    }
    deliveryCursor = event.sequence;

    const retained = projection.events[event.sequence - 1];
    if (retained) {
      if (!sameEvent(retained, event)) {
        throw new Error("conversation event replay changed retained history");
      }
      // A terminal listener can validate and retain this edge, then fail while
      // loading the canonical snapshot. Main deliberately withholds the ACK in
      // that case and replays from zero. Surface the identical terminal edge
      // again so the canonical reconciliation is retried instead of silently
      // acknowledging a projection that was never installed.
      if (event.kind === "state_changed" && TERMINAL_STATES.has(event.toState)) {
        reachedTerminal = true;
      }
      continue;
    }
    if (event.sequence !== projection.lastSequence + 1) {
      throw new Error("conversation event projection sequence is not contiguous");
    }
    applyNewEvent(projection, event);
    projection.events.push(event);
    projection.lastSequence = event.sequence;
    addedEvents.push(event);
    if (event.kind === "state_changed" && TERMINAL_STATES.has(event.toState)) {
      reachedTerminal = true;
    }
  }

  validateSafeInteger(batch.nextSequence, "conversation event cursor", 0);
  if (batch.nextSequence !== deliveryCursor) {
    throw new Error("conversation event batch cursor does not match its events");
  }
  projection.deliveryCursor = deliveryCursor;
  return { projection, addedEvents, reachedTerminal };
}

export function isTerminalConversationState(state: DaemonConversationTurnState): boolean {
  return TERMINAL_STATES.has(state);
}

function emptyProjection(turnId: string): ConversationEventProjection {
  validateIdentifier(turnId, "conversation event turn id");
  return {
    turnId,
    revision: 0,
    text: "",
    textUtf8Bytes: 0,
    textEventCount: 0,
    lastSequence: 0,
    deliveryCursor: 0,
    events: [],
  };
}

function cloneProjection(value: ConversationEventProjection): ConversationEventProjection {
  return {
    ...value,
    events: value.events.map(cloneEvent),
  };
}

function validateNotificationShape(notification: DesktopConversationTurnEventNotification): void {
  if (!isRecord(notification) || !hasExactKeys(notification, ["batch", "turnId"])) {
    throw new Error("invalid conversation event notification");
  }
  validateIdentifier(notification.turnId, "conversation event notification turn id");
  if (!isRecord(notification.batch) || !hasExactKeys(notification.batch, ["events", "hasMore", "nextSequence"])) {
    throw new Error("invalid conversation event batch");
  }
  if (!Array.isArray(notification.batch.events) || typeof notification.batch.hasMore !== "boolean") {
    throw new Error("invalid conversation event batch fields");
  }
}

function validateEventShape(
  rawEvent: DaemonConversationTurnEvent,
  expectedTurnId: string,
): DaemonConversationTurnEvent {
  if (!isRecord(rawEvent)) throw new Error("invalid conversation event");
  validateSafeInteger(rawEvent.sequence, "conversation event sequence", 1);
  validateIdentifier(rawEvent.turnId, "conversation event turn id");
  if (rawEvent.turnId !== expectedTurnId) throw new Error("conversation event owner mismatch");

  if (rawEvent.kind === "created") {
    if (!hasExactKeys(rawEvent, ["kind", "sequence", "turnId"])) {
      throw new Error("invalid created conversation event fields");
    }
    return { sequence: rawEvent.sequence, turnId: rawEvent.turnId, kind: "created" };
  }
  if (rawEvent.kind === "state_changed") {
    if (!hasExactKeys(rawEvent, ["fromState", "kind", "sequence", "toState", "turnId"])) {
      throw new Error("invalid state-change conversation event fields");
    }
    validateState(rawEvent.fromState);
    validateState(rawEvent.toState);
    return {
      sequence: rawEvent.sequence,
      turnId: rawEvent.turnId,
      kind: "state_changed",
      fromState: rawEvent.fromState,
      toState: rawEvent.toState,
    };
  }
  if (rawEvent.kind === "text_appended") {
    if (!hasExactKeys(rawEvent, ["kind", "sequence", "startUtf8Offset", "text", "turnId"])) {
      throw new Error("invalid text-append conversation event fields");
    }
    validateSafeInteger(rawEvent.startUtf8Offset, "conversation text offset", 0);
    if (typeof rawEvent.text !== "string") throw new Error("invalid conversation event text");
    return {
      sequence: rawEvent.sequence,
      turnId: rawEvent.turnId,
      kind: "text_appended",
      startUtf8Offset: rawEvent.startUtf8Offset,
      text: rawEvent.text,
    };
  }
  throw new Error("unknown conversation event kind");
}

function applyNewEvent(
  projection: ConversationEventProjection,
  event: DaemonConversationTurnEvent,
): void {
  if (projection.state && TERMINAL_STATES.has(projection.state)) {
    throw new Error("conversation event followed a terminal state");
  }
  if (event.kind === "created") {
    if (event.sequence !== 1 || projection.state !== undefined) {
      throw new Error("created conversation event is out of order");
    }
    projection.state = "reserved";
    projection.revision = 0;
    return;
  }
  if (event.kind === "state_changed") {
    if (projection.state !== event.fromState || !permitsTransition(event.fromState, event.toState)) {
      throw new Error("conversation state-change event is not a legal continuation");
    }
    projection.state = event.toState;
    projection.revision += 1;
    return;
  }

  const bytes = encoder.encode(event.text).byteLength;
  if (
    projection.state !== "provider_started"
    || event.startUtf8Offset !== projection.textUtf8Bytes
    || bytes < 1
    || bytes > MAX_TEXT_CHUNK_BYTES
    || containsUnsupportedControl(event.text)
    || containsUnpairedSurrogate(event.text)
    || projection.textEventCount >= MAX_TEXT_EVENTS
    || projection.textUtf8Bytes + bytes > MAX_TEXT_BYTES
  ) {
    throw new Error("conversation text-append event is invalid");
  }
  projection.text += event.text;
  projection.textUtf8Bytes += bytes;
  projection.textEventCount += 1;
}

function permitsTransition(from: DaemonConversationTurnState, to: DaemonConversationTurnState): boolean {
  return (from === "reserved" && (to === "provider_started" || to === "cancelled"))
    || (from === "provider_started" && (
      to === "completed" || to === "failed" || to === "interrupted_needs_review"
    ));
}

function sameEvent(left: DaemonConversationTurnEvent, right: DaemonConversationTurnEvent): boolean {
  if (left.sequence !== right.sequence || left.turnId !== right.turnId || left.kind !== right.kind) return false;
  if (left.kind === "created" && right.kind === "created") return true;
  if (left.kind === "state_changed" && right.kind === "state_changed") {
    return left.fromState === right.fromState && left.toState === right.toState;
  }
  return left.kind === "text_appended"
    && right.kind === "text_appended"
    && left.startUtf8Offset === right.startUtf8Offset
    && left.text === right.text;
}

function containsUnsupportedControl(value: string): boolean {
  return [...value].some((character) => {
    const codePoint = character.codePointAt(0) ?? 0;
    return (codePoint <= 0x1f && codePoint !== 0x09 && codePoint !== 0x0a && codePoint !== 0x0d)
      || (codePoint >= 0x7f && codePoint <= 0x9f);
  });
}

function containsUnpairedSurrogate(value: string): boolean {
  for (let index = 0; index < value.length; index += 1) {
    const codeUnit = value.charCodeAt(index);
    if (codeUnit >= 0xd800 && codeUnit <= 0xdbff) {
      const next = value.charCodeAt(index + 1);
      if (next < 0xdc00 || next > 0xdfff) return true;
      index += 1;
    } else if (codeUnit >= 0xdc00 && codeUnit <= 0xdfff) {
      return true;
    }
  }
  return false;
}

function cloneEvent(event: DaemonConversationTurnEvent): DaemonConversationTurnEvent {
  return { ...event };
}

function validateIdentifier(value: unknown, field: string): asserts value is string {
  if (typeof value !== "string" || !IDENTIFIER.test(value)) throw new Error(`invalid ${field}`);
}

function validateState(value: unknown): asserts value is DaemonConversationTurnState {
  if (typeof value !== "string" || !STATES.has(value as DaemonConversationTurnState)) {
    throw new Error("invalid conversation event state");
  }
}

function validateSafeInteger(value: unknown, field: string, minimum: number): asserts value is number {
  if (!Number.isSafeInteger(value) || (value as number) < minimum) throw new Error(`invalid ${field}`);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, expected: string[]): boolean {
  const actual = Object.keys(value).toSorted();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}
