import { randomBytes, randomUUID } from "node:crypto";
import { existsSync, mkdirSync, rmSync } from "node:fs";
import net from "node:net";
import path from "node:path";
import { spawn, type ChildProcess } from "node:child_process";
import type {
  DaemonAccountState,
  DaemonApproval,
  DaemonArtifact,
  DaemonArtifactOpenReceipt,
  DaemonAutomation,
  DaemonAutomationInput,
  DaemonCapabilityStatus,
  DaemonChatModelCatalog,
  DaemonChatModelPreference,
  DaemonConversationFork,
  DaemonConversationForkDelivery,
  DaemonConversationForkKind,
  DaemonConversationForkMetadata,
  DaemonConversationInheritedOutcome,
  DaemonConversationTurnEventBatch,
  DaemonConversationTurn,
  DaemonDesktopPreferences,
  DaemonHostExecutionPolicy,
  DaemonHostWorkSnapshot,
  DaemonMessage,
  DaemonProject,
  DaemonRun,
  DaemonRunState,
  DaemonStatus,
  DaemonSuperGrokEnrollmentStatus,
  DaemonThread,
  DaemonUsageScopeKind,
  DaemonUsageSummary,
  DaemonUsageWindow,
  DaemonWorkspaceSnapshot,
  DaemonWorkspaceSearchResults,
} from "../../src/contracts/bridge.js";
import {
  ApprovalDecision,
  ApprovalRisk,
  ApprovalScope,
  ApprovalStatus,
  ArtifactOpenFailureCode,
  ArtifactOpenReceiptStatus,
  ArtifactState,
  AuthMethod,
  AutomationSchedulerHealth,
  AutomationState,
  Capability,
  CapabilityAvailability,
  CapabilitySurface,
  ConversationFailureKind,
  type ConversationForkDelivery,
  ConversationForkDeliveryState,
  ConversationForkKind,
  ConversationMessageDerivationKind,
  ConversationRetryEligibility,
  ConversationTurnResult,
  type ConversationTurnEvent,
  ConversationTurnEventKind,
  ConversationTurnOrigin,
  ConversationTurnState,
  MessageRole,
  MessageState,
  MissedRunPolicy,
  OverlapPolicy,
  ProjectState,
  type CapabilityStatus,
  type HostExecutionPolicy,
  type HostWorkSnapshot,
  type Run,
  type RunEvent,
  type SuperGrokEnrollmentStatus,
  RunEventKind,
  RunState,
  RunKind,
  ThreadState,
  WorkExecutionBackend,
  WorkspaceSearchKind,
} from "../generated/daemon/v1/daemon.js";
import {
  DaemonProtocolClient,
  DaemonProtocolError,
  DaemonConversationTurnEventSubscription,
  DaemonRunEventSubscription,
  DaemonRpcClient,
  PROTOCOL_VERSION,
  type RpcConnectionState,
} from "./DaemonRpcClient.js";
import {
  applyDevelopmentAcpDescriptor,
  resolveDevelopmentAcpDescriptor,
} from "./developmentAcpDescriptor.js";

// Daemon composition probes the optional official ACP runtime with a bounded
// 15-second initialization window before binding IPC. Keep the supervisor's
// deadline above that boundary so an unavailable Work runtime degrades the
// daemon instead of making every core capability look like a startup failure.
const STARTUP_TIMEOUT_MS = 25_000;
const CONNECT_ATTEMPT_TIMEOUT_MS = 500;
const RETRY_DELAY_MS = 100;
const MAX_UNEXPECTED_RESTARTS_PER_MINUTE = 3;
const WORKSPACE_PAGE_SIZE = 100;
const MAX_WORKSPACE_ENTITIES = 10_000;
const MAX_PROJECTS = 1_000;
const MAX_CONVERSATION_MESSAGES = 2_000;
const MAX_CONVERSATION_CONTEXT_MESSAGES = 1_000;
const MAX_CONVERSATION_TURNS = 2_000;
const MAX_CONVERSATION_BYTES = 16 * 1024 * 1024;
const MAX_CONVERSATION_TURN_BYTES = 32 * 1024 * 1024;
const MAX_CONVERSATION_CITATIONS = 256;
const MAX_CONVERSATION_CITATION_BYTES = 1_000_000;
const MAX_CONVERSATION_FAMILY_THREADS = 256;
const MAX_CONVERSATION_INHERITED_OUTCOMES = 256;
const MAX_CONVERSATION_FORK_METADATA_BYTES = 3 * 1024 * 1024;
const CONVERSATION_PAGE_SIZE = 3;
const CONVERSATION_TURN_PAGE_SIZE = 100;
const MAX_RUN_EVENT_SUBSCRIPTIONS = 8;
const MAX_WORKSPACE_SEARCH_RESULTS = 100;
const DAEMON_STARTUP_NONCE_BYTES = 32;

export type DaemonRunEventKind =
  | "created"
  | "state_changed"
  | "approval_requested"
  | "effect_prepared"
  | "effect_needs_review";

export interface DaemonRunEvent {
  sequence: number;
  runId: string;
  occurredAtUnixMs: number;
  kind: DaemonRunEventKind;
  fromState?: DaemonRunState;
  toState?: DaemonRunState;
  relatedId?: string;
}

export interface DaemonRunEventBatch {
  events: DaemonRunEvent[];
  nextSequence: number;
  hasMore: boolean;
}

export interface DaemonSupervisorOptions {
  appPath: string;
  resourcesPath: string;
  runtimeDirectory: string;
  platform?: NodeJS.Platform;
  daemonBinary?: string;
  /** Explicit development-launch gate for local binary and native-prompt overrides. */
  allowDevelopmentBinary: boolean;
  /** Forward daemon stderr to this process (development only) so startup failures are diagnosable. */
  inheritDaemonStderr?: boolean;
}

/** Owns one nonce-paired daemon process and reconnectable local RPC client. */
export class DaemonSupervisor {
  private readonly options: DaemonSupervisorOptions;
  private readonly platform: NodeJS.Platform;
  private readonly nonce = randomBytes(32);
  private readonly listeners = new Set<(status: DaemonStatus) => void>();
  private readonly runEventSubscriptions = new Set<DaemonRunEventSubscription>();
  private readonly conversationEventSubscriptions = new Set<DaemonConversationTurnEventSubscription>();
  private readonly runtimePath: string;
  private readonly endpoint: string;
  private child: ChildProcess | undefined;
  private protocol: DaemonProtocolClient | undefined;
  private startPromise: Promise<void> | undefined;
  private rpcGeneration = 0;
  private unexpectedRestartTimer: NodeJS.Timeout | undefined;
  private unexpectedRestartTimes: number[] = [];
  private unexpectedRestartRequiresManual = false;
  private stopping = false;
  private ready = false;
  private status: DaemonStatus = { state: "stopped", updatedAtUnixMs: Date.now() };

  constructor(options: DaemonSupervisorOptions) {
    this.options = options;
    this.platform = options.platform ?? process.platform;
    const suffix = `${process.pid}-${randomUUID()}`;
    this.runtimePath = path.join(options.runtimeDirectory, `grok-desktop-${suffix}`);
    this.endpoint = this.platform === "win32"
      ? `\\\\.\\pipe\\grok-desktop-daemon-${suffix}`
      : path.join(this.runtimePath, "daemon.sock");
  }

  getStatus(): DaemonStatus {
    return { ...this.status };
  }

  subscribe(listener: (status: DaemonStatus) => void): () => void {
    this.listeners.add(listener);
    listener(this.getStatus());
    return () => this.listeners.delete(listener);
  }

  async start(): Promise<void> {
    // A request initiated through the ordinary supervisor API is the explicit
    // manual retry advertised after the automatic restart budget is exhausted.
    if (this.unexpectedRestartTimer) clearTimeout(this.unexpectedRestartTimer);
    this.unexpectedRestartTimer = undefined;
    this.unexpectedRestartRequiresManual = false;
    return this.startShared();
  }

  private async startAutomatically(): Promise<void> {
    if (this.unexpectedRestartRequiresManual) {
      throw new DaemonProtocolError("daemon automatic restart requires a manual retry");
    }
    if (this.unexpectedRestartTimer) {
      throw new DaemonProtocolError("daemon automatic restart backoff is active");
    }
    try {
      return await this.startShared();
    } catch (error) {
      this.scheduleUnexpectedRestart();
      throw error;
    }
  }

  private async startShared(): Promise<void> {
    if (this.stopping) throw new DaemonProtocolError("daemon supervisor is stopping");
    if (this.ready) return;
    if (this.startPromise) return this.startPromise;
    this.startPromise = this.startInternal()
      .catch((error) => {
        if (!this.stopping && this.status.state === "starting") {
          this.setDegraded("The local daemon could not be started.");
        }
        throw error;
      })
      .finally(() => {
        this.startPromise = undefined;
      });
    return this.startPromise;
  }

  async bootstrap(): Promise<{
    status: DaemonStatus;
    capabilities: DaemonCapabilityStatus[];
    workExecutionMode: import("../../src/contracts/bridge.js").DaemonWorkExecutionMode;
    hostWorkRuntimeReady: boolean;
    hostBoundRunActive: boolean;
    hostWork: DaemonHostWorkSnapshot[];
    accountState: DaemonAccountState;
    workspace: DaemonWorkspaceSnapshot;
  }> {
    await this.start();
    const protocol = this.requireProtocol();
    try {
      const [capabilities, hostWork, accountState, workspace] = await Promise.all([
        protocol.getCapabilitySnapshot(),
        protocol.listHostWorkRuns(),
        protocol.getAccountState(),
        loadWorkspace(protocol),
      ]);
      this.setConnected();
      return {
        status: this.getStatus(),
        capabilities: capabilities.statuses.map(mapCapability),
        workExecutionMode: workExecutionModeFromWire(capabilities.workExecutionBackend),
        hostWorkRuntimeReady: capabilities.hostWorkRuntimeReady,
        hostBoundRunActive: capabilities.hostBoundRunActive,
        hostWork: hostWork.items.map(mapHostWorkSnapshot),
        accountState: mapAccountState(accountState),
        workspace,
      };
    } catch (error) {
      this.setDegraded("The daemon could not resolve current capabilities.");
      throw error;
    }
  }

  async getHostExecutionPolicy(): Promise<DaemonHostExecutionPolicy> {
    await this.start();
    return mapHostExecutionPolicy(await this.requireProtocol().getHostExecutionPolicy());
  }

  async enrollHostExecution(
    input: {
      expectedRevision: number;
      acknowledgmentVersion: number;
      typedAcknowledgment: string;
      filesystemRead: boolean;
      filesystemWrite: boolean;
      processExecute: boolean;
      pathRoots: string[];
      broadScopeAcknowledged: boolean;
    },
    idempotencyKey: string,
  ): Promise<DaemonHostExecutionPolicy> {
    await this.start();
    return mapHostExecutionPolicy(await this.requireProtocol().enrollHostExecution({
      ...input,
      expectedRevision: BigInt(input.expectedRevision),
    }, idempotencyKey));
  }

  async revokeHostExecution(expectedRevision: number, idempotencyKey: string): Promise<DaemonHostExecutionPolicy> {
    await this.start();
    return mapHostExecutionPolicy(await this.requireProtocol().revokeHostExecution(
      BigInt(expectedRevision), idempotencyKey,
    ));
  }

  async prepareHostWorkRuntime(idempotencyKey: string): Promise<DaemonHostExecutionPolicy> {
    await this.start();
    return mapHostExecutionPolicy(await this.requireProtocol().prepareHostWorkRuntime(idempotencyKey));
  }

  async deactivateHostWorkRuntime(idempotencyKey: string): Promise<DaemonHostExecutionPolicy> {
    await this.start();
    return mapHostExecutionPolicy(await this.requireProtocol().deactivateHostWorkRuntime(idempotencyKey));
  }

  async startHostWork(
    projectId: string,
    threadId: string,
    prompt: string,
    idempotencyKey: string,
  ): Promise<DaemonHostWorkSnapshot> {
    await this.start();
    const result = await this.requireProtocol().startHostWork(projectId, threadId, prompt, idempotencyKey);
    if (!result.run) throw new DaemonProtocolError("Host Work response is missing its run");
    return { run: mapRun(result.run) };
  }

  async cancelHostWork(runId: string, idempotencyKey: string): Promise<DaemonHostWorkSnapshot> {
    await this.start();
    const result = await this.requireProtocol().cancelHostWork(runId, idempotencyKey);
    if (!result.run) throw new DaemonProtocolError("Host Work cancel response is missing its run");
    return { run: mapRun(result.run) };
  }

  async listHostWorkRuns(limit: number): Promise<DaemonHostWorkSnapshot[]> {
    await this.start();
    return (await this.requireProtocol().listHostWorkRuns(limit)).items.map(mapHostWorkSnapshot);
  }

  async getAccountState(): Promise<DaemonAccountState> {
    await this.start();
    const state = await this.requireProtocol().getAccountState();
    this.setConnected();
    return mapAccountState(state);
  }

  async startGrokBuildAuth(idempotencyKey: string): Promise<{ state: string; authenticated: boolean }> {
    await this.start();
    const status = await this.requireProtocol().startGrokBuildAuth(idempotencyKey);
    this.setConnected();
    return {
      state: String(status.state ?? "not_authenticated"),
      authenticated: status.authenticated === true,
    };
  }

  async getGrokBuildAuthStatus(): Promise<{ state: string; authenticated: boolean }> {
    await this.start();
    const status = await this.requireProtocol().getGrokBuildAuthStatus();
    this.setConnected();
    return {
      state: String(status.state ?? "not_authenticated"),
      authenticated: status.authenticated === true,
    };
  }

  async getManagedIntegration(integrationId: string): Promise<{
    id: string;
    state: string;
    installedVersion: string;
    availableVersion: string;
    rollbackVersion: string;
    revision: number;
    signatureVerified: boolean;
  }> {
    await this.start();
    const integration = await this.requireProtocol().getManagedIntegration(integrationId);
    this.setConnected();
    return mapManagedIntegration(integration);
  }

  async changeManagedIntegration(
    integrationId: string,
    action: string,
    expectedRevision: number,
    idempotencyKey: string,
  ): Promise<{
    id: string;
    state: string;
    installedVersion: string;
    availableVersion: string;
    rollbackVersion: string;
    revision: number;
    signatureVerified: boolean;
  }> {
    await this.start();
    const integration = await this.requireProtocol().changeManagedIntegration(
      integrationId,
      action,
      BigInt(expectedRevision),
      idempotencyKey,
    );
    this.setConnected();
    return mapManagedIntegration(integration);
  }

  async getDesktopPreferences(): Promise<DaemonDesktopPreferences> {
    await this.start();
    const preferences = await this.requireProtocol().getDesktopPreferences();
    this.setConnected();
    return mapDesktopPreferences(preferences);
  }

  async updateDesktopPreferences(
    expectedRevision: number,
    keepRunningInNotificationArea: boolean,
    idempotencyKey: string,
  ): Promise<DaemonDesktopPreferences> {
    await this.start();
    const preferences = await this.requireProtocol().updateDesktopPreferences(
      BigInt(expectedRevision),
      keepRunningInNotificationArea,
      idempotencyKey,
    );
    this.setConnected();
    return mapDesktopPreferences(preferences);
  }

  async getChatModelCatalog(): Promise<DaemonChatModelCatalog> {
    await this.start();
    const catalog = await this.requireProtocol().getChatModelCatalog();
    this.setConnected();
    return mapChatModelCatalog(catalog);
  }

  async getUsageSummary(
    scopeKind: DaemonUsageScopeKind,
    scopeId: string | undefined,
    window: DaemonUsageWindow,
  ): Promise<DaemonUsageSummary> {
    await this.start();
    const summary = await this.requireProtocol().getUsageSummary(
      scopeKind,
      scopeId ?? "",
      window,
    );
    this.setConnected();
    return mapUsageSummary(summary);
  }

  async selectChatModel(
    expectedRevision: number,
    modelId: string,
    idempotencyKey: string,
  ): Promise<DaemonChatModelPreference> {
    await this.start();
    const preference = await this.requireProtocol().selectChatModel(
      BigInt(expectedRevision),
      modelId,
      idempotencyKey,
    );
    this.setConnected();
    return mapChatModelPreference(preference);
  }

  async enrollXaiApiKey(parentWindowToken: bigint, idempotencyKey: string): Promise<DaemonAccountState> {
    await this.start();
    const state = await this.requireProtocol().enrollXaiApiKey(parentWindowToken, idempotencyKey);
    this.setConnected();
    return mapAccountState(state);
  }

  async beginSuperGrokDeviceEnrollment(idempotencyKey: string): Promise<DaemonSuperGrokEnrollmentStatus> {
    await this.start();
    return mapSuperGrokEnrollmentStatus(
      await this.requireProtocol().beginSuperGrokDeviceEnrollment(idempotencyKey),
    );
  }

  async getSuperGrokEnrollmentStatus(): Promise<DaemonSuperGrokEnrollmentStatus> {
    await this.start();
    return mapSuperGrokEnrollmentStatus(await this.requireProtocol().getSuperGrokEnrollmentStatus());
  }

  async cancelSuperGrokEnrollment(idempotencyKey: string): Promise<DaemonSuperGrokEnrollmentStatus> {
    await this.start();
    return mapSuperGrokEnrollmentStatus(
      await this.requireProtocol().cancelSuperGrokEnrollment(idempotencyKey),
    );
  }

  async disconnectSuperGrok(idempotencyKey: string): Promise<DaemonSuperGrokEnrollmentStatus> {
    await this.start();
    return mapSuperGrokEnrollmentStatus(await this.requireProtocol().disconnectSuperGrok(idempotencyKey));
  }

  async deleteXaiApiKey(idempotencyKey: string): Promise<DaemonAccountState> {
    await this.start();
    const state = await this.requireProtocol().deleteXaiApiKey(idempotencyKey);
    this.setConnected();
    return mapAccountState(state);
  }

  async createProject(name: string, description: string, idempotencyKey: string): Promise<DaemonProject> {
    await this.start();
    const project = await this.requireProtocol().createProject(name, description, idempotencyKey);
    this.setConnected();
    return mapProject(project);
  }

  async importArtifact(
    projectId: string,
    displayName: string,
    mediaType: string,
    sourcePath: string,
    idempotencyKey: string,
  ): Promise<DaemonArtifact> {
    await this.start();
    const result = await this.requireProtocol().importArtifact(
      projectId,
      displayName,
      mediaType,
      sourcePath,
      idempotencyKey,
    );
    const artifact = mapImportedArtifactOperation(result, projectId, displayName, mediaType);
    this.setConnected();
    return artifact;
  }

  async openArtifact(
    artifactId: string,
    contentVersion: number,
    idempotencyKey: string,
  ): Promise<DaemonArtifactOpenReceipt> {
    await this.start();
    const result = await this.requireProtocol().openArtifact(
      artifactId,
      contentVersion,
      idempotencyKey,
    );
    const receipt = mapArtifactOpenOperation(result, artifactId, contentVersion);
    this.setConnected();
    return receipt;
  }

  async removeArtifact(
    artifactId: string,
    expectedRevision: number,
    expectedContentVersion: number,
    idempotencyKey: string,
  ): Promise<DaemonArtifactRemovalOutcome> {
    await this.start();
    const result = await this.requireProtocol().removeArtifact(
      artifactId,
      BigInt(expectedRevision),
      expectedContentVersion,
      idempotencyKey,
    );
    const outcome = mapArtifactRemovalOperation(
      result,
      artifactId,
      expectedRevision,
      expectedContentVersion,
    );
    this.setConnected();
    return outcome;
  }

  async searchWorkspace(
    projectId: string | undefined,
    query: string,
    offset: number,
    limit: number,
  ): Promise<DaemonWorkspaceSearchResults> {
    await this.start();
    const results = await this.requireProtocol().searchWorkspace(projectId ?? "", query, offset, limit);
    this.setConnected();
    return mapWorkspaceSearchResults(results, offset, limit);
  }

  async createThread(projectId: string, title: string, idempotencyKey: string): Promise<DaemonThread> {
    await this.start();
    const thread = await this.requireProtocol().createThread(projectId, title, idempotencyKey);
    this.setConnected();
    return mapThread(thread);
  }

  async getConversation(threadId: string): Promise<{
    thread: DaemonThread;
    messages: DaemonMessage[];
    turns: DaemonConversationTurn[];
    forkMetadata: DaemonConversationForkMetadata;
    workRun?: DaemonRun;
  }> {
    await this.start();
    const protocol = this.requireProtocol();
    let lastInconsistency: DaemonProtocolError | undefined;
    for (let attempt = 0; attempt < 3; attempt += 1) {
      const [thread, messages, turns, forkMetadata, workRuns] = await Promise.all([
        protocol.getThread(threadId),
        collectConversationMessages(protocol, threadId),
        collectConversationTurns(protocol, threadId),
        protocol.getConversationForkMetadata(threadId),
        protocol.listHostWorkRuns(2, threadId),
      ]);
      const mappedThread = mapThread(thread);
      const mappedMessages = messages.map(mapMessage);
      const mappedTurns = turns.map(mapConversationTurn);
      const mappedForkMetadata = mapConversationForkMetadata(forkMetadata, threadId);
      const mappedWorkRuns = workRuns.items.map(mapHostWorkSnapshot);
      if (mappedWorkRuns.length > 1) {
        throw new DaemonProtocolError("daemon Work conversation has multiple owning runs");
      }
      const workRun = mappedWorkRuns[0]?.run;
      try {
        validateConversationAggregate(
          mappedThread,
          mappedMessages,
          mappedTurns,
          mappedForkMetadata,
          workRun,
        );
        this.setConnected();
        return {
          thread: mappedThread,
          messages: mappedMessages,
          turns: mappedTurns,
          forkMetadata: mappedForkMetadata,
          ...(workRun ? { workRun } : {}),
        };
      } catch (error) {
        if (!(error instanceof DaemonProtocolError)) throw error;
        lastInconsistency = error;
        if (attempt < 2) await delay(25);
      }
    }
    throw lastInconsistency ?? new DaemonProtocolError("daemon conversation snapshot is inconsistent");
  }

  async startConversationTurn(
    threadId: string,
    content: string,
    idempotencyKey: string,
    modelId?: string,
    searchEnabled = false,
  ): Promise<DaemonConversationTurn> {
    await this.start();
    const turn = await this.requireProtocol().startConversationTurn(
      threadId,
      content,
      idempotencyKey,
      modelId,
      searchEnabled,
    );
    this.setConnected();
    return mapConversationTurn(turn);
  }

  async cancelConversationTurn(
    turnId: string,
    expectedRevision: number,
    idempotencyKey: string,
  ): Promise<DaemonConversationTurn> {
    await this.start();
    const turn = await this.requireProtocol().cancelConversationTurn(
      turnId,
      BigInt(expectedRevision),
      idempotencyKey,
    );
    this.setConnected();
    return mapConversationTurn(turn);
  }

  async retryConversationTurn(
    sourceTurnId: string,
    expectedRevision: number,
    idempotencyKey: string,
  ): Promise<DaemonConversationTurn> {
    await this.start();
    const rawTurn = await this.requireProtocol().retryConversationTurn(
      sourceTurnId,
      BigInt(expectedRevision),
      idempotencyKey,
    );
    const turn = mapRetryConversationTurnResponse(rawTurn, sourceTurnId);
    this.setConnected();
    return turn;
  }

  async branchConversationThread(
    sourceTurnId: string,
    expectedRevision: number,
    idempotencyKey: string,
  ): Promise<DaemonConversationFork> {
    await this.start();
    const fork = await this.requireProtocol().branchConversationThread(
      sourceTurnId,
      BigInt(expectedRevision),
      idempotencyKey,
    );
    const mapped = mapConversationFork(fork, "branch", sourceTurnId);
    this.setConnected();
    return mapped;
  }

  async editAndBranchConversationTurn(
    sourceTurnId: string,
    expectedRevision: number,
    content: string,
    idempotencyKey: string,
  ): Promise<DaemonConversationFork> {
    await this.start();
    const fork = await this.requireProtocol().editAndBranchConversationTurn(
      sourceTurnId,
      BigInt(expectedRevision),
      content,
      idempotencyKey,
    );
    const mapped = mapConversationFork(fork, "edit_and_branch", sourceTurnId);
    this.setConnected();
    return mapped;
  }

  async regenerateConversationTurn(
    sourceTurnId: string,
    expectedRevision: number,
    idempotencyKey: string,
  ): Promise<DaemonConversationFork> {
    await this.start();
    const fork = await this.requireProtocol().regenerateConversationTurn(
      sourceTurnId,
      BigInt(expectedRevision),
      idempotencyKey,
    );
    const mapped = mapConversationFork(fork, "regenerate", sourceTurnId);
    this.setConnected();
    return mapped;
  }

  async getConversationForkMetadata(
    threadId: string,
  ): Promise<DaemonConversationForkMetadata> {
    await this.start();
    const metadata = await this.requireProtocol().getConversationForkMetadata(threadId);
    const mapped = mapConversationForkMetadata(metadata, threadId);
    this.setConnected();
    return mapped;
  }

  async acknowledgeConversationForkDelivery(
    childThreadId: string,
    expectedRevision: number,
    idempotencyKey: string,
  ): Promise<DaemonConversationForkDelivery> {
    if (expectedRevision !== 0) {
      throw new DaemonProtocolError("conversation fork delivery acknowledgement requires revision zero");
    }
    await this.start();
    const delivery = await this.requireProtocol().acknowledgeConversationForkDelivery(
      childThreadId,
      BigInt(expectedRevision),
      idempotencyKey,
    );
    const mapped = mapConversationForkDelivery(delivery, childThreadId);
    if (mapped.state !== "acknowledged" || mapped.revision !== expectedRevision + 1) {
      throw new DaemonProtocolError("daemon conversation fork delivery acknowledgement is invalid");
    }
    this.setConnected();
    return mapped;
  }

  async createAutomation(input: DaemonAutomationInput, idempotencyKey: string): Promise<DaemonAutomation> {
    await this.start();
    const automation = await this.requireProtocol().createAutomation(automationToWire(input), idempotencyKey);
    this.setConnected();
    return mapAutomation(automation);
  }

  async updateAutomation(
    automationId: string,
    expectedRevision: number,
    input: DaemonAutomationInput,
    idempotencyKey: string,
  ): Promise<DaemonAutomation> {
    await this.start();
    const automation = await this.requireProtocol().updateAutomation({
      automationId,
      expectedRevision: BigInt(expectedRevision),
      ...automationToWire(input),
    }, idempotencyKey);
    this.setConnected();
    return mapAutomation(automation);
  }

  /**
   * Starts a bounded, read-only run-event channel on its own local connection.
   * The returned disposer aborts an in-flight long poll and closes its socket.
   */
  async subscribeRunEvents(
    runId: string,
    afterSequence: number,
    listener: (batch: DaemonRunEventBatch) => void | Promise<void>,
    onError?: (error: Error) => void,
  ): Promise<() => void> {
    if (!Number.isSafeInteger(afterSequence) || afterSequence < 0) {
      throw new DaemonProtocolError("invalid run event cursor");
    }
    if (this.runEventSubscriptions.size >= MAX_RUN_EVENT_SUBSCRIPTIONS) {
      throw new DaemonProtocolError("too many run event subscriptions");
    }
    await this.startAutomatically();
    if (this.stopping) throw new DaemonProtocolError("daemon supervisor is stopping");
    if (this.runEventSubscriptions.size >= MAX_RUN_EVENT_SUBSCRIPTIONS) {
      throw new DaemonProtocolError("too many run event subscriptions");
    }
    const protocol = new DaemonProtocolClient(new DaemonRpcClient({
      nonce: this.nonce,
      connect: () => connectEndpoint(this.endpoint),
      maxPendingRequests: 1,
    }));
    let subscription: DaemonRunEventSubscription;
    try {
      subscription = new DaemonRunEventSubscription(protocol, {
        runId,
        afterSequence: BigInt(afterSequence),
        listener: async (batch) => listener({
          events: batch.events.map(mapRunEvent),
          nextSequence: safeNumber(batch.nextSequence, "next run event cursor"),
          hasMore: batch.hasMore,
        }),
        onError,
      });
    } catch (error) {
      protocol.close();
      throw error;
    }
    this.runEventSubscriptions.add(subscription);
    void subscription.start().finally(() => {
      this.runEventSubscriptions.delete(subscription);
    });
    return () => {
      subscription.close();
      this.runEventSubscriptions.delete(subscription);
    };
  }

  /** Starts a replay-from-zero durable Chat event channel on its own connection. */
  async subscribeConversationTurnEvents(
    turnId: string,
    listener: (batch: DaemonConversationTurnEventBatch) => void | Promise<void>,
    onError?: (error: Error) => void,
  ): Promise<() => void> {
    const activeSubscriptions = this.runEventSubscriptions.size + this.conversationEventSubscriptions.size;
    if (activeSubscriptions >= MAX_RUN_EVENT_SUBSCRIPTIONS) {
      throw new DaemonProtocolError("too many daemon event subscriptions");
    }
    await this.startAutomatically();
    if (this.stopping) throw new DaemonProtocolError("daemon supervisor is stopping");
    if (this.runEventSubscriptions.size + this.conversationEventSubscriptions.size >= MAX_RUN_EVENT_SUBSCRIPTIONS) {
      throw new DaemonProtocolError("too many daemon event subscriptions");
    }
    const protocol = new DaemonProtocolClient(new DaemonRpcClient({
      nonce: this.nonce,
      connect: () => connectEndpoint(this.endpoint),
      maxPendingRequests: 1,
    }));
    let subscription: DaemonConversationTurnEventSubscription;
    try {
      subscription = new DaemonConversationTurnEventSubscription(protocol, {
        turnId,
        listener: async (batch) => listener({
          events: batch.events.map(mapConversationTurnEvent),
          nextSequence: safeNumber(batch.nextSequence, "next conversation event cursor"),
          hasMore: batch.hasMore,
        }),
        onError,
      });
    } catch (error) {
      protocol.close();
      throw error;
    }
    this.conversationEventSubscriptions.add(subscription);
    void subscription.start().finally(() => {
      this.conversationEventSubscriptions.delete(subscription);
    });
    return () => {
      subscription.close();
      this.conversationEventSubscriptions.delete(subscription);
    };
  }

  async decideApproval(
    approvalId: string,
    expectedRevision: number,
    approved: boolean,
    idempotencyKey: string,
  ): Promise<DaemonApproval> {
    await this.start();
    const approval = await this.requireProtocol().decideApproval(
      approvalId,
      BigInt(expectedRevision),
      approved ? ApprovalDecision.APPROVAL_DECISION_GRANT : ApprovalDecision.APPROVAL_DECISION_DENY,
      idempotencyKey,
    );
    this.setConnected();
    return mapApprovalDecisionResponse(approval, approvalId, expectedRevision, approved);
  }

  async stop(): Promise<void> {
    if (this.stopping) return;
    this.stopping = true;
    this.ready = false;
    if (this.unexpectedRestartTimer) clearTimeout(this.unexpectedRestartTimer);
    this.unexpectedRestartTimer = undefined;
    this.unexpectedRestartTimes = [];
    this.unexpectedRestartRequiresManual = false;
    for (const subscription of this.runEventSubscriptions) subscription.close();
    this.runEventSubscriptions.clear();
    for (const subscription of this.conversationEventSubscriptions) subscription.close();
    this.conversationEventSubscriptions.clear();
    this.rpcGeneration += 1;
    this.protocol?.close();
    this.protocol = undefined;
    const child = this.child;
    this.child = undefined;
    if (child && child.exitCode === null && child.signalCode === null) {
      await stopChild(child);
    }
    if (this.platform !== "win32") {
      rmSync(this.runtimePath, { recursive: true, force: true });
    }
    this.setStatus({ state: "stopped", updatedAtUnixMs: Date.now() });
  }

  private async startInternal(): Promise<void> {
    this.setStatus({ state: "starting", updatedAtUnixMs: Date.now() });
    if (this.platform !== "win32") mkdirSync(this.runtimePath, { recursive: true, mode: 0o700 });
    const executable = resolveDaemonBinary(this.options, this.platform);
    const startingChild = this.spawnDaemon(executable);
    const rpcGeneration = this.rpcGeneration + 1;
    this.rpcGeneration = rpcGeneration;
    const rpc = new DaemonRpcClient({
      nonce: this.nonce,
      connect: () => connectEndpoint(this.endpoint),
      requestTimeoutMs: 1_500,
      onConnectionState: (state, error) => {
        if (this.rpcGeneration === rpcGeneration) this.onRpcState(state, error);
      },
    });
    const startingProtocol = new DaemonProtocolClient(rpc);
    this.protocol = startingProtocol;
    const deadline = Date.now() + STARTUP_TIMEOUT_MS;
    let lastError: unknown;
    while (!this.stopping && Date.now() < deadline) {
      if (!this.ownsLiveStartup(startingChild, startingProtocol)) break;
      try {
        const health = await startingProtocol.health();
        if (health.protocolVersion !== PROTOCOL_VERSION) {
          throw new DaemonProtocolError(
            `daemon reported protocol ${health.protocolVersion}, supervisor requires ${PROTOCOL_VERSION}`,
          );
        }
        // The child exit callback can run while health() is resolving. Never
        // publish a ready supervisor whose child/protocol ownership was already
        // cleared by that callback; otherwise every later request sees
        // ready=true with no usable protocol and cannot initiate a restart.
        if (!this.ownsLiveStartup(startingChild, startingProtocol)) {
          lastError = new DaemonProtocolError("daemon exited while startup health was being confirmed");
          break;
        }
        this.ready = true;
        this.setStatus({
          state: "connected",
          serviceVersion: health.serviceVersion,
          protocolVersion: health.protocolVersion,
          instanceId: health.instanceId,
          agentRuntime: mapAgentRuntime(health.agentRuntime),
          automationScheduler: mapAutomationSchedulerHealth(health.automationScheduler),
          updatedAtUnixMs: Date.now(),
        });
        return;
      } catch (error) {
        lastError = error;
        await delay(RETRY_DELAY_MS);
      }
    }
    this.ready = false;
    if (this.protocol === startingProtocol) {
      this.rpcGeneration += 1;
      this.protocol.close();
      this.protocol = undefined;
    }
    const failedChild = this.child;
    this.child = undefined;
    const didNotBecomeReady = failedChild?.exitCode === null && failedChild.signalCode === null;
    // A daemon left running after a failed startup would keep the database
    // lock and turn every retry into an instant DatabaseInUse failure.
    if (failedChild && failedChild.exitCode === null && failedChild.signalCode === null) {
      await stopChild(failedChild);
    }
    if (!this.stopping) {
      this.setDegraded(didNotBecomeReady
        ? "The local daemon did not become ready."
        : "The local daemon exited during startup.");
    }
    const causeMessage = lastError instanceof Error ? lastError.message : lastError ? String(lastError) : "startup timed out";
    throw new Error(`daemon startup failed: ${causeMessage}`, { cause: lastError });
  }

  private spawnDaemon(executable: string): ChildProcess {
    const child = spawn(executable, [], {
      // Packaged appPath points at app.asar, which is a file. The verified
      // daemon directory is a real, fixed working directory on every platform.
      cwd: path.dirname(executable),
      env: daemonEnvironment(
        this.endpoint,
        this.platform,
        this.options.allowDevelopmentBinary,
      ),
      shell: false,
      windowsHide: true,
      stdio: ["pipe", "ignore", this.options.inheritDaemonStderr ? "inherit" : "ignore"],
    });
    const bootstrap = child.stdin;
    if (!bootstrap) {
      child.kill();
      throw new Error("daemon bootstrap channel is unavailable");
    }
    // The nonce is an IPC bearer value. A one-shot pipe keeps it out of argv,
    // the daemon environment, and /proc/<pid>/environ. Ignore EPIPE here: the
    // ordinary startup path observes and reports an early child exit without
    // reproducing bootstrap bytes in diagnostics.
    bootstrap.once("error", () => undefined);
    bootstrap.end(daemonBootstrapInput(this.nonce));
    this.child = child;
    child.once("exit", () => {
      if (child !== this.child) return;
      const shouldRestart = this.ready && !this.stopping;
      this.child = undefined;
      this.ready = false;
      // Dedicated durable-event clients retain their acknowledged cursors and
      // retry the socket. The next ordinary supervisor request restarts the
      // daemon; closing them here would leave Electron main holding a stale
      // watch that can no longer receive events after that restart.
      this.rpcGeneration += 1;
      this.protocol?.close();
      this.protocol = undefined;
      if (!this.stopping) {
        this.setDegraded("The local daemon stopped unexpectedly.");
        if (shouldRestart) this.scheduleUnexpectedRestart();
      }
    });
    child.once("error", () => {
      if (!this.stopping) this.setDegraded("The local daemon could not be started.");
    });
    return child;
  }

  private ownsLiveStartup(child: ChildProcess, protocol: DaemonProtocolClient): boolean {
    return !this.stopping
      && this.child === child
      && child.exitCode === null
      && child.signalCode === null
      && this.protocol === protocol;
  }

  private scheduleUnexpectedRestart(): void {
    if (this.stopping || this.ready || this.unexpectedRestartTimer) return;
    const now = Date.now();
    this.unexpectedRestartTimes = this.unexpectedRestartTimes.filter((time) => now - time < 60_000);
    if (this.unexpectedRestartTimes.length >= MAX_UNEXPECTED_RESTARTS_PER_MINUTE) {
      this.unexpectedRestartRequiresManual = true;
      this.setDegraded("The local daemon stopped repeatedly and requires a manual retry.");
      return;
    }
    this.unexpectedRestartTimes.push(now);
    const delayMs = 250 * (2 ** (this.unexpectedRestartTimes.length - 1));
    this.unexpectedRestartTimer = setTimeout(() => {
      this.unexpectedRestartTimer = undefined;
      if (this.stopping || this.ready) return;
      void this.startAutomatically().catch(() => undefined);
    }, delayMs);
  }

  private onRpcState(state: RpcConnectionState, _error?: Error): void {
    if (state === "connected" && this.ready) this.setConnected();
    if (state === "disconnected" && this.ready && !this.stopping) {
      this.setDegraded("The local daemon connection was interrupted. Reconnecting on the next request.");
    }
  }

  private setConnected(): void {
    this.setStatus({ ...this.status, state: "connected", reason: undefined, updatedAtUnixMs: Date.now() });
  }

  private setDegraded(reason: string): void {
    this.setStatus({ ...this.status, state: "degraded", reason, updatedAtUnixMs: Date.now() });
  }

  private setStatus(status: DaemonStatus): void {
    this.status = status;
    for (const listener of this.listeners) listener(this.getStatus());
  }

  private requireProtocol(): DaemonProtocolClient {
    if (!this.protocol) throw new Error("daemon protocol client is unavailable");
    return this.protocol;
  }
}

export function resolveDaemonBinary(options: DaemonSupervisorOptions, platform: NodeJS.Platform): string {
  const executable = platform === "win32" ? "grok-daemon.exe" : "grok-daemon";
  const packagedBinary = path.join(options.resourcesPath, "bin", executable);
  const candidates = options.allowDevelopmentBinary
    ? [
        options.daemonBinary,
        process.env.GROK_DAEMON_BINARY,
        packagedBinary,
        path.resolve(options.appPath, "../..", "target", "debug", executable),
        path.resolve(options.appPath, "../..", "target", "release", executable),
      ]
    : [packagedBinary];
  const match = candidates
    .filter((candidate): candidate is string => Boolean(candidate))
    .find(existsSync);
  if (!match) throw new Error("grok-daemon binary is not available");
  return match;
}

export function daemonEnvironment(
  endpoint: string,
  platform: NodeJS.Platform,
  allowDevelopmentOverrides = false,
): NodeJS.ProcessEnv {
  const safeKeys = [
    "PATH", "Path", "SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR", "HOME", "USERPROFILE",
    "LOCALAPPDATA", "APPDATA", "XDG_RUNTIME_DIR", "RUST_LOG", "RUST_BACKTRACE",
    // The daemon renders its own native prompts (pinentry on unix); display
    // session variables are not secrets and are required for that boundary.
    "WAYLAND_DISPLAY", "DISPLAY", "XAUTHORITY", "XDG_SESSION_TYPE",
  ];
  const environment: NodeJS.ProcessEnv = {};
  for (const key of safeKeys) {
    if (process.env[key] !== undefined) environment[key] = process.env[key];
  }
  const pinentryOverride = process.env.GROK_PINENTRY;
  // GROK_PINENTRY is an operator/development escape hatch, not packaged
  // configuration. The explicit development-binary gate is derived from
  // !app.isPackaged by Electron main, so production launches always strip it.
  // Keep the value absolute so Command cannot reinterpret it through PATH.
  if (
    allowDevelopmentOverrides
    && platform !== "win32"
    && validDevelopmentExecutableOverride(pinentryOverride, platform)
  ) {
    environment.GROK_PINENTRY = pinentryOverride;
  }
  // Development-only official Grok Build ACP descriptor. Packaged launches
  // never receive these variables; release daemons reject them without the
  // debug-acp-descriptor feature and signed catalog path remains required for
  // production components.
  if (allowDevelopmentOverrides) {
    if (process.env.GROK_DAEMON_EPHEMERAL === "1") {
      environment.GROK_DAEMON_EPHEMERAL = "1";
    }
    const installationId = process.env.GROK_INSTALLATION_ID;
    if (installationId && /^[A-Za-z0-9_-]{1,64}$/.test(installationId)) {
      environment.GROK_INSTALLATION_ID = installationId;
    }
    const acp = resolveDevelopmentAcpDescriptor({
      platform,
      env: process.env,
    });
    if (acp) applyDevelopmentAcpDescriptor(environment, acp);
  }
  environment.GROK_DAEMON_STARTUP_NONCE_STDIN = "1";
  if (platform === "win32") environment.GROK_DAEMON_PIPE = endpoint;
  else environment.GROK_DAEMON_SOCKET = endpoint;
  return environment;
}

export function daemonBootstrapInput(nonce: Buffer): Buffer {
  if (nonce.length !== DAEMON_STARTUP_NONCE_BYTES) {
    throw new Error("daemon startup nonce must contain exactly 32 bytes");
  }
  return Buffer.from(nonce);
}

function validDevelopmentExecutableOverride(
  value: string | undefined,
  platform: NodeJS.Platform,
): value is string {
  if (!value || Buffer.byteLength(value, "utf8") > 4_096) return false;
  const platformPath = platform === "win32" ? path.win32 : path.posix;
  return platformPath.isAbsolute(value) && !Array.from(value).some((character) => {
    const point = character.codePointAt(0) ?? 0;
    return point <= 0x1f || (point >= 0x7f && point <= 0x9f);
  });
}

function connectEndpoint(endpoint: string): Promise<net.Socket> {
  return new Promise((resolve, reject) => {
    const socket = net.createConnection(endpoint);
    const timeout = setTimeout(() => {
      cleanup();
      socket.destroy();
      reject(new Error("daemon connection attempt timed out"));
    }, CONNECT_ATTEMPT_TIMEOUT_MS);
    const cleanup = () => {
      clearTimeout(timeout);
      socket.off("connect", connected);
      socket.off("error", failed);
    };
    const connected = () => {
      cleanup();
      socket.setNoDelay(true);
      resolve(socket);
    };
    const failed = (error: Error) => {
      cleanup();
      socket.destroy();
      reject(error);
    };
    socket.once("connect", connected);
    socket.once("error", failed);
  });
}

async function loadWorkspace(protocol: DaemonProtocolClient): Promise<DaemonWorkspaceSnapshot> {
  const projects = await collectPages(
    (cursor) => protocol.listProjects(cursor, WORKSPACE_PAGE_SIZE),
    (page) => page.projects,
    MAX_PROJECTS,
  );
  const threads: DaemonWorkspaceSnapshot["threads"] = [];
  const artifacts: DaemonWorkspaceSnapshot["artifacts"] = [];
  const automations: DaemonWorkspaceSnapshot["automations"] = [];
  for (const project of projects) {
    let remaining = MAX_WORKSPACE_ENTITIES - projects.length - threads.length - artifacts.length - automations.length;
    const projectThreads = await collectPages(
      (cursor) => protocol.listThreads(project.id, cursor, WORKSPACE_PAGE_SIZE),
      (page) => page.threads,
      remaining,
    );
    threads.push(...projectThreads.map(mapThread));
    remaining -= projectThreads.length;
    const projectArtifacts = await collectPages(
      (cursor) => protocol.listArtifacts(project.id, cursor, WORKSPACE_PAGE_SIZE),
      (page) => page.artifacts,
      remaining,
    );
    artifacts.push(...projectArtifacts.map(mapArtifact));
    remaining -= projectArtifacts.length;
    const projectAutomations = await collectPages(
      (cursor) => protocol.listAutomations(project.id, cursor, WORKSPACE_PAGE_SIZE),
      (page) => page.automations,
      remaining,
    );
    automations.push(...projectAutomations.map((automation) => (
      mapListedAutomation(automation, project.id)
    )));
  }
  return {
    projects: projects.map(mapProject),
    threads,
    messages: [],
    artifacts,
    automations,
  };
}

async function collectPages<P extends { nextCursor: string }, T>(
  fetchPage: (cursor: string) => Promise<P>,
  items: (page: P) => T[],
  maximum = MAX_WORKSPACE_ENTITIES,
): Promise<T[]> {
  const collected: T[] = [];
  const seen = new Set<string>();
  let cursor = "";
  do {
    const page = await fetchPage(cursor);
    const pageItems = items(page);
    if (collected.length + pageItems.length > maximum) {
      throw new DaemonProtocolError("daemon workspace response exceeded the entity limit");
    }
    collected.push(...pageItems);
    const next = page.nextCursor;
    if (!next) return collected;
    if (seen.has(next)) throw new DaemonProtocolError("daemon workspace cursor repeated");
    seen.add(next);
    cursor = boundedString(next, "workspace cursor", 512);
  } while (cursor);
  return collected;
}

async function collectConversationMessages(
  protocol: DaemonProtocolClient,
  threadId: string,
): Promise<import("../generated/daemon/v1/daemon.js").Message[]> {
  const messages: import("../generated/daemon/v1/daemon.js").Message[] = [];
  const messageIds = new Set<string>();
  const seen = new Set<string>();
  let contentBytes = 0;
  let cursor = "";
  do {
    const page = await protocol.listMessages(threadId, cursor, CONVERSATION_PAGE_SIZE);
    for (const message of page.messages) {
      if (messageIds.has(message.id)) {
        throw new DaemonProtocolError("daemon conversation repeated a message");
      }
      messageIds.add(message.id);
      contentBytes += Buffer.byteLength(message.content, "utf8");
      if (messages.length >= MAX_CONVERSATION_MESSAGES || contentBytes > MAX_CONVERSATION_BYTES) {
        throw new DaemonProtocolError("daemon conversation exceeded the content limit");
      }
      messages.push(message);
    }
    if (!page.nextCursor) return messages;
    if (seen.has(page.nextCursor)) throw new DaemonProtocolError("daemon conversation cursor repeated");
    seen.add(page.nextCursor);
    cursor = boundedString(page.nextCursor, "conversation cursor", 512);
  } while (cursor);
  return messages;
}

async function collectConversationTurns(
  protocol: DaemonProtocolClient,
  threadId: string,
): Promise<import("../generated/daemon/v1/daemon.js").ConversationTurnResult[]> {
  const turns: import("../generated/daemon/v1/daemon.js").ConversationTurnResult[] = [];
  const turnIds = new Set<string>();
  const seen = new Set<string>();
  let encodedBytes = 0;
  let cursor = "";
  do {
    const page = await protocol.listConversationTurns(threadId, cursor, CONVERSATION_TURN_PAGE_SIZE);
    if (turns.length + page.turns.length > MAX_CONVERSATION_TURNS) {
      throw new DaemonProtocolError("daemon conversation exceeded the turn limit");
    }
    for (const turn of page.turns) {
      encodedBytes += ConversationTurnResult.encode(turn).finish().byteLength;
      if (encodedBytes > MAX_CONVERSATION_TURN_BYTES) {
        throw new DaemonProtocolError("daemon conversation turns exceeded the byte limit");
      }
      if (turnIds.has(turn.turnId)) {
        throw new DaemonProtocolError("daemon conversation repeated a turn");
      }
      turnIds.add(turn.turnId);
      turns.push(turn);
    }
    if (!page.nextCursor) return turns;
    if (seen.has(page.nextCursor)) throw new DaemonProtocolError("daemon conversation turn cursor repeated");
    seen.add(page.nextCursor);
    cursor = boundedString(page.nextCursor, "conversation turn cursor", 512);
  } while (cursor);
  return turns;
}

function mapProject(project: import("../generated/daemon/v1/daemon.js").Project): DaemonProject {
  return {
    id: boundedString(project.id, "project id"),
    name: boundedString(project.name, "project name", 256),
    description: boundedText(project.description, "project description", 4_096),
    state: projectStateFromWire(project.state),
    revision: safeNumber(project.revision, "project revision"),
    createdAtUnixMs: safeNumber(project.createdAtUnixMs, "project creation time"),
    updatedAtUnixMs: safeNumber(project.updatedAtUnixMs, "project update time"),
  };
}

export function mapThread(thread: import("../generated/daemon/v1/daemon.js").Thread): DaemonThread {
  if (!thread.lineage) throw new DaemonProtocolError("daemon thread lineage is missing");
  const id = boundedString(thread.id, "thread id");
  return {
    id,
    projectId: boundedString(thread.projectId, "thread project id"),
    title: boundedString(thread.title, "thread title", 512),
    state: threadStateFromWire(thread.state),
    revision: safeNumber(thread.revision, "thread revision"),
    createdAtUnixMs: safeNumber(thread.createdAtUnixMs, "thread creation time"),
    updatedAtUnixMs: safeNumber(thread.updatedAtUnixMs, "thread update time"),
    lineage: mapConversationThreadLineage(thread.lineage, id),
  };
}

export function mapMessage(message: import("../generated/daemon/v1/daemon.js").Message): DaemonMessage {
  if (!message.derivation) throw new DaemonProtocolError("daemon message derivation is missing");
  const id = boundedString(message.id, "message id");
  const role = messageRoleFromWire(message.role);
  const state = messageStateFromWire(message.state);
  const derivation = mapConversationMessageDerivation(message.derivation, id, role);
  if (state === "deleted" && derivation.origin !== "original") {
    throw new DaemonProtocolError("daemon deleted message derivation is invalid");
  }
  return {
    id,
    threadId: boundedString(message.threadId, "message thread id"),
    sequence: safeNumber(message.sequence, "message sequence"),
    role,
    content: boundedText(message.content, "message content", 1024 * 1024),
    state,
    revision: safeNumber(message.revision, "message revision"),
    createdAtUnixMs: safeNumber(message.createdAtUnixMs, "message creation time"),
    updatedAtUnixMs: safeNumber(message.updatedAtUnixMs, "message update time"),
    derivation,
  };
}

function mapConversationThreadLineage(
  lineage: import("../generated/daemon/v1/daemon.js").ConversationThreadLineage,
  ownerThreadId: string,
): DaemonThread["lineage"] {
  const rootThreadId = boundedString(lineage.rootThreadId, "conversation root thread id");
  if (lineage.origin?.$case === "original") {
    if (rootThreadId !== ownerThreadId || lineage.forkDepth !== 0) {
      throw new DaemonProtocolError("daemon original thread lineage is invalid");
    }
    return { origin: "original", rootThreadId, forkDepth: 0 };
  }
  if (lineage.origin?.$case !== "fork") {
    throw new DaemonProtocolError("daemon thread lineage origin is invalid");
  }
  const fork = lineage.origin.value;
  const parentThreadId = boundedString(fork.parentThreadId, "conversation parent thread id");
  const sourceTurnId = boundedString(fork.sourceTurnId, "conversation fork source turn id");
  const sourceMessageId = boundedString(fork.sourceMessageId, "conversation fork source message id");
  if (
    rootThreadId === ownerThreadId
    || parentThreadId === ownerThreadId
    || !Number.isSafeInteger(lineage.forkDepth)
    || lineage.forkDepth < 1
    || lineage.forkDepth > 64
    || ((lineage.forkDepth === 1) !== (parentThreadId === rootThreadId))
  ) {
    throw new DaemonProtocolError("daemon forked thread lineage is invalid");
  }
  return {
    origin: "fork",
    rootThreadId,
    parentThreadId,
    sourceTurnId,
    sourceMessageId,
    kind: conversationForkKindFromWire(fork.kind),
    forkDepth: lineage.forkDepth,
  };
}

function conversationForkKindFromWire(value: ConversationForkKind): DaemonConversationForkKind {
  switch (value) {
    case ConversationForkKind.CONVERSATION_FORK_KIND_BRANCH:
      return "branch";
    case ConversationForkKind.CONVERSATION_FORK_KIND_EDIT_AND_BRANCH:
      return "edit_and_branch";
    case ConversationForkKind.CONVERSATION_FORK_KIND_REGENERATE:
      return "regenerate";
    case ConversationForkKind.CONVERSATION_FORK_KIND_UNSPECIFIED:
    case ConversationForkKind.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("invalid daemon conversation fork kind");
  }
}

function mapConversationMessageDerivation(
  derivation: import("../generated/daemon/v1/daemon.js").ConversationMessageDerivation,
  ownerMessageId: string,
  role: DaemonMessage["role"],
): DaemonMessage["derivation"] {
  if (derivation.origin?.$case === "original") return { origin: "original" };
  if (derivation.origin?.$case !== "fork") {
    throw new DaemonProtocolError("daemon message derivation origin is invalid");
  }
  const fork = derivation.origin.value;
  const sourceMessageId = boundedString(fork.sourceMessageId, "derived source message id");
  const sourceTurnId = boundedString(fork.sourceTurnId, "derived source turn id");
  const contextPosition = fork.contextPosition;
  if (
    sourceMessageId === ownerMessageId
    || (contextPosition !== undefined && (
      !Number.isSafeInteger(contextPosition)
      || contextPosition < 1
      || contextPosition > MAX_CONVERSATION_CONTEXT_MESSAGES
    ))
  ) {
    throw new DaemonProtocolError("daemon forked message derivation is invalid");
  }
  switch (fork.kind) {
    case ConversationMessageDerivationKind.CONVERSATION_MESSAGE_DERIVATION_KIND_CONTEXT_COPY:
      if (contextPosition === undefined) {
        throw new DaemonProtocolError("daemon context-copy derivation has no context position");
      }
      return { origin: "fork", sourceMessageId, sourceTurnId, contextPosition, kind: "context_copy" };
    case ConversationMessageDerivationKind.CONVERSATION_MESSAGE_DERIVATION_KIND_SOURCE_ASSISTANT_COPY:
      if (contextPosition !== undefined || role !== "assistant") {
        throw new DaemonProtocolError("daemon assistant-copy derivation is invalid");
      }
      return { origin: "fork", sourceMessageId, sourceTurnId, kind: "source_assistant_copy" };
    case ConversationMessageDerivationKind.CONVERSATION_MESSAGE_DERIVATION_KIND_EDITED_USER:
      if (contextPosition === undefined || role !== "user") {
        throw new DaemonProtocolError("daemon edited-user derivation is invalid");
      }
      return { origin: "fork", sourceMessageId, sourceTurnId, contextPosition, kind: "edited_user" };
    case ConversationMessageDerivationKind.CONVERSATION_MESSAGE_DERIVATION_KIND_UNSPECIFIED:
    case ConversationMessageDerivationKind.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("invalid daemon conversation message derivation kind");
  }
}

export function mapConversationTurn(
  turn: import("../generated/daemon/v1/daemon.js").ConversationTurnResult,
): DaemonConversationTurn {
  if (!turn.userMessage || !turn.run || !turn.usage || !turn.lineage) {
    throw new DaemonProtocolError("daemon conversation turn is incomplete");
  }
  const turnId = boundedString(turn.turnId, "conversation turn id");
  const mapped: DaemonConversationTurn = {
    turnId,
    state: conversationTurnStateFromWire(turn.state),
    revision: safeNumber(turn.revision, "conversation turn revision"),
    modelId: boundedString(turn.modelId, "conversation model id", 512),
    searchEnabled: turn.searchEnabled,
    userMessage: mapMessage(turn.userMessage),
    assistantMessage: turn.assistantMessage ? mapMessage(turn.assistantMessage) : undefined,
    run: mapRun(turn.run),
    failure: turn.failure
      ? {
          kind: conversationFailureKindFromWire(turn.failure.kind),
          message: boundedString(turn.failure.message, "conversation failure message", 512),
          retryable: turn.failure.retryable,
        }
      : undefined,
    citations: mapConversationCitations(turn.citations),
    usage: {
      inputTokens: safeNumber(turn.usage.inputTokens, "conversation input tokens"),
      outputTokens: safeNumber(turn.usage.outputTokens, "conversation output tokens"),
      costInUsdTicks: safeNumber(turn.usage.costInUsdTicks, "conversation cost ticks"),
    },
    zeroDataRetention: turn.zeroDataRetention,
    lineage: mapConversationTurnLineage(turn.lineage, turnId),
    retryEligibility: conversationRetryEligibilityFromWire(turn.retryEligibility),
  };
  validateConversationTurn(mapped);
  return mapped;
}

export function mapRetryConversationTurnResponse(
  turn: import("../generated/daemon/v1/daemon.js").ConversationTurnResult,
  requestedSourceTurnId: string,
): DaemonConversationTurn {
  const mapped = mapConversationTurn(turn);
  if (
    mapped.lineage.origin !== "retry"
    || mapped.lineage.sourceTurnId !== requestedSourceTurnId
  ) {
    throw new DaemonProtocolError("daemon retry response does not match the requested source turn");
  }
  return mapped;
}

export function mapConversationFork(
  fork: import("../generated/daemon/v1/daemon.js").ConversationForkResult,
  expectedKind: DaemonConversationForkKind,
  expectedSourceTurnId: string,
): DaemonConversationFork {
  if (!fork.childThread) {
    throw new DaemonProtocolError("daemon conversation fork is missing its child thread");
  }
  const childThread = mapThread(fork.childThread);
  if (!fork.delivery) {
    throw new DaemonProtocolError("daemon conversation fork is missing its delivery state");
  }
  const delivery = mapConversationForkDelivery(fork.delivery, childThread.id);
  if (
    childThread.lineage.origin !== "fork"
    || childThread.lineage.kind !== expectedKind
    || childThread.lineage.sourceTurnId !== expectedSourceTurnId
  ) {
    throw new DaemonProtocolError("daemon conversation fork does not match the request");
  }
  const startedTurn = fork.startedTurn ? mapConversationTurn(fork.startedTurn) : undefined;
  if ((expectedKind === "branch") !== (startedTurn === undefined)) {
    throw new DaemonProtocolError("daemon conversation fork has an invalid turn presence");
  }
  if (startedTurn) {
    const expectedTurnOrigin = expectedKind === "edit_and_branch"
      ? "edit_and_branch"
      : "regenerate";
    const expectedMessageDerivation = expectedKind === "edit_and_branch"
      ? "edited_user"
      : "context_copy";
    if (
      startedTurn.run.threadId !== childThread.id
      || startedTurn.run.projectId !== childThread.projectId
      || startedTurn.userMessage.threadId !== childThread.id
      || startedTurn.lineage.origin !== expectedTurnOrigin
      || startedTurn.lineage.sourceTurnId !== expectedSourceTurnId
      || startedTurn.userMessage.derivation.origin !== "fork"
      || startedTurn.userMessage.derivation.kind !== expectedMessageDerivation
      || startedTurn.userMessage.derivation.sourceTurnId !== expectedSourceTurnId
    ) {
      throw new DaemonProtocolError("daemon conversation fork turn is not owned by its child");
    }
  }
  return { childThread, startedTurn, delivery };
}

export function mapConversationForkDelivery(
  delivery: ConversationForkDelivery,
  expectedChildThreadId?: string,
): DaemonConversationForkDelivery {
  const childThreadId = boundedString(
    delivery.childThreadId,
    "conversation fork delivery child thread id",
  );
  if (expectedChildThreadId !== undefined && childThreadId !== expectedChildThreadId) {
    throw new DaemonProtocolError("daemon conversation fork delivery belongs to another child");
  }
  const revision = safeNumber(delivery.revision, "conversation fork delivery revision");
  let state: DaemonConversationForkDelivery["state"];
  switch (delivery.state) {
    case ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_PENDING:
      state = "pending";
      break;
    case ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_ACKNOWLEDGED:
      state = "acknowledged";
      break;
    case ConversationForkDeliveryState.CONVERSATION_FORK_DELIVERY_STATE_UNSPECIFIED:
    case ConversationForkDeliveryState.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("invalid daemon conversation fork delivery state");
  }
  if ((state === "pending" && revision !== 0) || (state === "acknowledged" && revision !== 1)) {
    throw new DaemonProtocolError("daemon conversation fork delivery state is inconsistent");
  }
  return { childThreadId, state, revision };
}

export function mapConversationForkMetadata(
  metadata: import("../generated/daemon/v1/daemon.js").ConversationForkMetadata,
  ownerThreadId: string,
): DaemonConversationForkMetadata {
  if (!metadata.lineage) {
    throw new DaemonProtocolError("daemon conversation fork metadata is missing lineage");
  }
  if (
    metadata.inheritedAssistantOutcomes.length > MAX_CONVERSATION_INHERITED_OUTCOMES
    || metadata.familyThreads.length < 1
    || metadata.familyThreads.length > MAX_CONVERSATION_FAMILY_THREADS
  ) {
    throw new DaemonProtocolError("daemon conversation fork metadata exceeds its bounds");
  }
  const inheritedAssistantOutcomes: DaemonConversationInheritedOutcome[] = [];
  const outcomeMessages = new Set<string>();
  for (const outcome of metadata.inheritedAssistantOutcomes) {
    if (!outcome.usage) {
      throw new DaemonProtocolError("daemon inherited assistant outcome is incomplete");
    }
    const childAssistantMessageId = boundedString(
      outcome.childAssistantMessageId,
      "inherited child assistant message id",
    );
    if (outcomeMessages.has(childAssistantMessageId)) {
      throw new DaemonProtocolError("daemon inherited assistant outcome is duplicated");
    }
    outcomeMessages.add(childAssistantMessageId);
    inheritedAssistantOutcomes.push({
      childAssistantMessageId,
      sourceTurnId: boundedString(outcome.sourceTurnId, "inherited source turn id"),
      modelId: boundedString(outcome.modelId, "inherited model id", 512),
      citations: mapConversationCitations(outcome.citations),
      usage: {
        inputTokens: safeNumber(outcome.usage.inputTokens, "inherited input tokens"),
        outputTokens: safeNumber(outcome.usage.outputTokens, "inherited output tokens"),
        costInUsdTicks: safeNumber(outcome.usage.costInUsdTicks, "inherited cost ticks"),
      },
      zeroDataRetention: outcome.zeroDataRetention,
    });
  }
  const familyThreads = metadata.familyThreads.map(mapThread);
  if (new Set(familyThreads.map((thread) => thread.id)).size !== familyThreads.length) {
    throw new DaemonProtocolError("daemon conversation fork family contains duplicate threads");
  }
  const mapped = {
    lineage: mapConversationThreadLineage(metadata.lineage, ownerThreadId),
    inheritedAssistantOutcomes,
    familyThreads,
  };
  if (conversationForkMetadataEstimatedBytes(mapped) > MAX_CONVERSATION_FORK_METADATA_BYTES) {
    throw new DaemonProtocolError("daemon conversation fork metadata exceeds its byte bound");
  }
  return mapped;
}

function conversationForkMetadataEstimatedBytes(
  metadata: DaemonConversationForkMetadata,
): number {
  const encoder = new TextEncoder();
  const bytes = (value: string) => encoder.encode(value).byteLength;
  const lineageBytes = (lineage: DaemonThread["lineage"]) => {
    let total = 96 + bytes(lineage.rootThreadId);
    if (lineage.origin === "fork") {
      total += bytes(lineage.parentThreadId)
        + bytes(lineage.sourceTurnId)
        + bytes(lineage.sourceMessageId);
    }
    return total;
  };
  let total = 256 + lineageBytes(metadata.lineage);
  for (const thread of metadata.familyThreads) {
    total += 192
      + bytes(thread.id)
      + bytes(thread.projectId)
      + bytes(thread.title)
      + lineageBytes(thread.lineage);
  }
  for (const outcome of metadata.inheritedAssistantOutcomes) {
    total += 192
      + bytes(outcome.childAssistantMessageId)
      + bytes(outcome.sourceTurnId)
      + bytes(outcome.modelId);
    for (const citation of outcome.citations) {
      total += 32 + bytes(citation.url) + bytes(citation.title);
    }
  }
  return total;
}

function mapConversationTurnLineage(
  lineage: import("../generated/daemon/v1/daemon.js").ConversationTurnLineage,
  ownerTurnId: string,
): DaemonConversationTurn["lineage"] {
  switch (lineage.origin) {
    case ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_ORIGINAL:
      if (lineage.sourceTurnId !== "" || lineage.retryDepth !== 0) {
        throw new DaemonProtocolError("daemon original conversation lineage is invalid");
      }
      return { origin: "original", retryDepth: 0 };
    case ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_RETRY: {
      const sourceTurnId = boundedString(lineage.sourceTurnId, "conversation retry source turn id");
      if (
        sourceTurnId === ownerTurnId
        || !Number.isSafeInteger(lineage.retryDepth)
        || lineage.retryDepth < 1
        || lineage.retryDepth > 64
      ) {
        throw new DaemonProtocolError("daemon retry conversation lineage is invalid");
      }
      return { origin: "retry", sourceTurnId, retryDepth: lineage.retryDepth };
    }
    case ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_EDIT_AND_BRANCH:
    case ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_REGENERATE: {
      const sourceTurnId = boundedString(lineage.sourceTurnId, "conversation fork source turn id");
      if (sourceTurnId === ownerTurnId || lineage.retryDepth !== 0) {
        throw new DaemonProtocolError("daemon fork conversation turn lineage is invalid");
      }
      return {
        origin: lineage.origin === ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_EDIT_AND_BRANCH
          ? "edit_and_branch"
          : "regenerate",
        sourceTurnId,
        retryDepth: 0,
      };
    }
    case ConversationTurnOrigin.CONVERSATION_TURN_ORIGIN_UNSPECIFIED:
    case ConversationTurnOrigin.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("invalid daemon conversation turn origin");
  }
}

function conversationRetryEligibilityFromWire(
  value: ConversationRetryEligibility,
): DaemonConversationTurn["retryEligibility"] {
  switch (value) {
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_ALLOWED:
      return "allowed";
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_NOT_NEWEST:
      return "not_newest";
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_IN_PROGRESS:
      return "source_in_progress";
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_COMPLETED:
      return "source_completed";
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_INTERRUPTED_NEEDS_REVIEW:
      return "source_interrupted_needs_review";
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_FAILURE_NOT_RETRYABLE:
      return "failure_not_retryable";
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_ACCOUNT_UNAVAILABLE:
      return "source_account_unavailable";
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_DEPTH_EXHAUSTED:
      return "depth_exhausted";
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_SOURCE_READ_ONLY:
      return "source_read_only";
    case ConversationRetryEligibility.CONVERSATION_RETRY_ELIGIBILITY_UNSPECIFIED:
    case ConversationRetryEligibility.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("invalid daemon conversation retry eligibility");
  }
}

function mapConversationCitations(
  citations: import("../generated/daemon/v1/daemon.js").ConversationCitation[],
): DaemonConversationTurn["citations"] {
  if (citations.length > MAX_CONVERSATION_CITATIONS) {
    throw new DaemonProtocolError("daemon conversation citations exceeded the count limit");
  }
  let aggregateBytes = 0;
  return citations.map((citation) => {
    const title = boundedText(citation.title, "conversation citation title", 500);
    const url = boundedHttpsUrl(citation.url, "conversation citation url", 8_192);
    aggregateBytes += Buffer.byteLength(title, "utf8") + Buffer.byteLength(url, "utf8");
    if (aggregateBytes > MAX_CONVERSATION_CITATION_BYTES) {
      throw new DaemonProtocolError("daemon conversation citations exceeded the byte limit");
    }
    return { title, url };
  });
}

function validateConversationTurn(turn: DaemonConversationTurn): void {
  if (
    turn.userMessage.role !== "user"
    || turn.userMessage.state !== "active"
    || turn.userMessage.threadId !== turn.run.threadId
    || Boolean(turn.assistantMessage && turn.assistantMessage.role !== "assistant")
    || Boolean(turn.assistantMessage && turn.assistantMessage.state !== "active")
    || Boolean(turn.assistantMessage && turn.assistantMessage.threadId !== turn.userMessage.threadId)
    || (turn.assistantMessage && turn.assistantMessage.sequence <= turn.userMessage.sequence)
  ) {
    throw new DaemonProtocolError("daemon conversation turn message links are invalid");
  }
  const expectedRunState: Record<DaemonConversationTurn["state"], DaemonRunState> = {
    reserved: "queued",
    provider_started: "running",
    completed: "completed",
    failed: "failed",
    cancelled: "cancelled",
    interrupted_needs_review: "interrupted_needs_review",
  };
  if (turn.run.state !== expectedRunState[turn.state]) {
    throw new DaemonProtocolError("daemon conversation turn run state is invalid");
  }
  const expectedTurnRevision: Record<DaemonConversationTurn["state"], number> = {
    reserved: 0,
    provider_started: 1,
    completed: 2,
    failed: 2,
    cancelled: 1,
    interrupted_needs_review: 2,
  };
  const expectedRunRevision: Record<DaemonConversationTurn["state"], number> = {
    reserved: 0,
    provider_started: 2,
    completed: 3,
    failed: 3,
    cancelled: 1,
    interrupted_needs_review: 3,
  };
  if (
    turn.revision !== expectedTurnRevision[turn.state]
    || turn.run.revision !== expectedRunRevision[turn.state]
  ) {
    throw new DaemonProtocolError("daemon conversation turn revision is invalid");
  }
  const completed = turn.state === "completed";
  const failed = turn.state === "failed";
  if (
    Boolean(turn.assistantMessage) !== completed
    || Boolean(turn.failure) !== failed
    || (!completed && turn.citations.length > 0)
    || (!completed && turn.zeroDataRetention !== undefined)
    || (!completed && (
      turn.usage.inputTokens !== 0
      || turn.usage.outputTokens !== 0
      || turn.usage.costInUsdTicks !== 0
    ))
  ) {
    throw new DaemonProtocolError("daemon conversation turn outcome is invalid");
  }
  const safeRetrySource = turn.state === "cancelled"
    || (turn.state === "failed" && turn.failure?.retryable === true);
  const retryDepthAvailable = turn.lineage.retryDepth < 64;
  const eligibilityMatchesState = {
    allowed: safeRetrySource && retryDepthAvailable,
    not_newest: safeRetrySource && retryDepthAvailable,
    source_in_progress: turn.state === "reserved" || turn.state === "provider_started",
    source_completed: turn.state === "completed",
    source_interrupted_needs_review: turn.state === "interrupted_needs_review",
    failure_not_retryable: turn.state === "failed" && turn.failure?.retryable === false,
    source_account_unavailable: safeRetrySource && retryDepthAvailable,
    depth_exhausted: safeRetrySource && turn.lineage.retryDepth === 64,
    source_read_only: safeRetrySource && retryDepthAvailable,
  } satisfies Record<DaemonConversationTurn["retryEligibility"], boolean>;
  if (!eligibilityMatchesState[turn.retryEligibility]) {
    throw new DaemonProtocolError("daemon conversation retry eligibility conflicts with turn state");
  }
}

export function validateConversationAggregate(
  thread: DaemonThread,
  messages: DaemonMessage[],
  turns: DaemonConversationTurn[],
  forkMetadata: DaemonConversationForkMetadata,
  workRun?: DaemonRun,
): void {
  if (!sameThreadLineage(thread.lineage, forkMetadata.lineage)) {
    throw new DaemonProtocolError("daemon conversation fork metadata lineage is inconsistent");
  }
  const familyIds = new Set<string>();
  let currentFamilyThread: DaemonThread | undefined;
  for (const familyThread of forkMetadata.familyThreads) {
    if (
      familyIds.has(familyThread.id)
      || familyThread.projectId !== thread.projectId
      || familyThread.lineage.rootThreadId !== thread.lineage.rootThreadId
    ) {
      throw new DaemonProtocolError("daemon conversation fork family is inconsistent");
    }
    familyIds.add(familyThread.id);
    if (familyThread.id === thread.id) currentFamilyThread = familyThread;
  }
  if (!currentFamilyThread || !sameThread(currentFamilyThread, thread)) {
    throw new DaemonProtocolError("daemon conversation fork family omits the current thread");
  }
  const familyById = new Map(
    forkMetadata.familyThreads.map((familyThread) => [familyThread.id, familyThread]),
  );
  for (const familyThread of forkMetadata.familyThreads) {
    if (familyThread.lineage.origin === "original") continue;
    const parent = familyById.get(familyThread.lineage.parentThreadId);
    if (
      !parent
      || parent.lineage.forkDepth + 1 !== familyThread.lineage.forkDepth
    ) {
      throw new DaemonProtocolError("daemon conversation fork family has invalid ancestry");
    }
  }
  const canonical = new Map<string, DaemonMessage>();
  let previousSequence = 0;
  for (const message of messages) {
    if (
      message.threadId !== thread.id
      || message.sequence <= previousSequence
      || canonical.has(message.id)
    ) {
      throw new DaemonProtocolError("daemon conversation message order is invalid");
    }
    previousSequence = message.sequence;
    canonical.set(message.id, message);
  }
  validateConversationForkMessagePrefix(thread, messages);
  if (workRun) {
    validateWorkConversation(thread, messages, turns, forkMetadata, workRun);
    return;
  }
  const linkedMessages = new Set<string>();
  const inheritedOutcomes = new Map(
    forkMetadata.inheritedAssistantOutcomes.map((outcome) => [
      outcome.childAssistantMessageId,
      outcome,
    ]),
  );
  if (inheritedOutcomes.size !== forkMetadata.inheritedAssistantOutcomes.length) {
    throw new DaemonProtocolError("daemon conversation inherited outcomes are duplicated");
  }
  const turnsById = new Map(turns.map((turn) => [turn.turnId, turn]));
  if (turnsById.size !== turns.length) {
    throw new DaemonProtocolError("daemon conversation contains duplicate turns");
  }
  const activeTurnCount = turns.filter((turn) => (
    turn.state === "reserved" || turn.state === "provider_started"
  )).length;
  if (activeTurnCount > 1) {
    throw new DaemonProtocolError("daemon conversation contains multiple active turns");
  }
  const retrySources = new Set<string>();
  for (const turn of turns) {
    if (turn.run.threadId !== thread.id || turn.run.projectId !== thread.projectId) {
      throw new DaemonProtocolError("daemon conversation turn ownership is invalid");
    }
    const newest = turn.userMessage.sequence === previousSequence;
    if (
      (turn.retryEligibility === "allowed" && !newest)
      || (turn.retryEligibility === "not_newest" && newest)
    ) {
      throw new DaemonProtocolError("daemon conversation retry eligibility has an invalid position");
    }
    if (turn.lineage.origin === "retry") {
      const source = turnsById.get(turn.lineage.sourceTurnId);
      const sourceIsSafe = source?.state === "cancelled"
        || (source?.state === "failed" && source.failure?.retryable === true);
      if (
        !source
        || !sourceIsSafe
        || source.userMessage.sequence + 1 !== turn.userMessage.sequence
        || retrySources.has(turn.lineage.sourceTurnId)
        || source.userMessage.content !== turn.userMessage.content
        || source.modelId !== turn.modelId
        || source.lineage.retryDepth + 1 !== turn.lineage.retryDepth
      ) {
        throw new DaemonProtocolError("daemon conversation retry lineage references invalid history");
      }
      retrySources.add(turn.lineage.sourceTurnId);
    }
    if (
      turn.lineage.origin === "edit_and_branch"
      || turn.lineage.origin === "regenerate"
    ) {
      const expectedKind = turn.lineage.origin;
      const expectedDerivation = expectedKind === "edit_and_branch"
        ? "edited_user"
        : "context_copy";
      if (
        thread.lineage.origin !== "fork"
        || thread.lineage.kind !== expectedKind
        || thread.lineage.sourceTurnId !== turn.lineage.sourceTurnId
        || turn.userMessage.derivation.origin !== "fork"
        || turn.userMessage.derivation.kind !== expectedDerivation
        || turn.userMessage.derivation.sourceTurnId !== turn.lineage.sourceTurnId
      ) {
        throw new DaemonProtocolError("daemon fork turn lineage references invalid history");
      }
    }
    for (const message of [turn.userMessage, turn.assistantMessage].filter(
      (value): value is DaemonMessage => Boolean(value),
    )) {
      const stored = canonical.get(message.id);
      if (!stored || linkedMessages.has(message.id) || !sameMessage(stored, message)) {
        throw new DaemonProtocolError("daemon conversation turn references invalid history");
      }
      linkedMessages.add(message.id);
    }
  }
  for (const message of messages) {
    if (
      message.derivation.origin === "fork"
      && (
        thread.lineage.origin !== "fork"
        || message.derivation.sourceTurnId !== thread.lineage.sourceTurnId
      )
    ) {
      throw new DaemonProtocolError("daemon derived message does not match its thread fork");
    }
    const inherited = inheritedOutcomes.get(message.id);
    if (message.role === "assistant" && !linkedMessages.has(message.id)) {
      if (
        !inherited
        || message.derivation.origin !== "fork"
        || (
          message.derivation.kind !== "context_copy"
          && message.derivation.kind !== "source_assistant_copy"
        )
        || (
          message.derivation.kind === "source_assistant_copy"
          && message.derivation.sourceTurnId !== inherited.sourceTurnId
        )
      ) {
        throw new DaemonProtocolError("daemon conversation contains an unlinked assistant message");
      }
      inheritedOutcomes.delete(message.id);
    } else if (inherited) {
      throw new DaemonProtocolError("daemon inherited outcome is not an assistant copy");
    }
  }
  if (inheritedOutcomes.size !== 0) {
    throw new DaemonProtocolError("daemon inherited outcome references missing history");
  }
}

function validateWorkConversation(
  thread: DaemonThread,
  messages: DaemonMessage[],
  turns: DaemonConversationTurn[],
  forkMetadata: DaemonConversationForkMetadata,
  run: DaemonRun,
): void {
  const terminalWithoutOutput = run.state === "failed"
    || run.state === "cancelled"
    || run.state === "interrupted_needs_review";
  const expectedMessages = run.state === "completed" ? 2 : 1;
  if (
    thread.lineage.origin !== "original"
    || forkMetadata.familyThreads.length !== 1
    || forkMetadata.inheritedAssistantOutcomes.length !== 0
    || turns.length !== 0
    || run.kind !== "work"
    || run.workBackend !== "host_direct"
    || run.projectId !== thread.projectId
    || run.threadId !== thread.id
    || messages.length !== expectedMessages
    || messages[0]?.role !== "user"
    || messages[0]?.sequence !== 1
    || messages[0]?.state !== "active"
    || messages[0]?.derivation.origin !== "original"
    || Boolean(terminalWithoutOutput && messages.some((message) => message.role === "assistant"))
  ) {
    throw new DaemonProtocolError("daemon Work conversation aggregate is invalid");
  }
  if (run.state === "completed") {
    const assistant = messages[1];
    if (
      !assistant
      || assistant.role !== "assistant"
      || assistant.sequence !== 2
      || assistant.state !== "active"
      || assistant.derivation.origin !== "original"
    ) {
      throw new DaemonProtocolError("daemon Work conversation outcome is invalid");
    }
  }
}

function validateConversationForkMessagePrefix(
  thread: DaemonThread,
  messages: DaemonMessage[],
): void {
  if (thread.lineage.origin === "original") return;
  let derivedCount = 0;
  let reachedOriginalSuffix = false;
  for (const message of messages) {
    if (message.derivation.origin === "original") {
      reachedOriginalSuffix = true;
      continue;
    }
    if (
      reachedOriginalSuffix
      || message.state !== "active"
      || message.sequence !== derivedCount + 1
      || message.derivation.sourceTurnId !== thread.lineage.sourceTurnId
      || (
        message.derivation.kind !== "source_assistant_copy"
        && message.derivation.contextPosition !== message.sequence
      )
    ) {
      throw new DaemonProtocolError("daemon conversation fork message prefix is invalid");
    }
    derivedCount += 1;
  }
  const prefix = messages.slice(0, derivedCount);
  const final = prefix.at(-1);
  if (!final || final.derivation.origin !== "fork") {
    throw new DaemonProtocolError("daemon conversation fork has no derived message prefix");
  }
  const prior = prefix.slice(0, -1);
  if (prior.some((message) => (
    message.derivation.origin !== "fork" || message.derivation.kind !== "context_copy"
  ))) {
    throw new DaemonProtocolError("daemon conversation fork context prefix is invalid");
  }
  const finalMatches = {
    branch: final.role === "assistant"
      && final.derivation.kind === "source_assistant_copy"
      && final.derivation.sourceMessageId === thread.lineage.sourceMessageId
      && prior.length > 0
      && prior.at(-1)?.role === "user",
    edit_and_branch: final.role === "user"
      && final.derivation.kind === "edited_user"
      && final.derivation.sourceMessageId === thread.lineage.sourceMessageId,
    regenerate: final.role === "user" && final.derivation.kind === "context_copy",
  }[thread.lineage.kind];
  if (!finalMatches) {
    throw new DaemonProtocolError("daemon conversation fork divergence message is invalid");
  }
}

function sameThread(left: DaemonThread, right: DaemonThread): boolean {
  return left.id === right.id
    && left.projectId === right.projectId
    && left.title === right.title
    && left.state === right.state
    && left.revision === right.revision
    && left.createdAtUnixMs === right.createdAtUnixMs
    && left.updatedAtUnixMs === right.updatedAtUnixMs
    && sameThreadLineage(left.lineage, right.lineage);
}

function sameThreadLineage(
  left: DaemonThread["lineage"],
  right: DaemonThread["lineage"],
): boolean {
  if (
    left.origin !== right.origin
    || left.rootThreadId !== right.rootThreadId
    || left.forkDepth !== right.forkDepth
  ) {
    return false;
  }
  if (left.origin === "original" || right.origin === "original") return true;
  return left.parentThreadId === right.parentThreadId
    && left.sourceTurnId === right.sourceTurnId
    && left.sourceMessageId === right.sourceMessageId
    && left.kind === right.kind;
}

function sameMessage(left: DaemonMessage, right: DaemonMessage): boolean {
  return left.id === right.id
    && left.threadId === right.threadId
    && left.sequence === right.sequence
    && left.role === right.role
    && left.content === right.content
    && left.state === right.state
    && left.revision === right.revision
    && left.createdAtUnixMs === right.createdAtUnixMs
    && left.updatedAtUnixMs === right.updatedAtUnixMs
    && sameMessageDerivation(left.derivation, right.derivation);
}

function sameMessageDerivation(
  left: DaemonMessage["derivation"],
  right: DaemonMessage["derivation"],
): boolean {
  if (left.origin !== right.origin) return false;
  if (left.origin === "original" || right.origin === "original") return true;
  return left.sourceMessageId === right.sourceMessageId
    && left.sourceTurnId === right.sourceTurnId
    && left.contextPosition === right.contextPosition
    && left.kind === right.kind;
}

function mapAccountState(
  state: import("../generated/daemon/v1/daemon.js").AccountState,
): DaemonAccountState {
  return {
    xaiApiKeyConfigured: state.xaiApiKeyConfigured,
    xaiCapabilitiesResolved: state.xaiCapabilitiesResolved,
    grokBuildAuthenticated: state.grokBuildAuthenticated === true,
  };
}

function mapSuperGrokEnrollmentStatus(
  status: SuperGrokEnrollmentStatus,
): DaemonSuperGrokEnrollmentStatus {
  const states = new Set(["disconnected", "starting", "awaiting_user", "connected", "failed"]);
  if (!states.has(status.state)) {
    throw new DaemonProtocolError("daemon returned an invalid SuperGrok enrollment state");
  }
  return {
    state: status.state as DaemonSuperGrokEnrollmentStatus["state"],
    verificationUri: status.verificationUri
      ? boundedHttpsUrl(status.verificationUri, "SuperGrok verification URI", 2_048)
      : "",
    userCode: status.userCode ? boundedString(status.userCode, "SuperGrok user code", 128) : "",
    expiresAtUnixMs: safeNumber(status.expiresAtUnixMs, "SuperGrok enrollment expiry"),
    credentialGeneration: safeNumber(status.credentialGeneration, "SuperGrok credential generation"),
    reasonCode: status.reasonCode
      ? boundedString(status.reasonCode, "SuperGrok failure reason", 128)
      : "",
  };
}

function mapDesktopPreferences(
  preferences: import("../generated/daemon/v1/daemon.js").DesktopPreferences,
): DaemonDesktopPreferences {
  return {
    keepRunningInNotificationArea: preferences.keepRunningInNotificationArea,
    revision: safeNumber(preferences.revision, "desktop preference revision"),
    updatedAtUnixMs: safeNumber(preferences.updatedAtUnixMs, "desktop preference update time"),
  };
}

function mapChatModelPreference(
  preference: import("../generated/daemon/v1/daemon.js").ChatModelPreference,
): DaemonChatModelPreference {
  return {
    selectedModelId: boundedModelIdentifier(preference.selectedModelId, "selected chat model id"),
    revision: safeNumber(preference.revision, "chat model preference revision"),
    updatedAtUnixMs: safeNumber(preference.updatedAtUnixMs, "chat model preference update time"),
  };
}

function mapUsageSummary(
  summary: import("../generated/daemon/v1/daemon.js").UsageSummary,
): DaemonUsageSummary {
  const scopeKind = summary.scopeKind;
  if (scopeKind !== "workspace" && scopeKind !== "project" && scopeKind !== "thread") {
    throw new DaemonProtocolError("daemon usage summary scope is invalid");
  }
  const window = summary.window;
  if (window !== "last_7_days" && window !== "last_30_days" && window !== "all_time") {
    throw new DaemonProtocolError("daemon usage summary window is invalid");
  }
  if (scopeKind === "workspace" && summary.scopeId.length > 0) {
    throw new DaemonProtocolError("daemon workspace usage summary must not include a scope id");
  }
  if (scopeKind !== "workspace" && summary.scopeId.length === 0) {
    throw new DaemonProtocolError("daemon usage summary is missing a scope id");
  }
  return {
    inputTokens: safeNumber(summary.inputTokens, "usage input tokens"),
    outputTokens: safeNumber(summary.outputTokens, "usage output tokens"),
    costInUsdTicks: safeNumber(summary.costInUsdTicks, "usage cost ticks"),
    turnCount: safeNumber(summary.turnCount, "usage turn count"),
    scopeKind,
    scopeId: scopeKind === "workspace" ? "" : boundedString(summary.scopeId, "usage scope id", 128),
    window,
    asOfUnixMs: safeNumber(summary.asOfUnixMs, "usage as-of time"),
  };
}

export function mapChatModelCatalog(
  catalog: import("../generated/daemon/v1/daemon.js").ChatModelCatalog,
): DaemonChatModelCatalog {
  if (!catalog.preference || catalog.models.length > 256) {
    throw new DaemonProtocolError("daemon chat model catalog is incomplete or oversized");
  }
  const advertised = new Set<string>();
  const descriptors = catalog.models.map((model) => {
    const id = boundedModelIdentifier(model.id, "chat model id");
    if (advertised.has(id)) throw new DaemonProtocolError("daemon chat model catalog repeated an identifier");
    advertised.add(id);
    if (model.aliases.length > 64 || model.inputModalities.length > 16 || model.outputModalities.length > 16) {
      throw new DaemonProtocolError("daemon chat model descriptor is oversized");
    }
    const aliases = model.aliases.map((alias) => boundedModelIdentifier(alias, "chat model alias"));
    const inputModalities = model.inputModalities.map((value) => boundedModelModality(value));
    const outputModalities = model.outputModalities.map((value) => boundedModelModality(value));
    // Must match application `supports_text_conversation`: Imagine media ids
    // never become the durable Chat selection even with empty modalities.
    const isImagineMedia = [id, ...aliases].some((value) =>
      value.trim().toLowerCase().startsWith("grok-imagine-"),
    );
    const textConversationReady = !isImagineMedia
      && (inputModalities.length === 0 || inputModalities.includes("text"))
      && (outputModalities.length === 0 || outputModalities.includes("text"));
    if (textConversationReady !== model.textConversationReady) {
      throw new DaemonProtocolError("daemon chat model descriptor readiness is inconsistent");
    }
    return {
      id,
      aliases,
      inputModalities,
      outputModalities,
      textConversationReady,
    };
  });
  for (const model of descriptors) {
    for (const alias of model.aliases) {
      if (advertised.has(alias)) {
        throw new DaemonProtocolError("daemon chat model catalog contains an ambiguous alias");
      }
      advertised.add(alias);
    }
  }
  const preference = mapChatModelPreference(catalog.preference);
  const defaultModelId = boundedModelIdentifier(catalog.defaultModelId, "default chat model id");
  const ready = (modelId: string) => descriptors.some((model) =>
    model.textConversationReady && (model.id === modelId || model.aliases.includes(modelId))
  );
  const selectedReady = preference.revision > 0
    ? descriptors.some((model) => model.textConversationReady && model.id === preference.selectedModelId)
    : ready(preference.selectedModelId);
  if (
    selectedReady !== catalog.selectedModelReady
    || ready(defaultModelId) !== catalog.defaultModelReady
  ) {
    throw new DaemonProtocolError("daemon chat model readiness does not match its catalog");
  }
  return {
    models: descriptors,
    preference,
    defaultModelId,
    selectedModelReady: catalog.selectedModelReady,
    defaultModelReady: catalog.defaultModelReady,
  };
}

export function mapArtifact(artifact: import("../generated/daemon/v1/daemon.js").Artifact): DaemonArtifact {
  const state = artifactStateFromWire(artifact.state);
  const contentVersion = artifact.contentVersion;
  if (
    contentVersion !== undefined
    && (!Number.isSafeInteger(contentVersion) || contentVersion < 1 || contentVersion > 1_000_000)
  ) {
    throw new DaemonProtocolError("daemon artifact content version is invalid");
  }
  const id = boundedString(artifact.id, "artifact id");
  const projectId = boundedString(artifact.projectId, "artifact project id");
  const threadId = artifact.threadId ? boundedString(artifact.threadId, "artifact thread id") : undefined;
  const name = boundedString(artifact.name, "artifact name", 200);
  const revision = safeNumber(artifact.revision, "artifact revision");
  const createdAtUnixMs = safeNumber(artifact.createdAtUnixMs, "artifact creation time");
  const updatedAtUnixMs = safeNumber(artifact.updatedAtUnixMs, "artifact update time");
  if (updatedAtUnixMs < createdAtUnixMs) {
    throw new DaemonProtocolError("daemon artifact timestamps are invalid");
  }
  if (state === "available") {
    if (contentVersion === undefined || artifact.mediaType === "" || revision !== contentVersion) {
      throw new DaemonProtocolError("daemon available artifact content metadata is invalid");
    }
  } else if (artifact.mediaType !== "" || artifact.byteSize !== 0n || contentVersion !== undefined) {
    throw new DaemonProtocolError("daemon non-available artifact exposes content metadata");
  }
  if (
    (state === "unavailable" && (revision !== 0 || updatedAtUnixMs !== createdAtUnixMs))
    || (state === "deleted" && revision === 0)
  ) {
    throw new DaemonProtocolError("daemon artifact lifecycle metadata is invalid");
  }
  const mediaType = state === "available"
    ? boundedString(artifact.mediaType, "artifact media type", 255)
    : undefined;
  const byteSize = state === "available"
    ? safeNumber(artifact.byteSize, "artifact byte size")
    : undefined;
  if (byteSize !== undefined && byteSize > 64 * 1024 * 1024) {
    throw new DaemonProtocolError("daemon artifact byte size is invalid");
  }
  return {
    id,
    projectId,
    threadId,
    name,
    mediaType,
    byteSize,
    contentVersion,
    state,
    revision,
    createdAtUnixMs,
    updatedAtUnixMs,
  };
}

export function mapImportedArtifactOperation(
  operation: import("../generated/daemon/v1/daemon.js").ArtifactOperationResult,
  expectedProjectId: string,
  expectedDisplayName: string,
  expectedMediaType: string,
): DaemonArtifact {
  if (operation.result?.$case !== "importedArtifact") {
    throw new DaemonProtocolError("daemon artifact import response has the wrong result variant");
  }
  const artifact = mapArtifact(operation.result.value);
  if (
    artifact.projectId !== expectedProjectId
    || artifact.name !== expectedDisplayName
    || artifact.mediaType !== expectedMediaType
    || artifact.state !== "available"
    || artifact.contentVersion === undefined
  ) {
    throw new DaemonProtocolError("daemon artifact import response does not match the request");
  }
  return artifact;
}

export function mapArtifactOpenOperation(
  operation: import("../generated/daemon/v1/daemon.js").ArtifactOperationResult,
  expectedArtifactId: string,
  expectedContentVersion: number,
): DaemonArtifactOpenReceipt {
  if (operation.result?.$case !== "openReceipt") {
    throw new DaemonProtocolError("daemon artifact open response has the wrong result variant");
  }
  const receipt = operation.result.value;
  if (
    receipt.artifactId !== expectedArtifactId
    || receipt.contentVersion !== expectedContentVersion
  ) {
    throw new DaemonProtocolError("daemon artifact open receipt does not match the request");
  }
  switch (receipt.status) {
    case ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_OPENED:
      if (receipt.failureCode !== undefined) {
        throw new DaemonProtocolError("opened artifact receipt contains a failure code");
      }
      return {
        artifactId: receipt.artifactId,
        contentVersion: receipt.contentVersion,
        status: "opened",
      };
    case ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_FAILED:
      return {
        artifactId: receipt.artifactId,
        contentVersion: receipt.contentVersion,
        status: "failed",
        failureCode: mapArtifactOpenFailureCode(receipt.failureCode),
      };
    case ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_INTERRUPTED_NEEDS_REVIEW:
      if (receipt.failureCode !== undefined) {
        throw new DaemonProtocolError("interrupted artifact receipt contains a failure code");
      }
      return {
        artifactId: receipt.artifactId,
        contentVersion: receipt.contentVersion,
        status: "interrupted_needs_review",
      };
    case ArtifactOpenReceiptStatus.ARTIFACT_OPEN_RECEIPT_STATUS_UNSPECIFIED:
    case ArtifactOpenReceiptStatus.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("invalid daemon artifact open receipt status");
  }
}

export function mapRemovedArtifactOperation(
  operation: import("../generated/daemon/v1/daemon.js").ArtifactOperationResult,
  expectedArtifactId: string,
  expectedRevision: number,
): DaemonArtifact {
  if (operation.result?.$case !== "removedArtifact") {
    throw new DaemonProtocolError("daemon artifact removal response has the wrong result variant");
  }
  const artifact = mapArtifact(operation.result.value);
  if (
    artifact.id !== expectedArtifactId
    || artifact.state !== "deleted"
    || artifact.revision !== expectedRevision + 1
    || artifact.contentVersion !== undefined
  ) {
    throw new DaemonProtocolError("daemon artifact removal response does not match the request");
  }
  return artifact;
}

export type DaemonArtifactRemovalOutcome =
  | { status: "removed"; artifact: DaemonArtifact }
  | {
      status: "pending";
      artifactId: string;
      expectedRevision: number;
      expectedContentVersion: number;
      tombstone: DaemonArtifact;
    };

export function mapArtifactRemovalOperation(
  operation: import("../generated/daemon/v1/daemon.js").ArtifactOperationResult,
  expectedArtifactId: string,
  expectedRevision: number,
  expectedContentVersion: number,
): DaemonArtifactRemovalOutcome {
  if (operation.result?.$case === "removedArtifact") {
    return {
      status: "removed",
      artifact: mapRemovedArtifactOperation(operation, expectedArtifactId, expectedRevision),
    };
  }
  if (operation.result?.$case !== "removalPending") {
    throw new DaemonProtocolError("daemon artifact removal response has the wrong result variant");
  }
  const receipt = operation.result.value;
  const receiptRevision = safeNumber(
    receipt.expectedRevision,
    "artifact removal pending expected revision",
  );
  if (
    receipt.artifactId !== expectedArtifactId
    || receiptRevision !== expectedRevision
    || receipt.expectedContentVersion !== expectedContentVersion
    || receiptRevision !== receipt.expectedContentVersion
    || !receipt.tombstone
  ) {
    throw new DaemonProtocolError("daemon artifact removal pending receipt does not match the request");
  }
  const tombstone = mapArtifact(receipt.tombstone);
  if (
    tombstone.id !== expectedArtifactId
    || tombstone.state !== "deleted"
    || tombstone.revision !== expectedRevision + 1
    || tombstone.contentVersion !== undefined
  ) {
    throw new DaemonProtocolError("daemon artifact removal pending tombstone does not match the request");
  }
  return {
    status: "pending",
    artifactId: receipt.artifactId,
    expectedRevision: receiptRevision,
    expectedContentVersion: receipt.expectedContentVersion,
    tombstone,
  };
}

function mapArtifactOpenFailureCode(
  failureCode: ArtifactOpenFailureCode | undefined,
): Extract<DaemonArtifactOpenReceipt, { status: "failed" }>["failureCode"] {
  switch (failureCode) {
    case ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_CONTENT_UNAVAILABLE:
      return "content_unavailable";
    case ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_PLATFORM_UNAVAILABLE:
      return "platform_unavailable";
    case ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_DEADLINE_EXCEEDED:
      return "deadline_exceeded";
    case ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_INTEGRITY_FAILURE:
      return "integrity_failure";
    case ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_INTERRUPTED_BEFORE_DISPATCH:
      return "interrupted_before_dispatch";
    case undefined:
      throw new DaemonProtocolError("failed artifact receipt is missing its failure code");
    case ArtifactOpenFailureCode.ARTIFACT_OPEN_FAILURE_CODE_UNSPECIFIED:
    case ArtifactOpenFailureCode.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("invalid daemon artifact open failure code");
  }
}

export function mapWorkspaceSearchResults(
  results: import("../generated/daemon/v1/daemon.js").WorkspaceSearchResults,
  requestedOffset: number,
  requestedLimit: number,
): DaemonWorkspaceSearchResults {
  if (
    !Number.isSafeInteger(requestedOffset)
    || requestedOffset < 0
    || requestedOffset > 10_000
    || !Number.isSafeInteger(requestedLimit)
    || requestedLimit < 1
    || requestedLimit > MAX_WORKSPACE_SEARCH_RESULTS
    || results.hits.length > requestedLimit
  ) {
    throw new DaemonProtocolError("daemon workspace search response is outside the requested bounds");
  }
  const seen = new Set<string>();
  const hits = results.hits.map((hit) => {
    const id = boundedSearchString(hit.id, "workspace search hit id", 128);
    const projectId = boundedSearchString(hit.projectId, "workspace search project id", 128);
    const kind = workspaceSearchKindFromWire(hit.kind);
    const threadId = hit.threadId
      ? boundedSearchString(hit.threadId, "workspace search thread id", 128)
      : undefined;
    if ((kind === "thread" || kind === "message") && !threadId) {
      throw new DaemonProtocolError("daemon workspace search result is missing its conversation route");
    }
    if (kind === "thread" && threadId !== id) {
      throw new DaemonProtocolError("daemon thread search result has an inconsistent conversation route");
    }
    if (kind === "project" && projectId !== id) {
      throw new DaemonProtocolError("daemon project search result has inconsistent ownership");
    }
    if ((kind === "project" || kind === "automation") && threadId) {
      throw new DaemonProtocolError("daemon workspace search result has an unexpected conversation route");
    }
    const identity = `${kind}:${id}`;
    if (seen.has(identity)) throw new DaemonProtocolError("daemon workspace search response repeated a result");
    seen.add(identity);
    return {
      id,
      projectId,
      threadId,
      kind,
      title: boundedSearchString(hit.title, "workspace search title", 512),
      snippet: boundedSearchString(hit.snippet, "workspace search snippet", 512, true, true),
      updatedAtUnixMs: safeNumber(hit.updatedAtUnixMs, "workspace search update time"),
    };
  });
  if (results.hasMore) {
    if (
      hits.length !== requestedLimit
      || results.nextOffset !== requestedOffset + requestedLimit
      || results.nextOffset > 10_000
    ) {
      throw new DaemonProtocolError("daemon workspace search cursor is invalid");
    }
    return { hits, nextOffset: results.nextOffset, hasMore: true };
  }
  if (results.nextOffset !== 0) {
    throw new DaemonProtocolError("daemon workspace search returned an unexpected terminal cursor");
  }
  return { hits, hasMore: false };
}

function workspaceSearchKindFromWire(
  value: WorkspaceSearchKind,
): import("../../src/contracts/bridge.js").DaemonWorkspaceSearchKind {
  const values: Partial<Record<WorkspaceSearchKind, import("../../src/contracts/bridge.js").DaemonWorkspaceSearchKind>> = {
    [WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_PROJECT]: "project",
    [WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_THREAD]: "thread",
    [WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_MESSAGE]: "message",
    [WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_ARTIFACT]: "artifact",
    [WorkspaceSearchKind.WORKSPACE_SEARCH_KIND_AUTOMATION]: "automation",
  };
  const kind = values[value];
  if (!kind) throw new DaemonProtocolError(`invalid workspace search kind ${value}`);
  return kind;
}

export function mapListedAutomation(
  automation: import("../generated/daemon/v1/daemon.js").Automation,
  expectedProjectId: string,
): DaemonAutomation {
  const mapped = mapAutomation(automation);
  if (mapped.projectId !== expectedProjectId) {
    throw new DaemonProtocolError("daemon automation project does not match the requested project");
  }
  return mapped;
}

function mapManagedIntegration(integration: import("../generated/daemon/v1/daemon.js").ManagedIntegration): {
  id: string;
  state: string;
  installedVersion: string;
  availableVersion: string;
  rollbackVersion: string;
  revision: number;
  signatureVerified: boolean;
} {
  return {
    id: boundedString(integration.id, "managed integration id"),
    state: boundedString(integration.state, "managed integration state", 64),
    installedVersion: boundedString(integration.installedVersion ?? "", "installed version", 128),
    availableVersion: boundedString(integration.availableVersion ?? "", "available version", 128),
    rollbackVersion: boundedString(integration.rollbackVersion ?? "", "rollback version", 128),
    revision: safeNumber(integration.revision, "managed integration revision"),
    signatureVerified: integration.signatureVerified === true,
  };
}

function mapAutomation(automation: import("../generated/daemon/v1/daemon.js").Automation): DaemonAutomation {
  return {
    id: boundedString(automation.id, "automation id"),
    projectId: boundedString(automation.projectId, "automation project id"),
    title: boundedString(automation.title, "automation title", 512),
    prompt: boundedString(automation.prompt, "automation prompt", 64 * 1024),
    schedule: boundedString(automation.schedule, "automation schedule", 512),
    timezone: boundedString(automation.timezone, "automation timezone", 256),
    missedRunPolicy: missedRunPolicyFromWire(automation.missedRunPolicy),
    overlapPolicy: overlapPolicyFromWire(automation.overlapPolicy),
    state: automationStateFromWire(automation.state),
    revision: safeNumber(automation.revision, "automation revision"),
    createdAtUnixMs: safeNumber(automation.createdAtUnixMs, "automation creation time"),
    updatedAtUnixMs: safeNumber(automation.updatedAtUnixMs, "automation update time"),
  };
}

function automationToWire(input: DaemonAutomationInput) {
  return {
    projectId: input.projectId,
    title: input.title,
    prompt: input.prompt,
    schedule: input.schedule,
    timezone: input.timezone,
    missedRunPolicy: input.missedRunPolicy === "run_once"
      ? MissedRunPolicy.MISSED_RUN_POLICY_RUN_ONCE
      : MissedRunPolicy.MISSED_RUN_POLICY_SKIP,
    overlapPolicy: input.overlapPolicy === "queue_one"
      ? OverlapPolicy.OVERLAP_POLICY_QUEUE_ONE
      : OverlapPolicy.OVERLAP_POLICY_SKIP,
    scheduleActive: input.scheduleActive === true,
  };
}

function mapRun(run: Run): DaemonRun {
  return {
    id: boundedString(run.id, "run id"),
    projectId: boundedString(run.projectId, "project id"),
    threadId: boundedString(run.threadId, "thread id"),
    state: runStateFromWire(run.state),
    revision: safeNumber(run.revision, "run revision"),
    createdAtUnixMs: safeNumber(run.createdAtUnixMs, "run creation time"),
    updatedAtUnixMs: safeNumber(run.updatedAtUnixMs, "run update time"),
    kind: runKindFromWire(run.kind),
    ...(run.workBackend === WorkExecutionBackend.WORK_EXECUTION_BACKEND_UNSPECIFIED
      ? {}
      : { workBackend: workBackendFromWire(run.workBackend) }),
  };
}

function mapHostExecutionPolicy(policy: HostExecutionPolicy): DaemonHostExecutionPolicy {
  return {
    revision: safeNumber(policy.revision, "Host Tools policy revision"),
    active: policy.active,
    acknowledgmentVersion: policy.acknowledgmentVersion,
    requiredAcknowledgmentVersion: policy.requiredAcknowledgmentVersion,
    acknowledgedAtUnixMs: safeNumber(policy.acknowledgedAtUnixMs, "Host Tools acknowledgment time"),
    filesystemRead: policy.filesystemRead,
    filesystemWrite: policy.filesystemWrite,
    processExecute: policy.processExecute,
    pathRoots: policy.pathRoots.map((root) => boundedString(root, "Host Tools path root", 4096)),
    broadScopeAcknowledged: policy.broadScopeAcknowledged,
    updatedAtUnixMs: safeNumber(policy.updatedAtUnixMs, "Host Tools policy update time"),
    runtimePrepared: policy.runtimePrepared,
    unavailableReasonCode: policy.unavailableReasonCode
      ? boundedString(policy.unavailableReasonCode, "Host Tools unavailable reason", 128)
      : "",
  };
}

function mapHostWorkSnapshot(snapshot: HostWorkSnapshot): DaemonHostWorkSnapshot {
  if (!snapshot.run) throw new DaemonProtocolError("Host Work snapshot is missing its run");
  const run = mapRun(snapshot.run);
  if (run.kind !== "work" || run.workBackend !== "host_direct") {
    throw new DaemonProtocolError("Host Work snapshot has an invalid backend binding");
  }
  return {
    run,
    ...(snapshot.pendingApproval ? { pendingApproval: mapApproval(snapshot.pendingApproval) } : {}),
  };
}

function workExecutionModeFromWire(
  backend: WorkExecutionBackend,
): import("../../src/contracts/bridge.js").DaemonWorkExecutionMode {
  if (backend === WorkExecutionBackend.WORK_EXECUTION_BACKEND_UNSPECIFIED) return "limited";
  return workBackendFromWire(backend);
}

function workBackendFromWire(backend: WorkExecutionBackend): "host_direct" | "isolated_guest" {
  if (backend === WorkExecutionBackend.WORK_EXECUTION_BACKEND_HOST_DIRECT) return "host_direct";
  if (backend === WorkExecutionBackend.WORK_EXECUTION_BACKEND_ISOLATED_GUEST) return "isolated_guest";
  throw new DaemonProtocolError("invalid Work execution backend");
}

function runKindFromWire(kind: RunKind): DaemonRun["kind"] {
  if (kind === RunKind.RUN_KIND_UNSPECIFIED) return "unspecified";
  if (kind === RunKind.RUN_KIND_CHAT) return "chat";
  if (kind === RunKind.RUN_KIND_WORK) return "work";
  if (kind === RunKind.RUN_KIND_SCHEDULED) return "scheduled";
  throw new DaemonProtocolError("invalid run kind");
}

function mapRunEvent(event: RunEvent): DaemonRunEvent {
  const base = {
    sequence: safeNumber(event.sequence, "run event sequence"),
    runId: boundedString(event.runId, "run event run id", 128),
    occurredAtUnixMs: safeNumber(event.occurredAtUnixMs, "run event time"),
  };
  switch (event.kind) {
    case RunEventKind.RUN_EVENT_KIND_CREATED:
      return { ...base, kind: "created" };
    case RunEventKind.RUN_EVENT_KIND_STATE_CHANGED:
      return {
        ...base,
        kind: "state_changed",
        fromState: runStateFromWire(event.fromState),
        toState: runStateFromWire(event.toState),
      };
    case RunEventKind.RUN_EVENT_KIND_APPROVAL_REQUESTED:
      return {
        ...base,
        kind: "approval_requested",
        relatedId: boundedString(event.relatedId, "run event approval id", 128),
      };
    case RunEventKind.RUN_EVENT_KIND_EFFECT_PREPARED:
      return {
        ...base,
        kind: "effect_prepared",
        relatedId: boundedString(event.relatedId, "run event effect id", 128),
      };
    case RunEventKind.RUN_EVENT_KIND_EFFECT_NEEDS_REVIEW:
      return {
        ...base,
        kind: "effect_needs_review",
        relatedId: boundedString(event.relatedId, "run event effect id", 128),
      };
    case RunEventKind.RUN_EVENT_KIND_UNSPECIFIED:
    case RunEventKind.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("invalid daemon run event kind");
  }
}

function mapConversationTurnEvent(event: ConversationTurnEvent) {
  const base = {
    sequence: safeNumber(event.sequence, "conversation event sequence"),
    turnId: boundedString(event.turnId, "conversation event turn id", 128),
  };
  switch (event.kind) {
    case ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_CREATED:
      return { ...base, kind: "created" as const };
    case ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_STATE_CHANGED:
      return {
        ...base,
        kind: "state_changed" as const,
        fromState: conversationTurnStateFromWire(event.fromState),
        toState: conversationTurnStateFromWire(event.toState),
      };
    case ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_TEXT_APPENDED:
      return {
        ...base,
        kind: "text_appended" as const,
        startUtf8Offset: safeNumber(event.startUtf8Offset, "conversation event text offset"),
        text: boundedConversationEventText(event.textAppended),
      };
    case ConversationTurnEventKind.CONVERSATION_TURN_EVENT_KIND_UNSPECIFIED:
    case ConversationTurnEventKind.UNRECOGNIZED:
    default:
      throw new DaemonProtocolError("invalid daemon conversation event kind");
  }
}

export function mapApprovalDecisionResponse(
  approval: import("../generated/daemon/v1/daemon.js").Approval,
  requestedApprovalId: string,
  expectedRevision: number,
  approved: boolean,
): DaemonApproval {
  const mapped = mapApproval(approval);
  const expectedStatus = approved ? "granted" : "denied";
  if (
    mapped.id !== requestedApprovalId
    || mapped.revision !== expectedRevision + 1
    || mapped.status !== expectedStatus
  ) {
    throw new DaemonProtocolError("daemon approval decision response is inconsistent with the requested intent");
  }
  return mapped;
}

function mapApproval(approval: import("../generated/daemon/v1/daemon.js").Approval): DaemonApproval {
  if (!approval.action) throw new DaemonProtocolError("approval action is missing");
  const scope = approvalScopeFromWire(approval.scope);
  const resourceId = approval.resourceId
    ? boundedSearchString(approval.resourceId, "approval resource id", 128)
    : undefined;
  if ((scope === "resource") !== Boolean(resourceId)) {
    throw new DaemonProtocolError("approval scope and resource identity are inconsistent");
  }
  return {
    id: boundedString(approval.id, "approval id"),
    runId: boundedString(approval.runId, "approval run id"),
    status: approvalStatusFromWire(approval.status),
    revision: safeNumber(approval.revision, "approval revision"),
    action: {
      action: boundedString(approval.action.action, "approval action"),
      target: boundedString(approval.action.target, "approval target"),
      dataSummary: boundedString(approval.action.dataSummary, "approval data summary", 4_096),
      risk: approvalRiskFromWire(approval.action.risk),
    },
    scope,
    resourceId,
    expiresAtUnixMs: safeNumber(approval.expiresAtUnixMs, "approval expiry"),
  };
}

function mapCapability(status: CapabilityStatus): DaemonCapabilityStatus {
  const id = capabilityId(status.capability);
  return {
    id,
    label: capabilityLabel(id),
    source: capabilitySource(status.surface),
    authentication: authentication(status.authentication),
    availability: availability(status.availability),
    reasonCode: boundedString(status.reasonCode, "capability reason code"),
    reason: boundedString(status.reason, "capability reason", 1_024),
  };
}

function mapAgentRuntime(runtime: import("../generated/daemon/v1/daemon.js").AgentRuntimeHealth | undefined): DaemonStatus["agentRuntime"] {
  if (!runtime) return undefined;
  return {
    configured: runtime.configured,
    healthy: runtime.healthy,
    protocolVersion: runtime.protocolVersion,
    name: runtime.agentName ? boundedString(runtime.agentName, "agent runtime name", 128) : "Grok Build",
    version: runtime.agentVersion ? boundedString(runtime.agentVersion, "agent runtime version", 128) : "Unknown",
    reasonCode: runtime.reasonCode ? boundedString(runtime.reasonCode, "agent runtime reason code", 128) : "unknown",
    authMethods: runtime.authMethods.slice(0, 16).map((method) => ({
      id: boundedString(method.id, "agent auth method id", 128),
      name: boundedString(method.name, "agent auth method name", 128),
      description: method.description ? boundedString(method.description, "agent auth method description", 512) : "",
    })),
    capabilities: {
      loadSession: runtime.capabilities?.loadSession ?? false,
      embeddedContext: runtime.capabilities?.embeddedContext ?? false,
      imageInput: runtime.capabilities?.imageInput ?? false,
      audioInput: runtime.capabilities?.audioInput ?? false,
      mcpHttp: runtime.capabilities?.mcpHttp ?? false,
      mcpSse: runtime.capabilities?.mcpSse ?? false,
    },
  };
}

export function mapAutomationSchedulerHealth(value: AutomationSchedulerHealth): NonNullable<DaemonStatus["automationScheduler"]> {
  if (value === AutomationSchedulerHealth.AUTOMATION_SCHEDULER_HEALTH_KERNEL_INITIALIZED_EXECUTION_DISABLED) {
    return { state: "kernel_initialized_execution_disabled" };
  }
  if (value === AutomationSchedulerHealth.AUTOMATION_SCHEDULER_HEALTH_KERNEL_INITIALIZED_EXECUTION_ENABLED) {
    return { state: "kernel_initialized_execution_enabled" };
  }
  if (value === AutomationSchedulerHealth.AUTOMATION_SCHEDULER_HEALTH_RECOVERY_PENDING_EXECUTION_DISABLED) {
    return { state: "recovery_pending_execution_disabled" };
  }
  if (value === AutomationSchedulerHealth.AUTOMATION_SCHEDULER_HEALTH_DEGRADED_EXECUTION_DISABLED) {
    return { state: "degraded_execution_disabled" };
  }
  throw new DaemonProtocolError(`invalid automation scheduler health ${value}`);
}

function projectStateFromWire(value: ProjectState): DaemonProject["state"] {
  if (value === ProjectState.PROJECT_STATE_ACTIVE) return "active";
  if (value === ProjectState.PROJECT_STATE_ARCHIVED) return "archived";
  throw new DaemonProtocolError(`invalid daemon project state ${value}`);
}

function threadStateFromWire(value: ThreadState): DaemonThread["state"] {
  if (value === ThreadState.THREAD_STATE_OPEN) return "open";
  if (value === ThreadState.THREAD_STATE_ARCHIVED) return "archived";
  throw new DaemonProtocolError(`invalid daemon thread state ${value}`);
}

function messageRoleFromWire(value: MessageRole): DaemonMessage["role"] {
  const roles: Partial<Record<MessageRole, DaemonMessage["role"]>> = {
    [MessageRole.MESSAGE_ROLE_SYSTEM]: "system",
    [MessageRole.MESSAGE_ROLE_USER]: "user",
    [MessageRole.MESSAGE_ROLE_ASSISTANT]: "assistant",
  };
  const role = roles[value];
  if (!role) throw new DaemonProtocolError(`invalid daemon message role ${value}`);
  return role;
}

function messageStateFromWire(value: MessageState): DaemonMessage["state"] {
  if (value === MessageState.MESSAGE_STATE_ACTIVE) return "active";
  if (value === MessageState.MESSAGE_STATE_DELETED) return "deleted";
  throw new DaemonProtocolError(`invalid daemon message state ${value}`);
}

function artifactStateFromWire(value: ArtifactState): DaemonArtifact["state"] {
  if (value === ArtifactState.ARTIFACT_STATE_UNAVAILABLE) return "unavailable";
  if (value === ArtifactState.ARTIFACT_STATE_AVAILABLE) return "available";
  if (value === ArtifactState.ARTIFACT_STATE_DELETED) return "deleted";
  throw new DaemonProtocolError(`invalid daemon artifact state ${value}`);
}

function automationStateFromWire(value: AutomationState): DaemonAutomation["state"] {
  const states: Partial<Record<AutomationState, DaemonAutomation["state"]>> = {
    [AutomationState.AUTOMATION_STATE_ENABLED]: "enabled",
    [AutomationState.AUTOMATION_STATE_DISABLED]: "disabled",
    [AutomationState.AUTOMATION_STATE_ARCHIVED]: "archived",
  };
  const state = states[value];
  if (!state) throw new DaemonProtocolError(`invalid daemon automation state ${value}`);
  return state;
}

function missedRunPolicyFromWire(value: MissedRunPolicy): DaemonAutomation["missedRunPolicy"] {
  if (value === MissedRunPolicy.MISSED_RUN_POLICY_RUN_ONCE) return "run_once";
  if (value === MissedRunPolicy.MISSED_RUN_POLICY_SKIP) return "skip";
  throw new DaemonProtocolError(`invalid daemon missed-run policy ${value}`);
}

function overlapPolicyFromWire(value: OverlapPolicy): DaemonAutomation["overlapPolicy"] {
  if (value === OverlapPolicy.OVERLAP_POLICY_QUEUE_ONE) return "queue_one";
  if (value === OverlapPolicy.OVERLAP_POLICY_SKIP) return "skip";
  throw new DaemonProtocolError(`invalid daemon overlap policy ${value}`);
}

function runStateFromWire(value: RunState): DaemonRunState {
  const values: Partial<Record<RunState, DaemonRunState>> = {
    [RunState.RUN_STATE_QUEUED]: "queued",
    [RunState.RUN_STATE_PLANNING]: "planning",
    [RunState.RUN_STATE_AWAITING_APPROVAL]: "awaiting_approval",
    [RunState.RUN_STATE_RUNNING]: "running",
    [RunState.RUN_STATE_PAUSED]: "paused",
    [RunState.RUN_STATE_COMPLETED]: "completed",
    [RunState.RUN_STATE_FAILED]: "failed",
    [RunState.RUN_STATE_CANCELLED]: "cancelled",
    [RunState.RUN_STATE_INTERRUPTED_NEEDS_REVIEW]: "interrupted_needs_review",
  };
  const state = values[value];
  if (!state) throw new DaemonProtocolError(`invalid daemon run state ${value}`);
  return state;
}

function conversationTurnStateFromWire(value: ConversationTurnState): DaemonConversationTurn["state"] {
  const values: Partial<Record<ConversationTurnState, DaemonConversationTurn["state"]>> = {
    [ConversationTurnState.CONVERSATION_TURN_STATE_RESERVED]: "reserved",
    [ConversationTurnState.CONVERSATION_TURN_STATE_PROVIDER_STARTED]: "provider_started",
    [ConversationTurnState.CONVERSATION_TURN_STATE_COMPLETED]: "completed",
    [ConversationTurnState.CONVERSATION_TURN_STATE_FAILED]: "failed",
    [ConversationTurnState.CONVERSATION_TURN_STATE_CANCELLED]: "cancelled",
    [ConversationTurnState.CONVERSATION_TURN_STATE_INTERRUPTED_NEEDS_REVIEW]: "interrupted_needs_review",
  };
  const state = values[value];
  if (!state) throw new DaemonProtocolError(`invalid conversation turn state ${value}`);
  return state;
}

function conversationFailureKindFromWire(
  value: ConversationFailureKind,
): NonNullable<DaemonConversationTurn["failure"]>["kind"] {
  const values: Partial<Record<ConversationFailureKind, NonNullable<DaemonConversationTurn["failure"]>["kind"]>> = {
    [ConversationFailureKind.CONVERSATION_FAILURE_KIND_AUTHENTICATION]: "authentication",
    [ConversationFailureKind.CONVERSATION_FAILURE_KIND_FORBIDDEN]: "forbidden",
    [ConversationFailureKind.CONVERSATION_FAILURE_KIND_INVALID_REQUEST]: "invalid_request",
    [ConversationFailureKind.CONVERSATION_FAILURE_KIND_RATE_LIMITED]: "rate_limited",
    [ConversationFailureKind.CONVERSATION_FAILURE_KIND_UNAVAILABLE]: "unavailable",
    [ConversationFailureKind.CONVERSATION_FAILURE_KIND_PROTOCOL]: "protocol",
  };
  const kind = values[value];
  if (!kind) throw new DaemonProtocolError(`invalid conversation failure kind ${value}`);
  return kind;
}

function capabilityId(value: Capability): DaemonCapabilityStatus["id"] {
  const values: Partial<Record<Capability, DaemonCapabilityStatus["id"]>> = {
    [Capability.CAPABILITY_CHAT]: "chat",
    [Capability.CAPABILITY_WORK]: "work",
    [Capability.CAPABILITY_FILES]: "files",
    [Capability.CAPABILITY_SHELL]: "shell",
    [Capability.CAPABILITY_MCP]: "mcp",
    [Capability.CAPABILITY_BROWSER_AUTOMATION]: "browser_automation",
    [Capability.CAPABILITY_COMPUTER_USE]: "computer_use",
    [Capability.CAPABILITY_SEARCH]: "search",
    [Capability.CAPABILITY_RESEARCH]: "research",
    [Capability.CAPABILITY_IMAGINE_IMAGE]: "imagine_image",
    [Capability.CAPABILITY_IMAGINE_VIDEO]: "imagine_video",
    [Capability.CAPABILITY_REALTIME_VOICE]: "realtime_voice",
    [Capability.CAPABILITY_AUTOMATIONS]: "automations",
  };
  const id = values[value];
  if (!id) throw new DaemonProtocolError(`invalid daemon capability ${value}`);
  return id;
}

function capabilityLabel(id: DaemonCapabilityStatus["id"]): string {
  const labels: Record<DaemonCapabilityStatus["id"], string> = {
    chat: "Grok chat", work: "Work runtime", files: "Local files", shell: "Shell tools", mcp: "MCP",
    browser_automation: "Browser automation", computer_use: "Computer use", search: "Web & X search",
    research: "Research", imagine_image: "Imagine image", imagine_video: "Imagine video",
    realtime_voice: "Realtime voice", automations: "Automations",
  };
  return labels[id];
}

function capabilitySource(value: CapabilitySurface): DaemonCapabilityStatus["source"] {
  const values: Partial<Record<CapabilitySurface, DaemonCapabilityStatus["source"]>> = {
    [CapabilitySurface.CAPABILITY_SURFACE_SUBSCRIPTION_ACP]: "subscription_acp",
    [CapabilitySurface.CAPABILITY_SURFACE_XAI_API]: "xai_api",
    [CapabilitySurface.CAPABILITY_SURFACE_DESKTOP]: "desktop",
    [CapabilitySurface.CAPABILITY_SURFACE_MANAGED_ADDON]: "managed_addon",
    [CapabilitySurface.CAPABILITY_SURFACE_WEB_HANDOFF]: "web_handoff",
  };
  const source = values[value];
  if (!source) throw new DaemonProtocolError(`invalid capability surface ${value}`);
  return source;
}

function authentication(value: AuthMethod): DaemonCapabilityStatus["authentication"] {
  const values: Partial<Record<AuthMethod, DaemonCapabilityStatus["authentication"]>> = {
    [AuthMethod.AUTH_METHOD_NONE]: "none",
    [AuthMethod.AUTH_METHOD_SUBSCRIPTION_OAUTH]: "subscription_oauth",
    [AuthMethod.AUTH_METHOD_XAI_API_KEY]: "xai_api_key",
    [AuthMethod.AUTH_METHOD_EITHER]: "either",
  };
  const method = values[value];
  if (!method) throw new DaemonProtocolError(`invalid authentication method ${value}`);
  return method;
}

function availability(value: CapabilityAvailability): DaemonCapabilityStatus["availability"] {
  const values: Partial<Record<CapabilityAvailability, DaemonCapabilityStatus["availability"]>> = {
    [CapabilityAvailability.CAPABILITY_AVAILABILITY_AVAILABLE]: "available",
    [CapabilityAvailability.CAPABILITY_AVAILABILITY_LIMITED]: "limited",
    [CapabilityAvailability.CAPABILITY_AVAILABILITY_UNAVAILABLE]: "unavailable",
  };
  const result = values[value];
  if (!result) throw new DaemonProtocolError(`invalid capability availability ${value}`);
  return result;
}

function approvalRiskFromWire(value: ApprovalRisk): DaemonApproval["action"]["risk"] {
  const values: Partial<Record<ApprovalRisk, DaemonApproval["action"]["risk"]>> = {
    [ApprovalRisk.APPROVAL_RISK_LOW]: "low",
    [ApprovalRisk.APPROVAL_RISK_ELEVATED]: "elevated",
    [ApprovalRisk.APPROVAL_RISK_HIGH]: "high",
    [ApprovalRisk.APPROVAL_RISK_CRITICAL]: "critical",
  };
  const risk = values[value];
  if (!risk) throw new DaemonProtocolError(`invalid approval risk ${value}`);
  return risk;
}

function approvalScopeFromWire(value: ApprovalScope): DaemonApproval["scope"] {
  const values: Partial<Record<ApprovalScope, DaemonApproval["scope"]>> = {
    [ApprovalScope.APPROVAL_SCOPE_ONCE]: "once",
    [ApprovalScope.APPROVAL_SCOPE_RUN]: "run",
    [ApprovalScope.APPROVAL_SCOPE_RESOURCE]: "resource",
  };
  const scope = values[value];
  if (!scope) throw new DaemonProtocolError(`invalid approval scope ${value}`);
  return scope;
}

function approvalStatusFromWire(value: ApprovalStatus): DaemonApproval["status"] {
  const values: Partial<Record<ApprovalStatus, DaemonApproval["status"]>> = {
    [ApprovalStatus.APPROVAL_STATUS_PENDING]: "pending",
    [ApprovalStatus.APPROVAL_STATUS_GRANTED]: "granted",
    [ApprovalStatus.APPROVAL_STATUS_DENIED]: "denied",
    [ApprovalStatus.APPROVAL_STATUS_EXPIRED]: "expired",
    [ApprovalStatus.APPROVAL_STATUS_CANCELLED]: "cancelled",
  };
  const status = values[value];
  if (!status) throw new DaemonProtocolError(`invalid approval status ${value}`);
  return status;
}

function safeNumber(value: bigint, field: string): number {
  if (value < 0n || value > BigInt(Number.MAX_SAFE_INTEGER)) {
    throw new DaemonProtocolError(`${field} is outside the safe integer range`);
  }
  return Number(value);
}

function boundedString(value: string, field: string, maximum = 512): string {
  if (!value || Buffer.byteLength(value, "utf8") > maximum) {
    throw new DaemonProtocolError(`${field} is invalid`);
  }
  return value;
}

function boundedHttpsUrl(value: string, field: string, maximum: number): string {
  let parsed: URL;
  try {
    parsed = new URL(value);
  } catch {
    throw new DaemonProtocolError(`${field} is invalid`);
  }
  if (
    Buffer.byteLength(value, "utf8") > maximum
    || !value.startsWith("https://")
    || Array.from(value).some((character) => {
      const point = character.codePointAt(0) ?? 0;
      return character.trim().length === 0
        || point <= 0x1f
        || (point >= 0x7f && point <= 0x9f);
    })
    || value.includes("@")
    || parsed.protocol !== "https:"
    || !parsed.hostname
    || parsed.username
    || parsed.password
  ) {
    throw new DaemonProtocolError(`${field} is invalid`);
  }
  return value;
}

function boundedModelIdentifier(value: string, field: string): string {
  if (
    !value
    || Buffer.byteLength(value, "utf8") > 512
    || value.trim() !== value
    || Array.from(value).some((character) => {
      const point = character.codePointAt(0) ?? 0;
      return point <= 0x1f || (point >= 0x7f && point <= 0x9f);
    })
  ) {
    throw new DaemonProtocolError(`${field} is invalid`);
  }
  return value;
}

function boundedModelModality(value: string): string {
  if (
    !value
    || Buffer.byteLength(value, "utf8") > 64
    || value.trim() !== value
    || Array.from(value).some((character) => {
      const point = character.codePointAt(0) ?? 0;
      return point <= 0x1f || (point >= 0x7f && point <= 0x9f);
    })
  ) {
    throw new DaemonProtocolError("chat model modality is invalid");
  }
  return value;
}

function boundedText(value: string, field: string, maximum: number): string {
  if (Buffer.byteLength(value, "utf8") > maximum) {
    throw new DaemonProtocolError(`${field} is invalid`);
  }
  return value;
}

function boundedConversationEventText(value: string): string {
  if (
    Buffer.byteLength(value, "utf8") < 1
    || Buffer.byteLength(value, "utf8") > 16 * 1024
    || Array.from(value).some((character) => {
      const point = character.codePointAt(0) ?? 0;
      return character === "\0"
        || (point < 0x20 && character !== "\n" && character !== "\r" && character !== "\t");
    })
  ) {
    throw new DaemonProtocolError("conversation event text is invalid");
  }
  return value;
}

function boundedSearchString(
  value: string,
  field: string,
  maximum: number,
  allowEmpty = false,
  multiline = false,
): string {
  if (
    (!allowEmpty && value.trim().length === 0)
    || Buffer.byteLength(value, "utf8") > maximum
    || Array.from(value).some((character) => {
      const point = character.codePointAt(0) ?? 0;
      if (multiline && (character === "\n" || character === "\r" || character === "\t")) return false;
      return point <= 0x1f || (point >= 0x7f && point <= 0x9f);
    })
  ) {
    throw new DaemonProtocolError(`${field} is invalid`);
  }
  return value;
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
}

function stopChild(child: ChildProcess): Promise<void> {
  return new Promise((resolve) => {
    const force = setTimeout(() => {
      child.kill("SIGKILL");
      resolve();
    }, 2_000);
    child.once("exit", () => {
      clearTimeout(force);
      resolve();
    });
    child.kill("SIGTERM");
  });
}
