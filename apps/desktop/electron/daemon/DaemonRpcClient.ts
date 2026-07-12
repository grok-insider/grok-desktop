import { randomUUID, timingSafeEqual } from "node:crypto";
import type { Duplex } from "node:stream";
import {
  Envelope,
  type Approval,
  type ArtifactList,
  type ArtifactOperationResult,
  type Automation,
  type AutomationList,
  type CapabilityStatus,
  type ChatModelCatalog,
  type ChatModelPreference,
  type ConversationForkDelivery,
  type ConversationForkMetadata,
  type ConversationForkResult,
  type ConversationTurnEvent,
  type ConversationTurnEventBatch,
  ConversationTurnEventKind,
  type ConversationTurnList,
  type ConversationTurnResult,
  ConversationTurnState,
  type DesktopPreferences,
  type HealthResponse,
  type MessageList,
  type Project,
  type ProjectList,
  type Request,
  type Response,
  type RunEvent,
  RunEventKind,
  RunState,
  type Thread,
  type ThreadList,
  type WorkspaceSearchResults,
} from "../generated/daemon/v1/daemon.js";

// Epoch twenty withdraws unsafe scheduler execution and managed-integration mutations.
export const PROTOCOL_VERSION = 20;
export const MAX_FRAME_BYTES = 4 * 1024 * 1024;
const DEFAULT_REQUEST_TIMEOUT_MS = 5_000;
const DEFAULT_RESPONSE_GRACE_MS = 1_000;
const DEFAULT_MAX_PENDING_REQUESTS = 32;
const EXPIRED_REQUEST_TTL_MS = 60_000;
const MAX_EXPIRED_REQUEST_IDS = 128;
// The daemon allows two minutes for native entry and provider validation.
export const CREDENTIAL_ENROLLMENT_RPC_TIMEOUT_MS = 125_000;
// Start performs bounded credential/model preflight but returns before provider completion.
export const CONVERSATION_START_RPC_TIMEOUT_MS = 20_000;
// The daemon caps model discovery at 12 seconds and retains commit/response reserve.
export const CHAT_MODEL_RPC_TIMEOUT_MS = 16_000;
export const RUN_EVENT_POLL_MAX_BATCH_SIZE = 100;
export const RUN_EVENT_POLL_MAX_WAIT_MS = 20_000;
export const RUN_EVENT_POLL_DEFAULT_WAIT_MS = 15_000;
export const CONVERSATION_EVENT_POLL_MAX_BATCH_SIZE = 100;
export const CONVERSATION_EVENT_POLL_MAX_WAIT_MS = 20_000;
export const CONVERSATION_EVENT_POLL_DEFAULT_WAIT_MS = 15_000;
const RUN_EVENT_POLL_RPC_RESERVE_MS = 2_000;
const RUN_EVENT_SUBSCRIPTION_INITIAL_RETRY_MS = 100;
const RUN_EVENT_SUBSCRIPTION_MAX_RETRY_MS = 2_000;
const RUN_EVENT_LISTENER_MAX_DELIVERY_MS = 5_000;
const CONVERSATION_EVENT_MAX_TEXT_BYTES = 1024 * 1024;
const CONVERSATION_EVENT_MAX_CHUNK_BYTES = 16 * 1024;
const CONVERSATION_EVENT_MAX_TEXT_EVENTS = 4_097;
const U64_MAX = (1n << 64n) - 1n;
const DURABLE_RUN_EVENT_SEQUENCE_MAX = (1n << 63n) - 1n;
const MAX_ARTIFACT_CONTENT_VERSION = 1_000_000;
const ARTIFACT_IMPORT_RPC_TIMEOUT_MS = 35_000;
const ARTIFACT_OPEN_RPC_TIMEOUT_MS = 15_000;
const ARTIFACT_REMOVE_RPC_TIMEOUT_MS = 35_000;

export type StreamConnector = () => Promise<Duplex>;
export type RpcConnectionState = "connecting" | "connected" | "disconnected";

type ResponseResult = NonNullable<Response["result"]>;
type ResultValueMap = {
  health: Extract<ResponseResult, { $case: "health" }>["value"];
  capabilities: Extract<ResponseResult, { $case: "capabilities" }>["value"];
  events: Extract<ResponseResult, { $case: "events" }>["value"];
  approval: Extract<ResponseResult, { $case: "approval" }>["value"];
  project: Extract<ResponseResult, { $case: "project" }>["value"];
  projects: Extract<ResponseResult, { $case: "projects" }>["value"];
  thread: Extract<ResponseResult, { $case: "thread" }>["value"];
  threads: Extract<ResponseResult, { $case: "threads" }>["value"];
  message: Extract<ResponseResult, { $case: "message" }>["value"];
  messages: Extract<ResponseResult, { $case: "messages" }>["value"];
  artifact: Extract<ResponseResult, { $case: "artifact" }>["value"];
  artifacts: Extract<ResponseResult, { $case: "artifacts" }>["value"];
  automation: Extract<ResponseResult, { $case: "automation" }>["value"];
  automations: Extract<ResponseResult, { $case: "automations" }>["value"];
  automationHistory: Extract<ResponseResult, { $case: "automationHistory" }>["value"];
  searchResults: Extract<ResponseResult, { $case: "searchResults" }>["value"];
  accountState: Extract<ResponseResult, { $case: "accountState" }>["value"];
  conversationTurn: Extract<ResponseResult, { $case: "conversationTurn" }>["value"];
  conversationTurns: Extract<ResponseResult, { $case: "conversationTurns" }>["value"];
  desktopPreferences: Extract<ResponseResult, { $case: "desktopPreferences" }>["value"];
  runEventBatch: Extract<ResponseResult, { $case: "runEventBatch" }>["value"];
  chatModelCatalog: Extract<ResponseResult, { $case: "chatModelCatalog" }>["value"];
  chatModelPreference: Extract<ResponseResult, { $case: "chatModelPreference" }>["value"];
  conversationTurnEventBatch: Extract<ResponseResult, { $case: "conversationTurnEventBatch" }>["value"];
  conversationFork: Extract<ResponseResult, { $case: "conversationFork" }>["value"];
  conversationForkMetadata: Extract<ResponseResult, { $case: "conversationForkMetadata" }>["value"];
  conversationForkDelivery: Extract<ResponseResult, { $case: "conversationForkDelivery" }>["value"];
  artifactOperation: Extract<ResponseResult, { $case: "artifactOperation" }>["value"];
  grokBuildAuthStatus: Extract<ResponseResult, { $case: "grokBuildAuthStatus" }>["value"];
};

type RequestOperation = NonNullable<Request["operation"]>;
type OperationValue<K extends RequestOperation["$case"]> = Extract<RequestOperation, { $case: K }>["value"];

type PendingRequest = {
  resolve(result: ResponseResult): void;
  reject(error: Error): void;
  timeout: NodeJS.Timeout;
};

export class DaemonTransportError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "DaemonTransportError";
  }
}

export class DaemonProtocolError extends Error {
  constructor(message: string, options?: ErrorOptions) {
    super(message, options);
    this.name = "DaemonProtocolError";
  }
}

export class DaemonResponseError extends Error {
  constructor(
    message: string,
    readonly code: number,
    readonly retryable: boolean,
  ) {
    super(message);
    this.name = "DaemonResponseError";
  }
}

export interface DaemonRpcClientOptions {
  nonce: Uint8Array;
  connect: StreamConnector;
  requestTimeoutMs?: number;
  responseGraceMs?: number;
  maxPendingRequests?: number;
  onConnectionState?(state: RpcConnectionState, error?: Error): void;
}

export interface DeliveredRunEventBatch {
  readonly events: readonly RunEvent[];
  readonly nextSequence: bigint;
  readonly hasMore: boolean;
}

export interface DaemonRunEventSubscriptionOptions {
  runId: string;
  afterSequence: bigint;
  listener(batch: DeliveredRunEventBatch): void | Promise<void>;
  onError?(error: Error): void;
  batchLimit?: number;
  waitTimeoutMs?: number;
  /** Test-only tuning remains bounded by the production maximum. */
  initialRetryMs?: number;
  /** Test-only tuning remains bounded by the production maximum. */
  listenerTimeoutMs?: number;
}

export interface DeliveredConversationTurnEventBatch {
  readonly events: readonly ConversationTurnEvent[];
  readonly nextSequence: bigint;
  readonly hasMore: boolean;
}

export interface DaemonConversationTurnEventSubscriptionOptions {
  turnId: string;
  listener(batch: DeliveredConversationTurnEventBatch): void | Promise<void>;
  onError?(error: Error): void;
  batchLimit?: number;
  waitTimeoutMs?: number;
  /** Test-only tuning remains bounded by the production maximum. */
  initialRetryMs?: number;
  /** Test-only tuning remains bounded by the production maximum. */
  listenerTimeoutMs?: number;
}

/** Correlated, bounded Protobuf RPC over a local duplex stream. */
export class DaemonRpcClient {
  private readonly nonce: Buffer;
  private readonly connector: StreamConnector;
  private readonly requestTimeoutMs: number;
  private readonly responseGraceMs: number;
  private readonly maxPendingRequests: number;
  private readonly onConnectionState?: DaemonRpcClientOptions["onConnectionState"];
  private readonly pending = new Map<string, PendingRequest>();
  private readonly expiredRequestIds = new Map<string, number>();
  private stream: Duplex | undefined;
  private connectPromise: Promise<Duplex> | undefined;
  private readBuffer = Buffer.alloc(0);
  private closed = false;

  constructor(options: DaemonRpcClientOptions) {
    if (options.nonce.byteLength !== 32) {
      throw new DaemonProtocolError("daemon startup nonce must be exactly 32 bytes");
    }
    this.nonce = Buffer.from(options.nonce);
    this.connector = options.connect;
    this.requestTimeoutMs = options.requestTimeoutMs ?? DEFAULT_REQUEST_TIMEOUT_MS;
    this.responseGraceMs = options.responseGraceMs ?? DEFAULT_RESPONSE_GRACE_MS;
    if (!Number.isSafeInteger(this.responseGraceMs) || this.responseGraceMs < 1 || this.responseGraceMs > 10_000) {
      throw new DaemonProtocolError("invalid daemon response grace");
    }
    this.maxPendingRequests = options.maxPendingRequests ?? DEFAULT_MAX_PENDING_REQUESTS;
    this.onConnectionState = options.onConnectionState;
  }

  async request(
    operation: NonNullable<Request["operation"]>,
    idempotencyKey = "",
    timeoutMs = this.requestTimeoutMs,
  ): Promise<ResponseResult> {
    if (this.closed) throw new DaemonTransportError("daemon client is closed");
    if (this.pending.size >= this.maxPendingRequests) {
      throw new DaemonTransportError("too many pending daemon requests");
    }
    if (!Number.isSafeInteger(timeoutMs) || timeoutMs < 1 || timeoutMs > 180_000) {
      throw new DaemonProtocolError("invalid daemon request timeout");
    }

    const stream = await this.ensureConnected();
    const requestId = randomUUID();
    const envelope: Envelope = {
      protocolVersion: PROTOCOL_VERSION,
      requestId,
      startupNonce: this.nonce,
      deadlineUnixMs: BigInt(Date.now() + timeoutMs),
      idempotencyKey,
      payload: { $case: "request", value: { operation } },
    };
    const payload = Buffer.from(Envelope.encode(envelope).finish());
    if (payload.byteLength === 0 || payload.byteLength > MAX_FRAME_BYTES) {
      throw new DaemonProtocolError("encoded daemon request exceeds the frame limit");
    }
    const frame = Buffer.allocUnsafe(payload.byteLength + 4);
    frame.writeUInt32BE(payload.byteLength, 0);
    payload.copy(frame, 4);

    const response = new Promise<ResponseResult>((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(requestId);
        this.rememberExpiredRequest(requestId);
        reject(new DaemonTransportError(`daemon request ${requestId} timed out`));
      }, timeoutMs + this.responseGraceMs);
      this.pending.set(requestId, { resolve, reject, timeout });
    });

    const write = writeAll(stream, frame);
    void write.catch(() => undefined);
    try {
      await Promise.race([write, response.then(() => undefined)]);
    } catch (error) {
      if (!this.pending.has(requestId)) {
        this.failConnection(stream, new DaemonTransportError("daemon stream write did not complete in time"));
        return response;
      }
      this.rejectPending(requestId, transportError("failed to write daemon request", error));
      this.failConnection(stream, transportError("daemon stream write failed", error));
    }
    return response;
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    const error = new DaemonTransportError("daemon client closed");
    this.rejectAll(error);
    this.stream?.destroy();
    this.stream = undefined;
    this.connectPromise = undefined;
    this.readBuffer = Buffer.alloc(0);
    this.expiredRequestIds.clear();
  }

  private async ensureConnected(): Promise<Duplex> {
    if (this.stream && !this.stream.destroyed) return this.stream;
    if (this.connectPromise) return this.connectPromise;
    this.onConnectionState?.("connecting");
    this.connectPromise = this.connector()
      .then((stream) => {
        if (this.closed) {
          stream.destroy();
          throw new DaemonTransportError("daemon client closed while connecting");
        }
        this.stream = stream;
        this.readBuffer = Buffer.alloc(0);
        stream.on("data", (chunk: Buffer | Uint8Array | string) => this.onData(stream, chunk));
        stream.once("error", (error) => this.failConnection(stream, transportError("daemon stream failed", error)));
        stream.once("close", () => this.failConnection(stream, new DaemonTransportError("daemon stream closed")));
        this.onConnectionState?.("connected");
        return stream;
      })
      .catch((error) => {
        const normalized = transportError("could not connect to the daemon", error);
        this.onConnectionState?.("disconnected", normalized);
        throw normalized;
      })
      .finally(() => {
        this.connectPromise = undefined;
      });
    return this.connectPromise;
  }

  private onData(stream: Duplex, chunk: Buffer | Uint8Array | string): void {
    if (stream !== this.stream) return;
    let incoming = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    while (incoming.byteLength > 0 || this.readBuffer.byteLength > 0) {
      if (this.readBuffer.byteLength > 0) {
        const headerNeeded = Math.max(0, 4 - this.readBuffer.byteLength);
        if (headerNeeded > 0) {
          const consumed = Math.min(headerNeeded, incoming.byteLength);
          this.readBuffer = Buffer.concat([this.readBuffer, incoming.subarray(0, consumed)]);
          incoming = incoming.subarray(consumed);
          if (this.readBuffer.byteLength < 4) return;
        }
        const length = this.readBuffer.readUInt32BE(0);
        if (length === 0 || length > MAX_FRAME_BYTES) {
          this.failConnection(stream, new DaemonProtocolError(`invalid daemon frame length ${length}`));
          return;
        }
        const frameBytes = length + 4;
        const consumed = Math.min(frameBytes - this.readBuffer.byteLength, incoming.byteLength);
        if (consumed > 0) {
          this.readBuffer = Buffer.concat([this.readBuffer, incoming.subarray(0, consumed)]);
          incoming = incoming.subarray(consumed);
        }
        if (this.readBuffer.byteLength < frameBytes) return;
        const payload = this.readBuffer.subarray(4, frameBytes);
        this.readBuffer = Buffer.alloc(0);
        try {
          this.handleEnvelope(Envelope.decode(payload));
        } catch (error) {
          this.failConnection(stream, protocolError("invalid daemon response", error));
          return;
        }
        continue;
      }

      if (incoming.byteLength < 4) {
        this.readBuffer = Buffer.from(incoming);
        return;
      }
      const length = incoming.readUInt32BE(0);
      if (length === 0 || length > MAX_FRAME_BYTES) {
        this.failConnection(stream, new DaemonProtocolError(`invalid daemon frame length ${length}`));
        return;
      }
      const frameBytes = length + 4;
      if (incoming.byteLength < frameBytes) {
        this.readBuffer = Buffer.from(incoming);
        return;
      }
      const payload = incoming.subarray(4, frameBytes);
      incoming = incoming.subarray(frameBytes);
      try {
        this.handleEnvelope(Envelope.decode(payload));
      } catch (error) {
        this.failConnection(stream, protocolError("invalid daemon response", error));
        return;
      }
    }
  }

  private handleEnvelope(envelope: Envelope): void {
    if (envelope.protocolVersion !== PROTOCOL_VERSION) {
      throw new DaemonProtocolError(`unsupported daemon protocol version ${envelope.protocolVersion}`);
    }
    if (envelope.startupNonce.byteLength !== this.nonce.byteLength || !timingSafeEqual(envelope.startupNonce, this.nonce)) {
      throw new DaemonProtocolError("daemon response nonce does not match this process");
    }
    if (envelope.payload?.$case !== "response") {
      throw new DaemonProtocolError("daemon response payload is missing");
    }
    const pending = this.pending.get(envelope.requestId);
    if (!pending) {
      if (this.consumeExpiredRequest(envelope.requestId)) return;
      throw new DaemonProtocolError("daemon response request id is not pending");
    }
    this.pending.delete(envelope.requestId);
    clearTimeout(pending.timeout);
    const result = envelope.payload.value.result;
    if (!result) {
      pending.reject(new DaemonProtocolError("daemon response result is missing"));
      return;
    }
    if (result.$case === "error") {
      pending.reject(new DaemonResponseError(result.value.message, result.value.code, result.value.retryable));
      return;
    }
    pending.resolve(result);
  }

  private failConnection(stream: Duplex, error: Error): void {
    if (stream !== this.stream) return;
    this.stream = undefined;
    this.readBuffer = Buffer.alloc(0);
    stream.destroy();
    this.rejectAll(error);
    if (!this.closed) this.onConnectionState?.("disconnected", error);
  }

  private rejectPending(requestId: string, error: Error): void {
    const pending = this.pending.get(requestId);
    if (!pending) return;
    this.pending.delete(requestId);
    clearTimeout(pending.timeout);
    pending.reject(error);
  }

  private rejectAll(error: Error): void {
    for (const [requestId] of this.pending) this.rejectPending(requestId, error);
  }

  private rememberExpiredRequest(requestId: string): void {
    this.pruneExpiredRequests();
    while (this.expiredRequestIds.size >= MAX_EXPIRED_REQUEST_IDS) {
      const oldest = this.expiredRequestIds.keys().next().value as string | undefined;
      if (!oldest) break;
      this.expiredRequestIds.delete(oldest);
    }
    this.expiredRequestIds.set(requestId, Date.now() + EXPIRED_REQUEST_TTL_MS);
  }

  private consumeExpiredRequest(requestId: string): boolean {
    this.pruneExpiredRequests();
    if (!this.expiredRequestIds.has(requestId)) return false;
    this.expiredRequestIds.delete(requestId);
    return true;
  }

  private pruneExpiredRequests(): void {
    const now = Date.now();
    for (const [requestId, expiresAt] of this.expiredRequestIds) {
      if (expiresAt <= now) this.expiredRequestIds.delete(requestId);
    }
  }
}

/** Operation-level facade that prevents generated wire types escaping main. */
export class DaemonProtocolClient {
  constructor(private readonly rpc: DaemonRpcClient) {}

  async health(): Promise<HealthResponse> {
    return expectResult(await this.rpc.request({ $case: "health", value: {} }), "health");
  }

  async resolveCapabilities(): Promise<CapabilityStatus[]> {
    const response = expectResult(
      await this.rpc.request(
        { $case: "resolveCapabilities", value: { facts: undefined } },
        "",
        CHAT_MODEL_RPC_TIMEOUT_MS,
      ),
      "capabilities",
    );
    return response.statuses;
  }

  async pollRunEvents(
    runId: string,
    afterSequence: bigint,
    limit = RUN_EVENT_POLL_MAX_BATCH_SIZE,
    waitTimeoutMs = RUN_EVENT_POLL_DEFAULT_WAIT_MS,
  ): Promise<DeliveredRunEventBatch> {
    validateIdentifier(runId, "run id");
    validateRunEventCursor(afterSequence);
    if (!Number.isSafeInteger(limit) || limit < 1 || limit > RUN_EVENT_POLL_MAX_BATCH_SIZE) {
      throw new DaemonProtocolError("run event poll limit must be between 1 and 100");
    }
    if (!Number.isSafeInteger(waitTimeoutMs) || waitTimeoutMs < 0 || waitTimeoutMs > RUN_EVENT_POLL_MAX_WAIT_MS) {
      throw new DaemonProtocolError("run event poll wait must be between 0 and 20000 milliseconds");
    }
    const response = expectResult(
      await this.rpc.request(
        { $case: "pollRunEvents", value: { runId, afterSequence, limit, waitTimeoutMs } },
        "",
        waitTimeoutMs + RUN_EVENT_POLL_RPC_RESERVE_MS,
      ),
      "runEventBatch",
    );
    try {
      return validateRunEventBatch(response, runId, afterSequence, limit);
    } catch (error) {
      this.rpc.close();
      throw error;
    }
  }

  async decideApproval(approvalId: string, expectedRevision: bigint, decision: number, idempotencyKey: string): Promise<Approval> {
    return expectResult(
      await this.rpc.request(
        { $case: "decideApproval", value: { approvalId, expectedRevision, decision } },
        idempotencyKey,
      ),
      "approval",
    );
  }

  async createProject(name: string, description: string, idempotencyKey: string): Promise<Project> {
    return expectResult(
      await this.rpc.request({ $case: "createProject", value: { name, description } }, idempotencyKey),
      "project",
    );
  }

  async listProjects(cursor = "", limit = 100): Promise<ProjectList> {
    return expectResult(
      await this.rpc.request({ $case: "listProjects", value: { cursor, limit } }),
      "projects",
    );
  }

  async createThread(projectId: string, title: string, idempotencyKey: string): Promise<Thread> {
    return expectResult(
      await this.rpc.request({ $case: "createThread", value: { projectId, title } }, idempotencyKey),
      "thread",
    );
  }

  async getThread(threadId: string): Promise<Thread> {
    return expectResult(
      await this.rpc.request({ $case: "getThread", value: { threadId } }),
      "thread",
    );
  }

  async listThreads(projectId: string, cursor = "", limit = 100): Promise<ThreadList> {
    return expectResult(
      await this.rpc.request({ $case: "listThreads", value: { projectId, cursor, limit } }),
      "threads",
    );
  }

  async listMessages(threadId: string, cursor = "", limit = 100): Promise<MessageList> {
    return expectResult(
      await this.rpc.request({ $case: "listMessages", value: { threadId, cursor, limit } }),
      "messages",
    );
  }

  async startConversationTurn(
    threadId: string,
    content: string,
    idempotencyKey: string,
  ): Promise<ConversationTurnResult> {
    return expectResult(
      await this.rpc.request(
        { $case: "startConversationTurn", value: { threadId, content } },
        idempotencyKey,
        CONVERSATION_START_RPC_TIMEOUT_MS,
      ),
      "conversationTurn",
    );
  }

  async cancelConversationTurn(
    turnId: string,
    expectedRevision: bigint,
    idempotencyKey: string,
  ): Promise<ConversationTurnResult> {
    validateIdentifier(turnId, "conversation turn id");
    validateDurableSequence(expectedRevision, "conversation turn revision");
    return expectResult(
      await this.rpc.request(
        { $case: "cancelConversationTurn", value: { turnId, expectedRevision } },
        idempotencyKey,
      ),
      "conversationTurn",
    );
  }

  async retryConversationTurn(
    sourceTurnId: string,
    expectedRevision: bigint,
    idempotencyKey: string,
  ): Promise<ConversationTurnResult> {
    validateIdentifier(sourceTurnId, "conversation retry source turn id");
    validateDurableSequence(expectedRevision, "conversation retry source revision");
    return expectResult(
      await this.rpc.request(
        {
          $case: "retryConversationTurn",
          value: { sourceTurnId, expectedRevision },
        },
        idempotencyKey,
        CONVERSATION_START_RPC_TIMEOUT_MS,
      ),
      "conversationTurn",
    );
  }

  async branchConversationThread(
    sourceTurnId: string,
    expectedRevision: bigint,
    idempotencyKey: string,
  ): Promise<ConversationForkResult> {
    validateIdentifier(sourceTurnId, "conversation branch source turn id");
    validateDurableSequence(expectedRevision, "conversation branch source revision");
    return expectResult(
      await this.rpc.request(
        {
          $case: "branchConversationThread",
          value: { sourceTurnId, expectedRevision },
        },
        idempotencyKey,
      ),
      "conversationFork",
    );
  }

  async editAndBranchConversationTurn(
    sourceTurnId: string,
    expectedRevision: bigint,
    content: string,
    idempotencyKey: string,
  ): Promise<ConversationForkResult> {
    validateIdentifier(sourceTurnId, "conversation edit source turn id");
    validateDurableSequence(expectedRevision, "conversation edit source revision");
    return expectResult(
      await this.rpc.request(
        {
          $case: "editAndBranchConversationTurn",
          value: { sourceTurnId, expectedRevision, content },
        },
        idempotencyKey,
        CONVERSATION_START_RPC_TIMEOUT_MS,
      ),
      "conversationFork",
    );
  }

  async regenerateConversationTurn(
    sourceTurnId: string,
    expectedRevision: bigint,
    idempotencyKey: string,
  ): Promise<ConversationForkResult> {
    validateIdentifier(sourceTurnId, "conversation regenerate source turn id");
    validateDurableSequence(expectedRevision, "conversation regenerate source revision");
    return expectResult(
      await this.rpc.request(
        {
          $case: "regenerateConversationTurn",
          value: { sourceTurnId, expectedRevision },
        },
        idempotencyKey,
        CONVERSATION_START_RPC_TIMEOUT_MS,
      ),
      "conversationFork",
    );
  }

  async getConversationForkMetadata(threadId: string): Promise<ConversationForkMetadata> {
    validateIdentifier(threadId, "conversation thread id");
    return expectResult(
      await this.rpc.request({
        $case: "getConversationForkMetadata",
        value: { threadId },
      }),
      "conversationForkMetadata",
    );
  }

  async acknowledgeConversationForkDelivery(
    childThreadId: string,
    expectedRevision: bigint,
    idempotencyKey: string,
  ): Promise<ConversationForkDelivery> {
    validateIdentifier(childThreadId, "conversation fork child thread id");
    validateDurableSequence(expectedRevision, "conversation fork delivery revision");
    return expectResult(
      await this.rpc.request(
        {
          $case: "acknowledgeConversationForkDelivery",
          value: { childThreadId, expectedRevision },
        },
        idempotencyKey,
      ),
      "conversationForkDelivery",
    );
  }

  async pollConversationTurnEvents(
    turnId: string,
    afterSequence: bigint,
    limit = CONVERSATION_EVENT_POLL_MAX_BATCH_SIZE,
    waitTimeoutMs = CONVERSATION_EVENT_POLL_DEFAULT_WAIT_MS,
  ): Promise<ConversationTurnEventBatch> {
    validateIdentifier(turnId, "conversation turn id");
    validateConversationEventCursor(afterSequence);
    validateConversationPollOptions(limit, waitTimeoutMs);
    return expectResult(
      await this.rpc.request(
        {
          $case: "pollConversationTurnEvents",
          value: { turnId, afterSequence, limit, waitTimeoutMs },
        },
        "",
        waitTimeoutMs + RUN_EVENT_POLL_RPC_RESERVE_MS,
      ),
      "conversationTurnEventBatch",
    );
  }

  async listConversationTurns(
    threadId: string,
    cursor = "",
    limit = 100,
  ): Promise<ConversationTurnList> {
    return expectResult(
      await this.rpc.request({ $case: "listConversationTurns", value: { threadId, cursor, limit } }),
      "conversationTurns",
    );
  }

  async listArtifacts(projectId: string, cursor = "", limit = 100): Promise<ArtifactList> {
    return expectResult(
      await this.rpc.request({ $case: "listArtifacts", value: { projectId, cursor, limit } }),
      "artifacts",
    );
  }

  async importArtifact(
    projectId: string,
    displayName: string,
    mediaType: string,
    sourcePath: string,
    idempotencyKey: string,
  ): Promise<ArtifactOperationResult> {
    validateIdentifier(projectId, "artifact project id");
    validateArtifactText(displayName, "artifact display name", 200);
    validateArtifactText(mediaType, "artifact media type", 255);
    validateArtifactSourcePath(sourcePath);
    return expectResult(
      await this.rpc.request(
        {
          $case: "importArtifact",
          value: {
            projectId,
            threadId: undefined,
            displayName,
            mediaType,
            sourcePath,
          },
        },
        idempotencyKey,
        ARTIFACT_IMPORT_RPC_TIMEOUT_MS,
      ),
      "artifactOperation",
    );
  }

  async openArtifact(
    artifactId: string,
    contentVersion: number,
    idempotencyKey: string,
  ): Promise<ArtifactOperationResult> {
    validateIdentifier(artifactId, "artifact id");
    if (
      !Number.isSafeInteger(contentVersion)
      || contentVersion < 1
      || contentVersion > MAX_ARTIFACT_CONTENT_VERSION
    ) {
      throw new DaemonProtocolError("invalid artifact content version");
    }
    return expectResult(
      await this.rpc.request(
        { $case: "openArtifact", value: { artifactId, contentVersion } },
        idempotencyKey,
        ARTIFACT_OPEN_RPC_TIMEOUT_MS,
      ),
      "artifactOperation",
    );
  }

  async removeArtifact(
    artifactId: string,
    expectedRevision: bigint,
    expectedContentVersion: number,
    idempotencyKey: string,
  ): Promise<ArtifactOperationResult> {
    validateIdentifier(artifactId, "artifact id");
    validateDurableSequence(expectedRevision, "artifact revision");
    if (
      !Number.isSafeInteger(expectedContentVersion)
      || expectedContentVersion < 1
      || expectedContentVersion > MAX_ARTIFACT_CONTENT_VERSION
    ) {
      throw new DaemonProtocolError("invalid artifact content version");
    }
    if (expectedRevision !== BigInt(expectedContentVersion)) {
      throw new DaemonProtocolError("artifact revision does not match its content version");
    }
    return expectResult(
      await this.rpc.request(
        {
          $case: "removeArtifact",
          value: { artifactId, expectedRevision, expectedContentVersion },
        },
        idempotencyKey,
        ARTIFACT_REMOVE_RPC_TIMEOUT_MS,
      ),
      "artifactOperation",
    );
  }

  async createAutomation(value: OperationValue<"createAutomation">, idempotencyKey: string): Promise<Automation> {
    return expectResult(
      await this.rpc.request({ $case: "createAutomation", value }, idempotencyKey),
      "automation",
    );
  }

  async updateAutomation(value: OperationValue<"updateAutomation">, idempotencyKey: string): Promise<Automation> {
    return expectResult(
      await this.rpc.request({ $case: "updateAutomation", value }, idempotencyKey),
      "automation",
    );
  }

  async listAutomations(projectId: string, cursor = "", limit = 100): Promise<AutomationList> {
    return expectResult(
      await this.rpc.request({ $case: "listAutomations", value: { projectId, cursor, limit } }),
      "automations",
    );
  }

  async searchWorkspace(
    projectId: string,
    query: string,
    offset: number,
    limit: number,
  ): Promise<WorkspaceSearchResults> {
    return expectResult(
      await this.rpc.request({ $case: "searchWorkspace", value: { projectId, query, offset, limit } }),
      "searchResults",
    );
  }

  async getAccountState(): Promise<ResultValueMap["accountState"]> {
    return expectResult(
      await this.rpc.request({ $case: "getAccountState", value: {} }),
      "accountState",
    );
  }

  async startGrokBuildAuth(idempotencyKey: string): Promise<ResultValueMap["grokBuildAuthStatus"]> {
    return expectResult(
      await this.rpc.request(
        { $case: "startGrokBuildAuth", value: { idempotencyKey } },
        idempotencyKey,
      ),
      "grokBuildAuthStatus",
    );
  }

  async getGrokBuildAuthStatus(): Promise<ResultValueMap["grokBuildAuthStatus"]> {
    return expectResult(
      await this.rpc.request({ $case: "getGrokBuildAuthStatus", value: {} }),
      "grokBuildAuthStatus",
    );
  }

  async getManagedIntegration(
    integrationId: string,
  ): Promise<ResultValueMap["managedIntegration"]> {
    return expectResult(
      await this.rpc.request({
        $case: "getManagedIntegration",
        value: { integrationId },
      }),
      "managedIntegration",
    );
  }

  async changeManagedIntegration(
    integrationId: string,
    action: string,
    expectedRevision: bigint,
    idempotencyKey: string,
  ): Promise<ResultValueMap["managedIntegration"]> {
    return expectResult(
      await this.rpc.request(
        {
          $case: "changeManagedIntegration",
          value: { integrationId, action, expectedRevision },
        },
        idempotencyKey,
      ),
      "managedIntegration",
    );
  }

  async enrollXaiApiKey(parentWindowToken: bigint, idempotencyKey: string): Promise<ResultValueMap["accountState"]> {
    return expectResult(
      await this.rpc.request(
        { $case: "enrollXaiApiKey", value: { parentWindowToken } },
        idempotencyKey,
        CREDENTIAL_ENROLLMENT_RPC_TIMEOUT_MS,
      ),
      "accountState",
    );
  }

  async deleteXaiApiKey(idempotencyKey: string): Promise<ResultValueMap["accountState"]> {
    return expectResult(
      await this.rpc.request({ $case: "deleteXaiApiKey", value: {} }, idempotencyKey),
      "accountState",
    );
  }

  async getDesktopPreferences(): Promise<DesktopPreferences> {
    return expectResult(
      await this.rpc.request({ $case: "getDesktopPreferences", value: {} }),
      "desktopPreferences",
    );
  }

  async updateDesktopPreferences(
    expectedRevision: bigint,
    keepRunningInNotificationArea: boolean,
    idempotencyKey: string,
  ): Promise<DesktopPreferences> {
    return expectResult(
      await this.rpc.request(
        {
          $case: "updateDesktopPreferences",
          value: { expectedRevision, keepRunningInNotificationArea },
        },
        idempotencyKey,
      ),
      "desktopPreferences",
    );
  }

  async getChatModelCatalog(): Promise<ChatModelCatalog> {
    return expectResult(
      await this.rpc.request(
        { $case: "getChatModelCatalog", value: {} },
        "",
        CHAT_MODEL_RPC_TIMEOUT_MS,
      ),
      "chatModelCatalog",
    );
  }

  async selectChatModel(
    expectedRevision: bigint,
    modelId: string,
    idempotencyKey: string,
  ): Promise<ChatModelPreference> {
    return expectResult(
      await this.rpc.request(
        { $case: "selectChatModel", value: { expectedRevision, modelId } },
        idempotencyKey,
        CHAT_MODEL_RPC_TIMEOUT_MS,
      ),
      "chatModelPreference",
    );
  }

  close(): void {
    this.rpc.close();
  }
}

/**
 * Owns one dedicated read-only long-poll client. The cursor advances only
 * after a validated batch has been delivered successfully to the listener.
 */
export class DaemonRunEventSubscription {
  private readonly runId: string;
  private readonly listener: DaemonRunEventSubscriptionOptions["listener"];
  private readonly onError?: DaemonRunEventSubscriptionOptions["onError"];
  private readonly batchLimit: number;
  private readonly waitTimeoutMs: number;
  private readonly initialRetryMs: number;
  private readonly listenerTimeoutMs: number;
  private cursor: bigint;
  private stopped = false;
  private started = false;
  private cancelRetry: (() => void) | undefined;

  constructor(
    private readonly protocol: DaemonProtocolClient,
    options: DaemonRunEventSubscriptionOptions,
  ) {
    validateIdentifier(options.runId, "run id");
    validateRunEventCursor(options.afterSequence);
    if (typeof options.listener !== "function" || (options.onError && typeof options.onError !== "function")) {
      throw new DaemonProtocolError("invalid run event subscription listener");
    }
    this.runId = options.runId;
    this.cursor = options.afterSequence;
    this.listener = options.listener;
    this.onError = options.onError;
    this.batchLimit = options.batchLimit ?? RUN_EVENT_POLL_MAX_BATCH_SIZE;
    this.waitTimeoutMs = options.waitTimeoutMs ?? RUN_EVENT_POLL_DEFAULT_WAIT_MS;
    this.initialRetryMs = options.initialRetryMs ?? RUN_EVENT_SUBSCRIPTION_INITIAL_RETRY_MS;
    this.listenerTimeoutMs = options.listenerTimeoutMs ?? RUN_EVENT_LISTENER_MAX_DELIVERY_MS;
    if (
      !Number.isSafeInteger(this.initialRetryMs)
      || this.initialRetryMs < 1
      || this.initialRetryMs > RUN_EVENT_SUBSCRIPTION_MAX_RETRY_MS
    ) {
      throw new DaemonProtocolError("invalid run event subscription retry delay");
    }
    if (
      !Number.isSafeInteger(this.listenerTimeoutMs)
      || this.listenerTimeoutMs < 1
      || this.listenerTimeoutMs > RUN_EVENT_LISTENER_MAX_DELIVERY_MS
    ) {
      throw new DaemonProtocolError("invalid run event listener timeout");
    }
    // Reuse the operation validation before opening any subscription loop.
    validatePollOptions(this.batchLimit, this.waitTimeoutMs);
  }

  get afterSequence(): bigint {
    return this.cursor;
  }

  start(): Promise<void> {
    if (this.started) throw new DaemonProtocolError("run event subscription already started");
    this.started = true;
    return this.run();
  }

  close(): void {
    if (this.stopped) return;
    this.stopped = true;
    this.cancelRetry?.();
    this.cancelRetry = undefined;
    this.protocol.close();
  }

  private async run(): Promise<void> {
    let retryMs = this.initialRetryMs;
    try {
      while (!this.stopped) {
        let batch: DeliveredRunEventBatch;
        try {
          batch = await this.protocol.pollRunEvents(
            this.runId,
            this.cursor,
            this.batchLimit,
            this.waitTimeoutMs,
          );
        } catch (error) {
          if (this.stopped) return;
          const normalized = asError(error);
          if (!isRetryableEventPollError(normalized)) {
            this.reportError(normalized);
            return;
          }
          await this.waitBeforeRetry(retryMs);
          retryMs = Math.min(retryMs * 2, RUN_EVENT_SUBSCRIPTION_MAX_RETRY_MS);
          continue;
        }
        if (this.stopped) return;
        try {
          await withTimeout(
            Promise.resolve().then(() => this.listener(batch)),
            this.listenerTimeoutMs,
            "run event listener delivery timed out",
          );
        } catch (error) {
          if (this.stopped) return;
          this.reportError(asError(error));
          return;
        }
        if (this.stopped) return;
        this.cursor = batch.nextSequence;
        retryMs = this.initialRetryMs;
      }
    } finally {
      this.close();
    }
  }

  private waitBeforeRetry(delayMs: number): Promise<void> {
    if (this.stopped) return Promise.resolve();
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.cancelRetry = undefined;
        resolve();
      }, delayMs);
      this.cancelRetry = () => {
        clearTimeout(timer);
        resolve();
      };
    });
  }

  private reportError(error: Error): void {
    try {
      this.onError?.(error);
    } catch {
      // A reporting callback cannot revive or destabilize a failed channel.
    }
  }
}

type ConversationEventProjection = {
  state: ConversationTurnState | undefined;
  textOffset: bigint;
  textEvents: number;
  lastSequence: bigint;
};

/** Owns one dedicated resumable, read-only turn-event connection. */
export class DaemonConversationTurnEventSubscription {
  private readonly turnId: string;
  private readonly listener: DaemonConversationTurnEventSubscriptionOptions["listener"];
  private readonly onError?: DaemonConversationTurnEventSubscriptionOptions["onError"];
  private readonly batchLimit: number;
  private readonly waitTimeoutMs: number;
  private readonly initialRetryMs: number;
  private readonly listenerTimeoutMs: number;
  private cursor = 0n;
  private projection: ConversationEventProjection = {
    state: undefined,
    textOffset: 0n,
    textEvents: 0,
    lastSequence: 0n,
  };
  private stopped = false;
  private started = false;
  private cancelRetry: (() => void) | undefined;

  constructor(
    private readonly protocol: DaemonProtocolClient,
    options: DaemonConversationTurnEventSubscriptionOptions,
  ) {
    validateIdentifier(options.turnId, "conversation turn id");
    if (typeof options.listener !== "function" || (options.onError && typeof options.onError !== "function")) {
      throw new DaemonProtocolError("invalid conversation event subscription listener");
    }
    this.turnId = options.turnId;
    this.listener = options.listener;
    this.onError = options.onError;
    this.batchLimit = options.batchLimit ?? CONVERSATION_EVENT_POLL_MAX_BATCH_SIZE;
    this.waitTimeoutMs = options.waitTimeoutMs ?? CONVERSATION_EVENT_POLL_DEFAULT_WAIT_MS;
    this.initialRetryMs = options.initialRetryMs ?? RUN_EVENT_SUBSCRIPTION_INITIAL_RETRY_MS;
    this.listenerTimeoutMs = options.listenerTimeoutMs ?? RUN_EVENT_LISTENER_MAX_DELIVERY_MS;
    validateConversationPollOptions(this.batchLimit, this.waitTimeoutMs);
    if (
      !Number.isSafeInteger(this.initialRetryMs)
      || this.initialRetryMs < 1
      || this.initialRetryMs > RUN_EVENT_SUBSCRIPTION_MAX_RETRY_MS
    ) {
      throw new DaemonProtocolError("invalid conversation event subscription retry delay");
    }
    if (
      !Number.isSafeInteger(this.listenerTimeoutMs)
      || this.listenerTimeoutMs < 1
      || this.listenerTimeoutMs > RUN_EVENT_LISTENER_MAX_DELIVERY_MS
    ) {
      throw new DaemonProtocolError("invalid conversation event listener timeout");
    }
  }

  get afterSequence(): bigint {
    return this.cursor;
  }

  start(): Promise<void> {
    if (this.started) throw new DaemonProtocolError("conversation event subscription already started");
    this.started = true;
    return this.run();
  }

  close(): void {
    if (this.stopped) return;
    this.stopped = true;
    this.cancelRetry?.();
    this.cancelRetry = undefined;
    this.protocol.close();
  }

  private async run(): Promise<void> {
    let retryMs = this.initialRetryMs;
    try {
      while (!this.stopped) {
        let validated: {
          batch: DeliveredConversationTurnEventBatch;
          projection: ConversationEventProjection;
        };
        try {
          const batch = await this.protocol.pollConversationTurnEvents(
            this.turnId,
            this.cursor,
            this.batchLimit,
            this.waitTimeoutMs,
          );
          validated = validateConversationTurnEventBatch(
            batch,
            this.turnId,
            this.cursor,
            this.batchLimit,
            this.projection,
          );
        } catch (error) {
          if (this.stopped) return;
          const normalized = asError(error);
          if (!isRetryableEventPollError(normalized)) {
            this.reportError(normalized);
            return;
          }
          await this.waitBeforeRetry(retryMs);
          retryMs = Math.min(retryMs * 2, RUN_EVENT_SUBSCRIPTION_MAX_RETRY_MS);
          continue;
        }
        if (this.stopped) return;
        try {
          await withTimeout(
            Promise.resolve().then(() => this.listener(validated.batch)),
            this.listenerTimeoutMs,
            "conversation event listener delivery timed out",
          );
        } catch (error) {
          if (this.stopped) return;
          this.reportError(asError(error));
          return;
        }
        if (this.stopped) return;
        this.cursor = validated.batch.nextSequence;
        this.projection = validated.projection;
        retryMs = this.initialRetryMs;
        if (this.projection.state !== undefined && isTerminalConversationState(this.projection.state)) {
          return;
        }
      }
    } finally {
      this.close();
    }
  }

  private waitBeforeRetry(delayMs: number): Promise<void> {
    if (this.stopped) return Promise.resolve();
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.cancelRetry = undefined;
        resolve();
      }, delayMs);
      this.cancelRetry = () => {
        clearTimeout(timer);
        resolve();
      };
    });
  }

  private reportError(error: Error): void {
    try {
      this.onError?.(error);
    } catch {
      // A reporting callback cannot revive or destabilize a failed channel.
    }
  }
}

function expectResult<K extends keyof ResultValueMap>(
  result: ResponseResult,
  expected: K,
): ResultValueMap[K] {
  if (result.$case !== expected) {
    throw new DaemonProtocolError(`expected daemon ${expected} response, received ${result.$case}`);
  }
  return result.value as ResultValueMap[K];
}

function validatePollOptions(limit: number, waitTimeoutMs: number): void {
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > RUN_EVENT_POLL_MAX_BATCH_SIZE) {
    throw new DaemonProtocolError("run event poll limit must be between 1 and 100");
  }
  if (!Number.isSafeInteger(waitTimeoutMs) || waitTimeoutMs < 0 || waitTimeoutMs > RUN_EVENT_POLL_MAX_WAIT_MS) {
    throw new DaemonProtocolError("run event poll wait must be between 0 and 20000 milliseconds");
  }
}

function validateConversationPollOptions(limit: number, waitTimeoutMs: number): void {
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > CONVERSATION_EVENT_POLL_MAX_BATCH_SIZE) {
    throw new DaemonProtocolError("conversation event poll limit must be between 1 and 100");
  }
  if (
    !Number.isSafeInteger(waitTimeoutMs)
    || waitTimeoutMs < 0
    || waitTimeoutMs > CONVERSATION_EVENT_POLL_MAX_WAIT_MS
  ) {
    throw new DaemonProtocolError("conversation event poll wait must be between 0 and 20000 milliseconds");
  }
}

function validateConversationTurnEventBatch(
  batch: ConversationTurnEventBatch,
  turnId: string,
  afterSequence: bigint,
  limit: number,
  current: ConversationEventProjection,
): {
  batch: DeliveredConversationTurnEventBatch;
  projection: ConversationEventProjection;
} {
  if (batch.events.length > limit || (batch.hasMore && batch.events.length !== limit)) {
    throw new DaemonProtocolError("daemon conversation event batch exceeded its declared limit");
  }
  const projection = { ...current };
  let expected = afterSequence;
  for (const event of batch.events) {
    validateIdentifier(event.turnId, "conversation event turn id");
    if (event.turnId !== turnId) {
      throw new DaemonProtocolError("daemon conversation event ownership is invalid");
    }
    validateConversationEventCursor(event.sequence);
    if (event.sequence !== expected + 1n || event.sequence !== projection.lastSequence + 1n) {
      throw new DaemonProtocolError("daemon conversation event sequence is repeated, retrograde, or discontinuous");
    }
    if (projection.state !== undefined && isTerminalConversationState(projection.state)) {
      throw new DaemonProtocolError("daemon conversation event followed a terminal state");
    }
    applyConversationTurnEvent(event, projection);
    expected = event.sequence;
    projection.lastSequence = event.sequence;
  }
  if (batch.events.length === 0 && batch.hasMore) {
    throw new DaemonProtocolError("empty daemon conversation event batch cannot report more events");
  }
  validateConversationEventCursor(batch.nextSequence);
  if (batch.nextSequence !== expected) {
    throw new DaemonProtocolError("daemon conversation event cursor does not match the delivered batch");
  }
  return {
    batch: { events: batch.events, nextSequence: batch.nextSequence, hasMore: batch.hasMore },
    projection,
  };
}

function applyConversationTurnEvent(
  event: ConversationTurnEvent,
  projection: ConversationEventProjection,
): void {
  const hasStates = event.fromState !== ConversationTurnState.CONVERSATION_TURN_STATE_UNSPECIFIED
    || event.toState !== ConversationTurnState.CONVERSATION_TURN_STATE_UNSPECIFIED;
  switch (event.kind) {
    case ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_CREATED:
      if (
        event.sequence !== 1n
        || projection.state !== undefined
        || hasStates
        || event.startUtf8Offset !== 0n
        || event.textAppended !== ""
      ) {
        throw new DaemonProtocolError("invalid created conversation event");
      }
      projection.state = ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED;
      return;
    case ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_STATE_CHANGED:
      if (
        event.startUtf8Offset !== 0n
        || event.textAppended !== ""
        || projection.state !== event.fromState
        || !permitsConversationTransition(event.fromState, event.toState)
      ) {
        throw new DaemonProtocolError("invalid state-change conversation event");
      }
      projection.state = event.toState;
      return;
    case ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_TEXT_APPENDED: {
      const bytes = Buffer.byteLength(event.textAppended, "utf8");
      if (
        hasStates
        || projection.state !== ConversationTurnState.CONVERSATION_TURN_STATE_PROVIDER_STARTED
        || event.startUtf8Offset !== projection.textOffset
        || bytes < 1
        || bytes > CONVERSATION_EVENT_MAX_CHUNK_BYTES
        || containsUnsupportedConversationControl(event.textAppended)
      ) {
        throw new DaemonProtocolError("invalid text-append conversation event");
      }
      projection.textOffset += BigInt(bytes);
      projection.textEvents += 1;
      if (projection.textOffset > BigInt(CONVERSATION_EVENT_MAX_TEXT_BYTES)) {
        throw new DaemonProtocolError("daemon conversation event text exceeded the byte limit");
      }
      if (projection.textEvents > CONVERSATION_EVENT_MAX_TEXT_EVENTS) {
        throw new DaemonProtocolError("daemon conversation event count exceeded the limit");
      }
      return;
    }
    case ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_UNSPECIFIED:
    case ConversationTurnEventKind.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("unknown daemon conversation event kind");
  }
}

function permitsConversationTransition(
  from: ConversationTurnState,
  to: ConversationTurnState,
): boolean {
  return (
    from === ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED
    && (
      to === ConversationTurnState.CONVERSATION_TURN_STATE_PROVIDER_STARTED
      || to === ConversationTurnState.CONVERSATION_TURN_STATE_CANCELLED
    )
  ) || (
    from === ConversationTurnState.CONVERSATION_TURN_STATE_PROVIDER_STARTED
    && (
      to === ConversationTurnState.CONVERSATION_TURN_STATE_COMPLETED
      || to === ConversationTurnState.CONVERSATION_TURN_STATE_FAILED
      || to === ConversationTurnState.CONVERSATION_TURN_STATE_INTERRUPTED_NEEDS_REVIEW
    )
  );
}

function isTerminalConversationState(state: ConversationTurnState): boolean {
  return state === ConversationTurnState.CONVERSATION_TURN_STATE_COMPLETED
    || state === ConversationTurnState.CONVERSATION_TURN_STATE_FAILED
    || state === ConversationTurnState.CONVERSATION_TURN_STATE_CANCELLED
    || state === ConversationTurnState.CONVERSATION_TURN_STATE_INTERRUPTED_NEEDS_REVIEW;
}

function containsUnsupportedConversationControl(value: string): boolean {
  for (const character of value) {
    const point = character.codePointAt(0) ?? 0;
    if (character === "\0" || (point < 0x20 && character !== "\n" && character !== "\r" && character !== "\t")) {
      return true;
    }
  }
  return false;
}

function validateRunEventBatch(
  batch: ResultValueMap["runEventBatch"],
  runId: string,
  afterSequence: bigint,
  limit: number,
): DeliveredRunEventBatch {
  if (batch.events.length > limit || (batch.hasMore && batch.events.length !== limit)) {
    throw new DaemonProtocolError("daemon run event batch exceeded its declared limit");
  }
  let expected = afterSequence;
  for (const event of batch.events) {
    validateIdentifier(event.runId, "run event run id");
    if (event.runId !== runId) throw new DaemonProtocolError("daemon run event ownership is invalid");
    validateRunEventCursor(event.sequence);
    if (event.sequence !== expected + 1n) {
      throw new DaemonProtocolError("daemon run event sequence is repeated, retrograde, or discontinuous");
    }
    validateUnsigned64(event.occurredAtUnixMs, "run event time");
    validateRunEventShape(event);
    expected = event.sequence;
  }
  if (batch.events.length === 0 && batch.hasMore) {
    throw new DaemonProtocolError("empty daemon run event batch cannot report more events");
  }
  validateRunEventCursor(batch.nextSequence);
  if (batch.nextSequence !== expected) {
    throw new DaemonProtocolError("daemon run event cursor does not match the delivered batch");
  }
  return { events: batch.events, nextSequence: batch.nextSequence, hasMore: batch.hasMore };
}

function validateRunEventShape(event: RunEvent): void {
  const hasStates = event.fromState !== RunState.RUN_STATE_UNSPECIFIED
    || event.toState !== RunState.RUN_STATE_UNSPECIFIED;
  switch (event.kind) {
    case RunEventKind.RUN_EVENT_KIND_CREATED:
      if (event.sequence !== 1n || hasStates || event.relatedId !== "") {
        throw new DaemonProtocolError("invalid created run event");
      }
      break;
    case RunEventKind.RUN_EVENT_KIND_STATE_CHANGED:
      if (
        !isKnownRunState(event.fromState)
        || !isKnownRunState(event.toState)
        || event.relatedId !== ""
      ) {
        throw new DaemonProtocolError("invalid state-change run event");
      }
      break;
    case RunEventKind.RUN_EVENT_KIND_APPROVAL_REQUESTED:
    case RunEventKind.RUN_EVENT_KIND_EFFECT_PREPARED:
    case RunEventKind.RUN_EVENT_KIND_EFFECT_NEEDS_REVIEW:
      if (hasStates) throw new DaemonProtocolError("invalid related-object run event state");
      validateIdentifier(event.relatedId, "run event related id");
      break;
    case RunEventKind.RUN_EVENT_KIND_UNSPECIFIED:
    case RunEventKind.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("unknown daemon run event kind");
  }
}

function isKnownRunState(value: RunState): boolean {
  return value >= RunState.RUN_STATE_QUEUED
    && value <= RunState.RUN_STATE_INTERRUPTED_NEEDS_REVIEW;
}

function validateUnsigned64(value: bigint, label: string): void {
  if (value < 0n || value > U64_MAX) throw new DaemonProtocolError(`invalid ${label}`);
}

function validateRunEventCursor(value: bigint): void {
  if (value < 0n || value > DURABLE_RUN_EVENT_SEQUENCE_MAX) {
    throw new DaemonProtocolError("invalid run event cursor");
  }
}

function validateConversationEventCursor(value: bigint): void {
  if (value < 0n || value > DURABLE_RUN_EVENT_SEQUENCE_MAX) {
    throw new DaemonProtocolError("invalid conversation event cursor");
  }
}

function validateDurableSequence(value: bigint, label: string): void {
  if (value < 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new DaemonProtocolError(`invalid ${label}`);
  }
}

function validateIdentifier(value: string, label: string): void {
  if (
    value.length === 0
    || Buffer.byteLength(value, "utf8") > 128
    || hasControlCharacter(value)
  ) {
    throw new DaemonProtocolError(`invalid ${label}`);
  }
}

function validateArtifactText(value: string, label: string, maximum: number): void {
  if (
    value.length === 0
    || Buffer.byteLength(value, "utf8") > maximum
    || hasControlCharacter(value)
  ) {
    throw new DaemonProtocolError(`invalid ${label}`);
  }
}

function validateArtifactSourcePath(value: string): void {
  if (
    value.length === 0
    || Buffer.byteLength(value, "utf8") > 32 * 1024
    || hasControlCharacter(value)
  ) {
    throw new DaemonProtocolError("invalid artifact source path");
  }
}

function hasControlCharacter(value: string): boolean {
  for (const character of value) {
    const codePoint = character.codePointAt(0);
    if (
      codePoint !== undefined
      && (
        codePoint <= 0x1f
        || (codePoint >= 0x7f && codePoint <= 0x9f)
        || (codePoint >= 0xd800 && codePoint <= 0xdfff)
      )
    ) return true;
  }
  return false;
}

function isRetryableEventPollError(error: Error): boolean {
  return error instanceof DaemonTransportError
    || (error instanceof DaemonResponseError && error.retryable);
}

function asError(value: unknown): Error {
  return value instanceof Error ? value : new DaemonProtocolError("unknown run event subscription failure");
}

async function withTimeout<T>(promise: Promise<T>, timeoutMs: number, message: string): Promise<T> {
  let timer: NodeJS.Timeout | undefined;
  const timeout = new Promise<never>((_resolve, reject) => {
    timer = setTimeout(() => reject(new DaemonProtocolError(message)), timeoutMs);
  });
  try {
    return await Promise.race([promise, timeout]);
  } finally {
    if (timer) clearTimeout(timer);
  }
}

function writeAll(stream: Duplex, frame: Buffer): Promise<void> {
  return new Promise((resolve, reject) => {
    stream.write(frame, (error) => error ? reject(error) : resolve());
  });
}

function transportError(message: string, cause: unknown): DaemonTransportError {
  return cause instanceof DaemonTransportError ? cause : new DaemonTransportError(message, { cause });
}

function protocolError(message: string, cause: unknown): DaemonProtocolError {
  return cause instanceof DaemonProtocolError ? cause : new DaemonProtocolError(message, { cause });
}
