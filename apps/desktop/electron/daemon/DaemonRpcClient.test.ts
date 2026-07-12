// @vitest-environment node
import { Duplex } from "node:stream";
import { describe, expect, it, vi } from "vitest";
import {
  ApprovalDecision,
  ApprovalRisk,
  ApprovalScope,
  ApprovalStatus,
  Artifact,
  ArtifactOpenFailureCode,
  ArtifactOpenReceiptStatus,
  ArtifactState,
  AutomationSchedulerHealth,
  ConversationForkDeliveryState,
  ConversationRetryEligibility,
  ConversationTurnEventKind,
  ConversationTurnOrigin,
  ConversationTurnState,
  Envelope,
  ErrorCode,
  MessageRole,
  MessageState,
  ProjectState,
  Request,
  RunEventKind,
  RunState,
  WorkspaceSearchKind,
  type ConversationTurnEvent,
  type HealthResponse,
  type Response,
} from "../generated/daemon/v1/daemon.js";
import {
  CHAT_MODEL_RPC_TIMEOUT_MS,
  CONVERSATION_START_RPC_TIMEOUT_MS,
  CREDENTIAL_ENROLLMENT_RPC_TIMEOUT_MS,
  DaemonConversationTurnEventSubscription,
  DaemonProtocolClient,
  DaemonProtocolError,
  DaemonRunEventSubscription,
  DaemonRpcClient,
  DaemonTransportError,
  MAX_FRAME_BYTES,
  PROTOCOL_VERSION,
} from "./DaemonRpcClient.js";

const nonce = Buffer.alloc(32, 7);

class FakeDuplex extends Duplex {
  private readonly writes: Buffer[] = [];
  private readonly writeWaiters: Array<(value: Buffer) => void> = [];

  override _read(): void {}

  override _write(chunk: Buffer, _encoding: BufferEncoding, callback: (error?: Error | null) => void): void {
    const value = Buffer.from(chunk);
    const waiter = this.writeWaiters.shift();
    if (waiter) waiter(value);
    else this.writes.push(value);
    callback();
  }

  nextWrite(): Promise<Buffer> {
    const value = this.writes.shift();
    if (value) return Promise.resolve(value);
    return new Promise((resolve) => this.writeWaiters.push(resolve));
  }

  receive(value: Uint8Array): void {
    this.push(Buffer.from(value));
  }
}

class StalledWriteDuplex extends Duplex {
  override _read(): void {}

  override _write(_chunk: Buffer, _encoding: BufferEncoding, _callback: (error?: Error | null) => void): void {}
}

describe("DaemonRpcClient", () => {
  it("writes and reads fragmented length-prefixed Protobuf frames", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const responsePromise = protocol.health();
    const request = decodeFrame(await stream.nextWrite());
    const response = healthFrame(request, { instanceId: "daemon-fragmented" });

    stream.receive(response.subarray(0, 2));
    stream.receive(response.subarray(2, 7));
    stream.receive(response.subarray(7));

    await expect(responsePromise).resolves.toMatchObject({ instanceId: "daemon-fragmented", protocolVersion: PROTOCOL_VERSION });
    protocol.close();
  });

  it.each(Array.from({ length: PROTOCOL_VERSION }, (_, version) => version))(
    "rejects a response from compatibility epoch %s",
    async (version) => {
      const stream = new FakeDuplex();
      const protocol = client(stream);
      const responsePromise = protocol.health();
      const request = decodeFrame(await stream.nextWrite());
      const response = responseEnvelope(request);
      response.protocolVersion = version;
      stream.receive(encodeFrame(response));

      await expect(responsePromise).rejects.toThrow(`unsupported daemon protocol version ${version}`);
      expect(stream.destroyed).toBe(true);
    },
  );

  it("discards the reserved epoch-ten artifact mutation tags", () => {
    for (const legacyRequest of [
      Buffer.from([0xba, 0x01, 0x02, 0x0a, 0x00]),
      Buffer.from([0xc2, 0x01, 0x02, 0x0a, 0x00]),
      Buffer.from([0xca, 0x01, 0x02, 0x0a, 0x00]),
    ]) {
      const decoded = Request.decode(legacyRequest);
      expect(decoded.operation).toBeUndefined();
      expect(Request.encode(decoded).finish()).toHaveLength(0);
    }
  });

  it("discards epoch-eleven message mutations while preserving message reads", () => {
    for (const legacyRequest of [
      Buffer.from([0x92, 0x01, 0x02, 0x0a, 0x00]),
      Buffer.from([0x9a, 0x01, 0x02, 0x0a, 0x00]),
      Buffer.from([0xa2, 0x01, 0x02, 0x0a, 0x00]),
    ]) {
      const decoded = Request.decode(legacyRequest);
      expect(decoded.operation).toBeUndefined();
      expect(Request.encode(decoded).finish()).toHaveLength(0);
    }

    const getMessage = Request.encode({
      operation: { $case: "getMessage", value: { messageId: "message-1" } },
    }).finish();
    const listMessages = Request.encode({
      operation: {
        $case: "listMessages",
        value: { threadId: "thread-1", cursor: "", limit: 20 },
      },
    }).finish();
    expect(Array.from(getMessage.subarray(0, 2))).toEqual([0xaa, 0x01]);
    expect(Array.from(listMessages.subarray(0, 2))).toEqual([0xb2, 0x01]);
    expect(Request.decode(getMessage).operation?.$case).toBe("getMessage");
    expect(Request.decode(listMessages).operation?.$case).toBe("listMessages");
  });

  it("discards the reserved legacy Artifact relative-path field", () => {
    const legacyRelativePath = Buffer.from("private/a");
    const decoded = Artifact.decode(Buffer.concat([
      Buffer.from([0x2a, legacyRelativePath.length]),
      legacyRelativePath,
    ]));

    expect(decoded).not.toHaveProperty("relativePath");
    expect(Artifact.encode(decoded).finish()).toHaveLength(0);
  });

  it("parses multiple valid frames delivered in one stream chunk", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const first = protocol.health();
    const second = protocol.health();
    const firstRequest = decodeFrame(await stream.nextWrite());
    const secondRequest = decodeFrame(await stream.nextWrite());

    stream.receive(Buffer.concat([
      healthFrame(firstRequest, { instanceId: "daemon-first" }),
      healthFrame(secondRequest, { instanceId: "daemon-second" }),
    ]));

    await expect(first).resolves.toMatchObject({ instanceId: "daemon-first" });
    await expect(second).resolves.toMatchObject({ instanceId: "daemon-second" });
    protocol.close();
  });

  it("rejects an oversized frame before allocating its payload", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const responsePromise = protocol.health();
    await stream.nextWrite();
    const prefix = Buffer.alloc(4);
    prefix.writeUInt32BE(MAX_FRAME_BYTES + 1);
    stream.receive(prefix);

    await expect(responsePromise).rejects.toThrow(`invalid daemon frame length ${MAX_FRAME_BYTES + 1}`);
    expect(stream.destroyed).toBe(true);
  });

  it("closes the connection when a response has an invalid nonce", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const responsePromise = protocol.health();
    const request = decodeFrame(await stream.nextWrite());
    const response = responseEnvelope(request, { instanceId: "wrong-nonce" });
    response.startupNonce = Buffer.alloc(32, 9);
    stream.receive(encodeFrame(response));

    await expect(responsePromise).rejects.toBeInstanceOf(DaemonProtocolError);
    expect(stream.destroyed).toBe(true);
  });

  it("rejects the reserved unsolicited Envelope.Event payload in epoch four", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const responsePromise = protocol.health();
    const request = decodeFrame(await stream.nextWrite());
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: "",
      payload: { $case: "event", value: { runEvent: undefined } },
    }));

    await expect(responsePromise).rejects.toThrow("daemon response payload is missing");
    expect(stream.destroyed).toBe(true);
  });

  it("reconnects the next request after a stream closes", async () => {
    const first = new FakeDuplex();
    const second = new FakeDuplex();
    const connect = vi.fn()
      .mockResolvedValueOnce(first)
      .mockResolvedValueOnce(second);
    const rpc = new DaemonRpcClient({ nonce, connect, requestTimeoutMs: 1_000 });
    const protocol = new DaemonProtocolClient(rpc);

    const firstHealth = protocol.health();
    const firstRequest = decodeFrame(await first.nextWrite());
    first.receive(healthFrame(firstRequest, { instanceId: "daemon-1" }));
    await expect(firstHealth).resolves.toMatchObject({ instanceId: "daemon-1" });
    first.destroy();
    await new Promise((resolve) => setImmediate(resolve));

    const secondHealth = protocol.health();
    const secondRequest = decodeFrame(await second.nextWrite());
    second.receive(healthFrame(secondRequest, { instanceId: "daemon-2" }));
    await expect(secondHealth).resolves.toMatchObject({ instanceId: "daemon-2" });
    expect(connect).toHaveBeenCalledTimes(2);
    protocol.close();
  });

  it("resumes a dedicated event poll from the last delivered cursor after reconnect", async () => {
    const first = new FakeDuplex();
    const second = new FakeDuplex();
    const connect = vi.fn()
      .mockResolvedValueOnce(first)
      .mockResolvedValueOnce(second);
    const protocol = new DaemonProtocolClient(new DaemonRpcClient({
      nonce,
      connect,
      requestTimeoutMs: 1_000,
      responseGraceMs: 10,
      maxPendingRequests: 1,
    }));
    let delivered: ((batch: { nextSequence: bigint }) => void) | undefined;
    const delivery = new Promise<{ nextSequence: bigint }>((resolve) => {
      delivered = resolve;
    });
    const errors: Error[] = [];
    const subscription = new DaemonRunEventSubscription(protocol, {
      runId: "run-1",
      afterSequence: 0n,
      batchLimit: 2,
      waitTimeoutMs: 0,
      initialRetryMs: 1,
      listener: (batch) => delivered?.(batch),
      onError: (error) => errors.push(error),
    });
    const completed = subscription.start();

    const interrupted = decodeFrame(await first.nextWrite());
    expectPollCursor(interrupted, 0n);
    first.destroy();

    // The durable event may be committed while the client is disconnected.
    // The reconnect must ask from the same cursor, so it cannot miss it.
    const resumed = decodeFrame(await second.nextWrite());
    expectPollCursor(resumed, 0n);
    second.receive(runEventBatchFrame(resumed, [wireRunEvent(1n)], 1n, false));
    await expect(delivery).resolves.toMatchObject({ nextSequence: 1n });

    const next = decodeFrame(await second.nextWrite());
    expectPollCursor(next, 1n);
    expect(subscription.afterSequence).toBe(1n);
    expect(errors).toEqual([]);
    subscription.close();
    await completed;
    expect(second.destroyed).toBe(true);
    expect(connect).toHaveBeenCalledTimes(2);
  });

  it("fails closed on a discontinuous event sequence", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const polled = protocol.pollRunEvents("run-1", 0n, 2, 0);
    const request = decodeFrame(await stream.nextWrite());
    stream.receive(runEventBatchFrame(request, [wireRunEvent(2n)], 2n, false));

    await expect(polled).rejects.toThrow("repeated, retrograde, or discontinuous");
    expect(stream.destroyed).toBe(true);
  });

  it("rejects a run event cursor outside the durable signed sequence range before connecting", async () => {
    const connect = vi.fn();
    const protocol = new DaemonProtocolClient(new DaemonRpcClient({ nonce, connect }));

    await expect(protocol.pollRunEvents("run-1", 1n << 63n, 1, 0))
      .rejects.toThrow("invalid run event cursor");
    expect(connect).not.toHaveBeenCalled();
    protocol.close();
  });

  it("does not advance a subscription cursor when listener delivery fails", async () => {
    const stream = new FakeDuplex();
    const errors: Error[] = [];
    const protocol = client(stream);
    const subscription = new DaemonRunEventSubscription(protocol, {
      runId: "run-1",
      afterSequence: 0n,
      waitTimeoutMs: 0,
      listener: () => {
        throw new Error("listener rejected batch");
      },
      onError: (error) => errors.push(error),
    });
    const completed = subscription.start();
    const request = decodeFrame(await stream.nextWrite());
    stream.receive(runEventBatchFrame(request, [wireRunEvent(1n)], 1n, false));
    await completed;

    expect(subscription.afterSequence).toBe(0n);
    expect(errors).toHaveLength(1);
    expect(errors[0]?.message).toBe("listener rejected batch");
    expect(stream.destroyed).toBe(true);
  });

  it("bounds listener delivery time without advancing the cursor", async () => {
    const stream = new FakeDuplex();
    const errors: Error[] = [];
    const protocol = client(stream);
    const subscription = new DaemonRunEventSubscription(protocol, {
      runId: "run-1",
      afterSequence: 0n,
      waitTimeoutMs: 0,
      listenerTimeoutMs: 5,
      listener: () => new Promise<void>(() => undefined),
      onError: (error) => errors.push(error),
    });
    const completed = subscription.start();
    const request = decodeFrame(await stream.nextWrite());
    stream.receive(runEventBatchFrame(request, [wireRunEvent(1n)], 1n, false));
    await completed;

    expect(subscription.afterSequence).toBe(0n);
    expect(errors[0]?.message).toBe("run event listener delivery timed out");
    expect(stream.destroyed).toBe(true);
  });

  it("replays and validates a complete durable conversation event stream", async () => {
    const stream = new FakeDuplex();
    const delivered: Array<{ nextSequence: bigint; text: string }> = [];
    const errors: Error[] = [];
    const subscription = new DaemonConversationTurnEventSubscription(client(stream), {
      turnId: "turn-1",
      waitTimeoutMs: 0,
      listener: (batch) => {
        delivered.push({
          nextSequence: batch.nextSequence,
          text: batch.events.map((event) => event.textAppended).join(""),
        });
      },
      onError: (error) => errors.push(error),
    });
    const completed = subscription.start();
    const request = decodeFrame(await stream.nextWrite());
    expect(request.payload?.$case === "request" && request.payload.value.operation).toMatchObject({
      $case: "pollConversationTurnEvents",
      value: { turnId: "turn-1", afterSequence: 0n, limit: 100, waitTimeoutMs: 0 },
    });
    const events = [
      wireConversationEvent(1n, ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_CREATED),
      wireConversationEvent(
        2n,
        ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_STATE_CHANGED,
        {
          fromState: ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED,
          toState: ConversationTurnState.CONVERSATION_TURN_STATE_PROVIDER_STARTED,
        },
      ),
      wireConversationEvent(
        3n,
        ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_TEXT_APPENDED,
        { textAppended: "Hi", startUtf8Offset: 0n },
      ),
      wireConversationEvent(
        4n,
        ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_TEXT_APPENDED,
        { textAppended: "!", startUtf8Offset: 2n },
      ),
      wireConversationEvent(
        5n,
        ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_STATE_CHANGED,
        {
          fromState: ConversationTurnState.CONVERSATION_TURN_STATE_PROVIDER_STARTED,
          toState: ConversationTurnState.CONVERSATION_TURN_STATE_COMPLETED,
        },
      ),
    ];
    stream.receive(conversationEventBatchFrame(request, events, 5n, false));
    await completed;

    expect(subscription.afterSequence).toBe(5n);
    expect(delivered).toEqual([{ nextSequence: 5n, text: "Hi!" }]);
    expect(errors).toEqual([]);
    expect(stream.destroyed).toBe(true);
  });

  it("rejects a durable conversation stream with more than 4,097 text events", async () => {
    const stream = new FakeDuplex();
    const errors: Error[] = [];
    let deliveredBatches = 0;
    const subscription = new DaemonConversationTurnEventSubscription(client(stream), {
      turnId: "turn-1",
      waitTimeoutMs: 0,
      listener: () => {
        deliveredBatches += 1;
      },
      onError: (error) => errors.push(error),
    });
    const completed = subscription.start();

    for (let batchIndex = 0; batchIndex < 41; batchIndex += 1) {
      const request = decodeFrame(await stream.nextWrite());
      const firstSequence = BigInt(batchIndex * 100 + 1);
      const events = Array.from({ length: 100 }, (_value, index) => {
        const sequence = firstSequence + BigInt(index);
        if (sequence === 1n) {
          return wireConversationEvent(
            sequence,
            ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_CREATED,
          );
        }
        if (sequence === 2n) {
          return wireConversationEvent(
            sequence,
            ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_STATE_CHANGED,
            {
              fromState: ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED,
              toState: ConversationTurnState.CONVERSATION_TURN_STATE_PROVIDER_STARTED,
            },
          );
        }
        return wireConversationEvent(
          sequence,
          ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_TEXT_APPENDED,
          { textAppended: "x", startUtf8Offset: sequence - 3n },
        );
      });
      const lastSequence = firstSequence + 99n;
      stream.receive(conversationEventBatchFrame(request, events, lastSequence, batchIndex < 40));
    }
    await completed;

    expect(subscription.afterSequence).toBe(4_000n);
    expect(deliveredBatches).toBe(40);
    expect(errors).toHaveLength(1);
    expect(errors[0]?.message).toBe("daemon conversation event count exceeded the limit");
    expect(stream.destroyed).toBe(true);
  });

  it("ignores one valid late response after timeout without disrupting another request", async () => {
    const stream = new FakeDuplex();
    const rpc = new DaemonRpcClient({
      nonce,
      connect: async () => stream,
      requestTimeoutMs: 15,
      responseGraceMs: 1,
    });
    const protocol = new DaemonProtocolClient(rpc);

    const timedOut = protocol.health();
    const firstRequest = decodeFrame(await stream.nextWrite());
    await expect(timedOut).rejects.toThrow("timed out");

    const healthy = protocol.health();
    const secondRequest = decodeFrame(await stream.nextWrite());
    stream.receive(healthFrame(firstRequest, { instanceId: "late-daemon" }));
    stream.receive(healthFrame(secondRequest, { instanceId: "current-daemon" }));

    await expect(healthy).resolves.toMatchObject({ instanceId: "current-daemon" });
    expect(stream.destroyed).toBe(false);
    protocol.close();
  });

  it("returns the bounded timeout when the stream write callback stalls", async () => {
    const stream = new StalledWriteDuplex();
    const rpc = new DaemonRpcClient({
      nonce,
      connect: async () => stream,
      requestTimeoutMs: 10,
      responseGraceMs: 1,
    });
    const protocol = new DaemonProtocolClient(rpc);

    await expect(protocol.health()).rejects.toThrow("timed out");
    expect(stream.destroyed).toBe(true);
    protocol.close();
  });

  it("accepts a daemon deadline response during the generic response grace", async () => {
    const stream = new FakeDuplex();
    const rpc = new DaemonRpcClient({
      nonce,
      connect: async () => stream,
      requestTimeoutMs: 15,
      responseGraceMs: 30,
    });
    const protocol = new DaemonProtocolClient(rpc);
    const created = protocol.createProject("Late", "Deadline response", "late-project");
    const request = decodeFrame(await stream.nextWrite());

    await new Promise((resolve) => setTimeout(resolve, 20));
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: request.idempotencyKey,
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "error",
            value: {
              code: ErrorCode.ERROR_CODE_DEADLINE_EXCEEDED,
              message: "daemon request deadline was exceeded",
              retryable: true,
            },
          },
        },
      },
    }));

    await expect(created).rejects.toMatchObject({
      code: ErrorCode.ERROR_CODE_DEADLINE_EXCEEDED,
      retryable: true,
    });
    const healthy = protocol.health();
    const healthRequest = decodeFrame(await stream.nextWrite());
    stream.receive(healthFrame(healthRequest, { instanceId: "daemon-after-generic-deadline" }));
    await expect(healthy).resolves.toMatchObject({ instanceId: "daemon-after-generic-deadline" });
    expect(stream.destroyed).toBe(false);
    protocol.close();
  });

  it("carries workspace idempotency keys and returns the typed result", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const projectPromise = protocol.createProject("Launch", "Release work", "project-command-1");
    const request = decodeFrame(await stream.nextWrite());
    expect(request.idempotencyKey).toBe("project-command-1");
    expect(request.payload?.$case === "request" && request.payload.value.operation).toMatchObject({
      $case: "createProject",
      value: { name: "Launch", description: "Release work" },
    });
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: request.idempotencyKey,
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "project",
            value: {
              id: "project-1",
              name: "Launch",
              description: "Release work",
              state: ProjectState.PROJECT_STATE_ACTIVE,
              revision: 0n,
              createdAtUnixMs: 1n,
              updatedAtUnixMs: 1n,
            },
          },
        },
      },
    }));

    await expect(projectPromise).resolves.toMatchObject({ id: "project-1", name: "Launch" });
    protocol.close();
  });

  it("keeps artifact import, exact-version open, and removal on their closed operations", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const sourcePath = "/home/person/private/source-canary.pdf";
    const importing = protocol.importArtifact(
      "project-1",
      "source-canary.pdf",
      "application/pdf",
      sourcePath,
      "artifact-import-1",
    );
    const importRequest = decodeFrame(await stream.nextWrite());
    const importPayload = importRequest.payload?.$case === "request"
      ? importRequest.payload.value
      : undefined;
    expect(importRequest.idempotencyKey).toBe("artifact-import-1");
    expect(importPayload?.operation).toEqual({
      $case: "importArtifact",
      value: {
        projectId: "project-1",
        threadId: undefined,
        displayName: "source-canary.pdf",
        mediaType: "application/pdf",
        sourcePath,
      },
    });
    expect(Array.from(Request.encode(importPayload!).finish().subarray(0, 2)))
      .toEqual([0xba, 0x03]);
    stream.receive(responseResultFrame(importRequest, {
      $case: "artifactOperation",
      value: {
        result: {
          $case: "importedArtifact",
          value: {
            id: "artifact-1",
            projectId: "project-1",
            threadId: "",
            name: "source-canary.pdf",
            mediaType: "application/pdf",
            byteSize: 42n,
            state: ArtifactState.ARTIFACT_STATE_AVAILABLE,
            revision: 1n,
            createdAtUnixMs: 1n,
            updatedAtUnixMs: 2n,
            contentVersion: 1,
          },
        },
      },
    }));
    const importResult = await importing;
    expect(importResult.result?.$case).toBe("importedArtifact");
    expect(importResult).not.toHaveProperty("sourcePath");
    expect(importResult.result?.value).not.toHaveProperty("sourcePath");

    const opening = protocol.openArtifact("artifact-1", 1, "artifact-open-1");
    const openRequest = decodeFrame(await stream.nextWrite());
    const openPayload = openRequest.payload?.$case === "request" ? openRequest.payload.value : undefined;
    expect(openRequest.idempotencyKey).toBe("artifact-open-1");
    expect(openPayload?.operation).toEqual({
      $case: "openArtifact",
      value: { artifactId: "artifact-1", contentVersion: 1 },
    });
    expect(Array.from(Request.encode(openPayload!).finish().subarray(0, 2)))
      .toEqual([0xc2, 0x03]);
    stream.receive(responseResultFrame(openRequest, {
      $case: "artifactOperation",
      value: {
        result: {
          $case: "openReceipt",
          value: {
            artifactId: "artifact-1",
            contentVersion: 1,
            status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_FAILED,
            failureCode: ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_INTEGRITY_FAILURE,
          },
        },
      },
    }));
    await expect(opening).resolves.toMatchObject({
      result: {
        $case: "openReceipt",
        value: {
          artifactId: "artifact-1",
          contentVersion: 1,
          status: ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_FAILED,
          failureCode: ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_INTEGRITY_FAILURE,
        },
      },
    });

    const removing = protocol.removeArtifact("artifact-1", 1n, 1, "artifact-remove-1");
    const removeRequest = decodeFrame(await stream.nextWrite());
    const removePayload = removeRequest.payload?.$case === "request" ? removeRequest.payload.value : undefined;
    expect(removeRequest.idempotencyKey).toBe("artifact-remove-1");
    expect(removePayload?.operation).toEqual({
      $case: "removeArtifact",
      value: { artifactId: "artifact-1", expectedRevision: 1n, expectedContentVersion: 1 },
    });
    expect(Array.from(Request.encode(removePayload!).finish().subarray(0, 2)))
      .toEqual([0xca, 0x03]);
    stream.receive(responseResultFrame(removeRequest, {
      $case: "artifactOperation",
      value: {
        result: {
          $case: "removedArtifact",
          value: {
            id: "artifact-1",
            projectId: "project-1",
            threadId: "",
            name: "source-canary.pdf",
            mediaType: "",
            byteSize: 0n,
            state: ArtifactState.ARTIFACT_STATE_DELETED,
            revision: 2n,
            createdAtUnixMs: 1n,
            updatedAtUnixMs: 3n,
            contentVersion: undefined,
          },
        },
      },
    }));
    await expect(removing).resolves.toMatchObject({
      result: { $case: "removedArtifact", value: { id: "artifact-1", revision: 2n } },
    });

    const removalPending = protocol.removeArtifact("artifact-2", 2n, 2, "artifact-remove-pending");
    const pendingRequest = decodeFrame(await stream.nextWrite());
    stream.receive(responseResultFrame(pendingRequest, {
      $case: "artifactOperation",
      value: {
        result: {
          $case: "removalPending",
          value: {
            artifactId: "artifact-2",
            expectedRevision: 2n,
            expectedContentVersion: 2,
            tombstone: {
              id: "artifact-2",
              projectId: "project-1",
              threadId: "",
              name: "pending.pdf",
              mediaType: "",
              byteSize: 0n,
              state: ArtifactState.ARTIFACT_STATE_DELETED,
              revision: 3n,
              createdAtUnixMs: 1n,
              updatedAtUnixMs: 3n,
              contentVersion: undefined,
            },
          },
        },
      },
    }));
    await expect(removalPending).resolves.toMatchObject({
      result: {
        $case: "removalPending",
        value: {
          artifactId: "artifact-2",
          expectedRevision: 2n,
          expectedContentVersion: 2,
          tombstone: { id: "artifact-2", revision: 3n },
        },
      },
    });
    await expect(protocol.removeArtifact("artifact-1", 2n, 1, "artifact-remove-mismatch"))
      .rejects.toThrow("artifact revision does not match its content version");
    protocol.close();
  });

  it("sends bounded workspace search as a read-only typed operation", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const search = protocol.searchWorkspace("project-1", "release evidence", 0, 8);
    const request = decodeFrame(await stream.nextWrite());
    expect(request.idempotencyKey).toBe("");
    expect(request.payload?.$case === "request" && request.payload.value.operation).toMatchObject({
      $case: "searchWorkspace",
      value: { projectId: "project-1", query: "release evidence", offset: 0, limit: 8 },
    });
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: "",
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "searchResults",
            value: {
              hits: [{
                id: "message-1",
                projectId: "project-1",
                threadId: "thread-1",
                kind: WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_MESSAGE,
                title: "Release review",
                snippet: "Evidence",
                updatedAtUnixMs: 10n,
              }],
              nextOffset: 0,
              hasMore: false,
            },
          },
        },
      },
    }));

    await expect(search).resolves.toMatchObject({
      hits: [{ id: "message-1", threadId: "thread-1" }],
      hasMore: false,
    });
    protocol.close();
  });

  it("sends only an exact revisioned approval decision intent", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const decision = protocol.decideApproval(
      "approval-1",
      2n,
      ApprovalDecision.APPROVAL_DECISION_GRANT,
      "decision-key",
    );
    const request = decodeFrame(await stream.nextWrite());
    expect(request.idempotencyKey).toBe("decision-key");
    expect(request.payload?.$case === "request" && request.payload.value.operation).toEqual({
      $case: "decideApproval",
      value: {
        approvalId: "approval-1",
        expectedRevision: 2n,
        decision: ApprovalDecision.APPROVAL_DECISION_GRANT,
      },
    });
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: "decision-key",
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "approval",
            value: {
              id: "approval-1",
              runId: "run-1",
              action: {
                action: "publish_release",
                target: "Release notes",
                dataSummary: "Publishes the reviewed release notes.",
                risk: ApprovalRisk.APPROVAL_RISK_HIGH,
              },
              scope: ApprovalScope.APPROVAL_SCOPE_ONCE,
              resourceId: "",
              status: ApprovalStatus.APPROVAL_STATUS_GRANTED,
              revision: 3n,
              createdAtUnixMs: 1n,
              expiresAtUnixMs: 10_000n,
              decidedAtUnixMs: 2n,
            },
          },
        },
      },
    }));

    await expect(decision).resolves.toMatchObject({
      id: "approval-1",
      revision: 3n,
      status: ApprovalStatus.APPROVAL_STATUS_GRANTED,
    });
    protocol.close();
  });

  it("does not send caller-authored capability facts", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const startedAt = Date.now();
    const capabilitiesPromise = protocol.resolveCapabilities();
    const request = decodeFrame(await stream.nextWrite());
    expect(CHAT_MODEL_RPC_TIMEOUT_MS).toBeGreaterThan(15_000);
    expect(Number(request.deadlineUnixMs) - startedAt)
      .toBeGreaterThanOrEqual(CHAT_MODEL_RPC_TIMEOUT_MS - 1_000);
    expect(request.payload?.$case === "request" && request.payload.value.operation).toMatchObject({
      $case: "resolveCapabilities",
      value: { facts: undefined },
    });
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: "",
      payload: {
        $case: "response",
        value: { result: { $case: "capabilities", value: { statuses: [] } } },
      },
    }));

    await expect(capabilitiesPromise).resolves.toEqual([]);
    protocol.close();
  });

  it("uses typed bounded operations for live model discovery and canonical selection", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const catalogStartedAt = Date.now();
    const catalogPromise = protocol.getChatModelCatalog();
    const catalogRequest = decodeFrame(await stream.nextWrite());
    expect(Number(catalogRequest.deadlineUnixMs) - catalogStartedAt)
      .toBeGreaterThanOrEqual(CHAT_MODEL_RPC_TIMEOUT_MS - 1_000);
    expect(catalogRequest.payload?.$case === "request" && catalogRequest.payload.value.operation).toEqual({
      $case: "getChatModelCatalog",
      value: {},
    });
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: catalogRequest.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: "",
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "chatModelCatalog",
            value: {
              models: [{
                id: "grok-alternative",
                aliases: ["grok-current"],
                inputModalities: ["text"],
                outputModalities: ["text"],
                textConversationReady: true,
              }],
              preference: { selectedModelId: "grok-4.3", revision: 0n, updatedAtUnixMs: 0n },
              defaultModelId: "grok-4.3",
              selectedModelReady: false,
              defaultModelReady: false,
            },
          },
        },
      },
    }));
    await expect(catalogPromise).resolves.toMatchObject({
      selectedModelReady: false,
      models: [{ id: "grok-alternative" }],
    });

    const selectionStartedAt = Date.now();
    const selectedPromise = protocol.selectChatModel(0n, "grok-current", "model-command-1");
    const selectedRequest = decodeFrame(await stream.nextWrite());
    expect(Number(selectedRequest.deadlineUnixMs) - selectionStartedAt)
      .toBeGreaterThanOrEqual(CHAT_MODEL_RPC_TIMEOUT_MS - 1_000);
    expect(selectedRequest.idempotencyKey).toBe("model-command-1");
    expect(selectedRequest.payload?.$case === "request" && selectedRequest.payload.value.operation).toEqual({
      $case: "selectChatModel",
      value: { expectedRevision: 0n, modelId: "grok-current" },
    });
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: selectedRequest.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: selectedRequest.idempotencyKey,
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "chatModelPreference",
            value: {
              selectedModelId: "grok-alternative",
              revision: 1n,
              updatedAtUnixMs: 2n,
            },
          },
        },
      },
    }));
    await expect(selectedPromise).resolves.toMatchObject({
      selectedModelId: "grok-alternative",
      revision: 1n,
    });
    protocol.close();
  });

  it("returns only non-secret state after a credential mutation", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const startedAt = Date.now();
    const enrolled = protocol.enrollXaiApiKey(0x1234n, "credential-command-1");
    const request = decodeFrame(await stream.nextWrite());
    expect(CREDENTIAL_ENROLLMENT_RPC_TIMEOUT_MS).toBe(125_000);
    expect(Number(request.deadlineUnixMs) - startedAt)
      .toBeGreaterThanOrEqual(CREDENTIAL_ENROLLMENT_RPC_TIMEOUT_MS - 1_000);
    expect(request.idempotencyKey).toBe("credential-command-1");
    expect(request.payload?.$case === "request" && request.payload.value.operation).toMatchObject({
      $case: "enrollXaiApiKey",
      value: { parentWindowToken: 0x1234n },
    });
    expect(request.payload?.$case === "request" && request.payload.value.operation?.value).not.toHaveProperty("apiKey");
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: request.idempotencyKey,
      payload: {
        $case: "response",
        value: { result: { $case: "accountState", value: { xaiApiKeyConfigured: true, xaiCapabilitiesResolved: true, grokBuildAuthenticated: false } } },
      },
    }));

    await expect(enrolled).resolves.toEqual({ xaiApiKeyConfigured: true, xaiCapabilitiesResolved: true, grokBuildAuthenticated: false });
    expect(await enrolled).not.toHaveProperty("apiKey");
    protocol.close();
  });

  it("sends revisioned desktop preference updates with an idempotency key", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const updated = protocol.updateDesktopPreferences(2n, false, "desktop-preference-1");
    const request = decodeFrame(await stream.nextWrite());
    expect(request.idempotencyKey).toBe("desktop-preference-1");
    expect(request.payload?.$case === "request" && request.payload.value.operation).toMatchObject({
      $case: "updateDesktopPreferences",
      value: { expectedRevision: 2n, keepRunningInNotificationArea: false },
    });
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: request.idempotencyKey,
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "desktopPreferences",
            value: {
              keepRunningInNotificationArea: false,
              revision: 3n,
              updatedAtUnixMs: 10n,
            },
          },
        },
      },
    }));

    await expect(updated).resolves.toEqual({
      keepRunningInNotificationArea: false,
      revision: 3n,
      updatedAtUnixMs: 10n,
    });
    protocol.close();
  });

  it("starts a durable turn without waiting for provider completion", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const startedAt = Date.now();
    const started = protocol.startConversationTurn(
      "thread-1",
      "Ask Grok",
      "turn-command-1",
      "grok-alternative",
    );
    const request = decodeFrame(await stream.nextWrite());
    expect(Number(request.deadlineUnixMs) - startedAt).toBeGreaterThanOrEqual(19_000);
    expect(request.idempotencyKey).toBe("turn-command-1");
    expect(request.payload?.$case === "request" && request.payload.value.operation).toMatchObject({
      $case: "startConversationTurn",
      value: { threadId: "thread-1", content: "Ask Grok", modelId: "grok-alternative" },
    });
    const userMessage = wireMessage("message-user", MessageRole.MESSAGE_ROLE_USER, "Ask Grok", 1n);
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: request.idempotencyKey,
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "conversationTurn",
            value: {
              turnId: "turn-1",
              state: ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED,
              revision: 0n,
              modelId: "grok-4.3",
              userMessage,
              assistantMessage: undefined,
              run: {
                id: "run-1",
                projectId: "project-1",
                threadId: "thread-1",
                state: RunState.RUN_STATE_QUEUED,
                revision: 0n,
                createdAtUnixMs: 1n,
                updatedAtUnixMs: 1n,
              },
              failure: undefined,
              citations: [],
              usage: { inputTokens: 0n, outputTokens: 0n, costInUsdTicks: 0n },
              zeroDataRetention: undefined,
              lineage: {
                origin: ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_ORIGINAL,
                sourceTurnId: "",
                retryDepth: 0,
              },
              retryEligibility:
                ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_IN_PROGRESS,
            },
          },
        },
      },
    }));

    await expect(started).resolves.toMatchObject({
      turnId: "turn-1",
      state: ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED,
      assistantMessage: undefined,
    });
    protocol.close();
  });

  it("retries by source identity without renderer-owned provider input", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const startedAt = Date.now();
    const retried = protocol.retryConversationTurn("turn-source", 2n, "retry-command-1");
    const request = decodeFrame(await stream.nextWrite());
    expect(Number(request.deadlineUnixMs) - startedAt).toBeGreaterThanOrEqual(19_000);
    expect(request.idempotencyKey).toBe("retry-command-1");
    expect(request.payload?.$case === "request" && request.payload.value.operation).toEqual({
      $case: "retryConversationTurn",
      value: { sourceTurnId: "turn-source", expectedRevision: 2n },
    });
    const userMessage = wireMessage("message-retry", MessageRole.MESSAGE_ROLE_USER, "Ask Grok", 3n);
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: request.idempotencyKey,
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "conversationTurn",
            value: {
              turnId: "turn-retry",
              state: ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED,
              revision: 0n,
              modelId: "grok-4.3",
              userMessage,
              assistantMessage: undefined,
              run: {
                id: "run-retry",
                projectId: "project-1",
                threadId: "thread-1",
                state: RunState.RUN_STATE_QUEUED,
                revision: 0n,
                createdAtUnixMs: 3n,
                updatedAtUnixMs: 3n,
              },
              failure: undefined,
              citations: [],
              usage: { inputTokens: 0n, outputTokens: 0n, costInUsdTicks: 0n },
              zeroDataRetention: undefined,
              lineage: {
                origin: ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_RETRY,
                sourceTurnId: "turn-source",
                retryDepth: 1,
              },
              retryEligibility:
                ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_IN_PROGRESS,
            },
          },
        },
      },
    }));

    await expect(retried).resolves.toMatchObject({
      turnId: "turn-retry",
      lineage: {
        origin: ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_RETRY,
        sourceTurnId: "turn-source",
        retryDepth: 1,
      },
    });
    protocol.close();
  });

  it("keeps fork operations on their exact tags, budgets, and idempotency boundaries", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const cases = [
      {
        invoke: () => protocol.branchConversationThread("turn-source", 7n, "branch-command-1"),
        operation: "branchConversationThread",
        value: { sourceTurnId: "turn-source", expectedRevision: 7n },
        tag: [0x92, 0x03],
        idempotencyKey: "branch-command-1",
        expectedTimeoutMs: 1_000,
        response: {
          $case: "conversationFork",
          value: {
            childThread: undefined,
            startedTurn: undefined,
            delivery: {
              childThreadId: "thread-child",
              state: ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_PENDING,
              revision: 0n,
            },
          },
        } satisfies NonNullable<Response["result"]>,
      },
      {
        invoke: () => protocol.editAndBranchConversationTurn(
          "turn-source",
          7n,
          "Edited question",
          "edit-branch-command-1",
        ),
        operation: "editAndBranchConversationTurn",
        value: {
          sourceTurnId: "turn-source",
          expectedRevision: 7n,
          content: "Edited question",
        },
        tag: [0x9a, 0x03],
        idempotencyKey: "edit-branch-command-1",
        expectedTimeoutMs: CONVERSATION_START_RPC_TIMEOUT_MS,
        response: {
          $case: "conversationFork",
          value: {
            childThread: undefined,
            startedTurn: undefined,
            delivery: {
              childThreadId: "thread-child",
              state: ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_PENDING,
              revision: 0n,
            },
          },
        } satisfies NonNullable<Response["result"]>,
      },
      {
        invoke: () => protocol.regenerateConversationTurn(
          "turn-source",
          7n,
          "regenerate-command-1",
        ),
        operation: "regenerateConversationTurn",
        value: { sourceTurnId: "turn-source", expectedRevision: 7n },
        tag: [0xa2, 0x03],
        idempotencyKey: "regenerate-command-1",
        expectedTimeoutMs: CONVERSATION_START_RPC_TIMEOUT_MS,
        response: {
          $case: "conversationFork",
          value: {
            childThread: undefined,
            startedTurn: undefined,
            delivery: {
              childThreadId: "thread-child",
              state: ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_PENDING,
              revision: 0n,
            },
          },
        } satisfies NonNullable<Response["result"]>,
      },
      {
        invoke: () => protocol.getConversationForkMetadata("thread-child"),
        operation: "getConversationForkMetadata",
        value: { threadId: "thread-child" },
        tag: [0xaa, 0x03],
        idempotencyKey: "",
        expectedTimeoutMs: 1_000,
        response: {
          $case: "conversationForkMetadata",
          value: { lineage: undefined, inheritedAssistantOutcomes: [], familyThreads: [] },
        } satisfies NonNullable<Response["result"]>,
      },
      {
        invoke: () => protocol.acknowledgeConversationForkDelivery(
          "thread-child",
          0n,
          "fork-delivery-ack-1",
        ),
        operation: "acknowledgeConversationForkDelivery",
        value: { childThreadId: "thread-child", expectedRevision: 0n },
        tag: [0xb2, 0x03],
        idempotencyKey: "fork-delivery-ack-1",
        expectedTimeoutMs: 1_000,
        response: {
          $case: "conversationForkDelivery",
          value: {
            childThreadId: "thread-child",
            state: ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_ACKNOWLEDGED,
            revision: 1n,
          },
        } satisfies NonNullable<Response["result"]>,
      },
    ] as const;

    for (const testCase of cases) {
      const startedAt = Date.now();
      const pending = testCase.invoke();
      const request = decodeFrame(await stream.nextWrite());
      const requestPayload = request.payload?.$case === "request" ? request.payload.value : undefined;
      expect(requestPayload?.operation).toEqual({
        $case: testCase.operation,
        value: testCase.value,
      });
      expect(Array.from(Request.encode(requestPayload!).finish().subarray(0, 2)))
        .toEqual(testCase.tag);
      expect(request.idempotencyKey).toBe(testCase.idempotencyKey);
      const timeoutBudgetMs = Number(request.deadlineUnixMs) - startedAt;
      expect(timeoutBudgetMs).toBeGreaterThanOrEqual(testCase.expectedTimeoutMs);
      expect(timeoutBudgetMs).toBeLessThanOrEqual(testCase.expectedTimeoutMs + 500);
      stream.receive(responseResultFrame(request, testCase.response));
      await expect(pending).resolves.toBeDefined();
    }
    protocol.close();
  });

  it("propagates an ambiguous transport close after a fork mutation was written", async () => {
    const stream = new FakeDuplex();
    const protocol = client(stream);
    const pending = protocol.regenerateConversationTurn(
      "turn-source",
      7n,
      "regenerate-ambiguous-1",
    );
    const request = decodeFrame(await stream.nextWrite());
    expect(request.idempotencyKey).toBe("regenerate-ambiguous-1");
    stream.destroy();

    await expect(pending).rejects.toBeInstanceOf(DaemonTransportError);
    await expect(pending).rejects.toThrow("daemon stream closed");
    protocol.close();
  });

  it("keeps credential deadline responses inside the operation budget and preserves the stream", async () => {
    const stream = new FakeDuplex();
    const rpc = new DaemonRpcClient({ nonce, connect: async () => stream, requestTimeoutMs: 15 });
    const protocol = new DaemonProtocolClient(rpc);
    const enrolled = protocol.enrollXaiApiKey(0x1234n, "credential-timeout-1");
    const request = decodeFrame(await stream.nextWrite());

    await new Promise((resolve) => setTimeout(resolve, 25));
    stream.receive(encodeFrame({
      protocolVersion: PROTOCOL_VERSION,
      requestId: request.requestId,
      startupNonce: nonce,
      deadlineUnixMs: 0n,
      idempotencyKey: request.idempotencyKey,
      payload: {
        $case: "response",
        value: {
          result: {
            $case: "error",
            value: {
              code: ErrorCode.ERROR_CODE_DEADLINE_EXCEEDED,
              message: "daemon request deadline was exceeded",
              retryable: true,
            },
          },
        },
      },
    }));

    await expect(enrolled).rejects.toMatchObject({
      code: ErrorCode.ERROR_CODE_DEADLINE_EXCEEDED,
      retryable: true,
    });
    const healthy = protocol.health();
    const healthRequest = decodeFrame(await stream.nextWrite());
    stream.receive(healthFrame(healthRequest, { instanceId: "daemon-after-credential-timeout" }));
    await expect(healthy).resolves.toMatchObject({ instanceId: "daemon-after-credential-timeout" });
    expect(stream.destroyed).toBe(false);
    protocol.close();
  });
});

function client(stream: FakeDuplex): DaemonProtocolClient {
  return new DaemonProtocolClient(new DaemonRpcClient({ nonce, connect: async () => stream, requestTimeoutMs: 1_000 }));
}

function decodeFrame(frame: Buffer): Envelope {
  const length = frame.readUInt32BE(0);
  expect(length).toBeGreaterThan(0);
  expect(length).toBeLessThanOrEqual(MAX_FRAME_BYTES);
  expect(frame.byteLength).toBe(length + 4);
  return Envelope.decode(frame.subarray(4));
}

function healthFrame(request: Envelope, overrides: Partial<HealthResponse> = {}): Buffer {
  return encodeFrame(responseEnvelope(request, overrides));
}

function responseResultFrame(
  request: Envelope,
  result: NonNullable<Response["result"]>,
): Buffer {
  return encodeFrame({
    protocolVersion: PROTOCOL_VERSION,
    requestId: request.requestId,
    startupNonce: nonce,
    deadlineUnixMs: 0n,
    idempotencyKey: request.idempotencyKey,
    payload: { $case: "response", value: { result } },
  });
}

function responseEnvelope(request: Envelope, overrides: Partial<HealthResponse> = {}): Envelope {
  return {
    protocolVersion: PROTOCOL_VERSION,
    requestId: request.requestId,
    startupNonce: nonce,
    deadlineUnixMs: 0n,
    idempotencyKey: request.idempotencyKey,
    payload: {
      $case: "response",
      value: {
        result: {
          $case: "health",
          value: {
            serviceVersion: "0.1.0",
            protocolVersion: PROTOCOL_VERSION,
            instanceId: "daemon-test",
            agentRuntime: undefined,
            automationScheduler: AutomationSchedulerHealth.AUTOMATION_SCHEDULER_HEALTH_KERNEL_INITIALIZED_EXECUTION_DISABLED,
            ...overrides,
          },
        },
      },
    },
  };
}

function encodeFrame(envelope: Envelope): Buffer {
  const payload = Buffer.from(Envelope.encode(envelope).finish());
  const frame = Buffer.alloc(payload.byteLength + 4);
  frame.writeUInt32BE(payload.byteLength, 0);
  payload.copy(frame, 4);
  return frame;
}

function wireMessage(id: string, role: MessageRole, content: string, sequence: bigint) {
  return {
    id,
    threadId: "thread-1",
    sequence,
    role,
    content,
    state: MessageState.MESSAGE_STATE_ACTIVE,
    revision: 0n,
    createdAtUnixMs: sequence,
    updatedAtUnixMs: sequence,
    derivation: { origin: { $case: "original" as const, value: {} } },
  };
}

function expectPollCursor(request: Envelope, afterSequence: bigint): void {
  expect(request.idempotencyKey).toBe("");
  expect(request.payload?.$case === "request" && request.payload.value.operation).toMatchObject({
    $case: "pollRunEvents",
    value: {
      runId: "run-1",
      afterSequence,
      limit: 2,
      waitTimeoutMs: 0,
    },
  });
}

function wireRunEvent(sequence: bigint) {
  return {
    sequence,
    runId: "run-1",
    occurredAtUnixMs: 10n + sequence,
    kind: RunEventKind.RUN_EVENT_KIND_CREATED,
    fromState: RunState.RUN_STATE_UNSPECIFIED,
    toState: RunState.RUN_STATE_UNSPECIFIED,
    relatedId: "",
  };
}

function runEventBatchFrame(
  request: Envelope,
  events: ReturnType<typeof wireRunEvent>[],
  nextSequence: bigint,
  hasMore: boolean,
): Buffer {
  return encodeFrame({
    protocolVersion: PROTOCOL_VERSION,
    requestId: request.requestId,
    startupNonce: nonce,
    deadlineUnixMs: 0n,
    idempotencyKey: "",
    payload: {
      $case: "response",
      value: {
        result: {
          $case: "runEventBatch",
          value: { events, nextSequence, hasMore },
        },
      },
    },
  });
}

function wireConversationEvent(
  sequence: bigint,
  kind: ConversationTurnEventKind,
  overrides: Partial<ConversationTurnEvent> = {},
): ConversationTurnEvent {
  return {
    sequence,
    turnId: "turn-1",
    kind,
    fromState: ConversationTurnState.CONVERSATION_TURN_STATE_UNSPECIFIED,
    toState: ConversationTurnState.CONVERSATION_TURN_STATE_UNSPECIFIED,
    startUtf8Offset: 0n,
    textAppended: "",
    ...overrides,
  };
}

function conversationEventBatchFrame(
  request: Envelope,
  events: ConversationTurnEvent[],
  nextSequence: bigint,
  hasMore: boolean,
): Buffer {
  return encodeFrame({
    protocolVersion: PROTOCOL_VERSION,
    requestId: request.requestId,
    startupNonce: nonce,
    deadlineUnixMs: 0n,
    idempotencyKey: "",
    payload: {
      $case: "response",
      value: {
        result: {
          $case: "conversationTurnEventBatch",
          value: { events, nextSequence, hasMore },
        },
      },
    },
  });
}
