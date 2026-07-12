import type {
  BridgeResponse,
  DaemonAccountState,
  DaemonArtifact,
  DaemonAutomation,
  DaemonCapabilityStatus,
  DaemonChatModelCatalog,
  DaemonChatModelPreference,
  DaemonConversationFork,
  DaemonConversationForkMetadata,
  DaemonConversationTurn,
  DaemonDesktopPreferences,
  DaemonMessage,
  DaemonProject,
  DaemonStatus,
  DaemonThread,
  DaemonWorkspaceSnapshot,
  DesktopBridge,
  DesktopConversationTurnEventNotification,
} from "../contracts/bridge";
import type {
  AccountSetupState,
  ArtifactOpenResult,
  ArtifactRemovalResult,
  AutomationDraft,
  AutomationRunRecord,
  AutomationSchedule,
  AutomationSummary,
  CapabilityStatus,
  ChatModelCatalog,
  ChatModelPreference,
  ClientResult,
  ConversationAttachment,
  ConversationDetail,
  ConversationMessage,
  ConversationTurnDetail,
  CreateProjectInput,
  DesktopClient,
  DesktopPreferences,
  DesktopSnapshot,
  LibraryItem,
  ManagedIntegrationDetail,
  MediaCreation,
  StartRunInput,
  VoiceSession,
  VoiceSetup,
  WorkspaceSearchResults,
} from "./desktopClient";
import {
  applyConversationEventNotification,
  isTerminalConversationState,
  type ConversationEventProjection,
} from "./conversationEventProjection";
import {
  AUTOMATION_DEFINITION_ONLY_REASON,
  GROK_BUILD_AUTH_UNAVAILABLE_REASON,
  GROK_EXECUTION_UNAVAILABLE_REASON,
} from "./productAvailability";

const initialSnapshot = (): DesktopSnapshot => ({
  connection: { state: "connecting", profile: "Local workspace", plan: "Connecting" },
  capabilities: [],
  projects: [],
  runs: [],
  threads: [],
  library: [],
  automations: [],
  extensions: [],
});

const TURN_OWNERSHIP_WAIT_MS = 3_000;
const MAX_TERMINAL_PREFIX_COUNT = 2_000;
const MAX_TERMINAL_PREFIX_BYTES = 16 * 1024 * 1024;
const MAX_PENDING_CONVERSATION_FORK_MUTATIONS = 64;
const MAX_PENDING_ARTIFACT_REMOVAL_MUTATIONS = 64;
const ARTIFACT_REMOVAL_RECONCILIATION_DELAYS_MS = [250, 1_000, 4_000] as const;

type ConversationTerminalPrefix = Pick<
  ConversationEventProjection,
  "revision" | "state" | "text" | "textUtf8Bytes" | "turnId"
>;

type TurnThreadWaiter = {
  resolve(threadId: string): void;
  timer: ReturnType<typeof setTimeout>;
};

type NewChatMutation = {
  projectId: string;
  content: string;
  title: string;
  createThreadIdempotencyKey: string;
  startTurnIdempotencyKey: string;
  thread?: DaemonThread;
};

type ArtifactRemovalMutation = {
  expectedRevision: number;
  expectedContentVersion: number;
  expectedProjectId: string;
  expectedThreadId?: string;
  expectedName: string;
  expectedCreatedAtUnixMs: number;
  expectedUpdatedAtUnixMs: number;
  idempotencyKey: string;
  phase: "dispatching" | "ambiguous" | "daemon_pending";
  reconciliationAttempt: number;
};

/** Production renderer adapter; all mutable operations cross the preload bridge. */
export class ElectronDesktopClient implements DesktopClient {
  private snapshot = initialSnapshot();
  private readonly listeners = new Set<() => void>();
  private readonly projects = new Map<string, DaemonProject>();
  private readonly threads = new Map<string, DaemonThread>();
  private readonly artifacts = new Map<string, DaemonArtifact>();
  private readonly automations = new Map<string, DaemonAutomation>();
  private readonly conversations = new Map<string, ConversationDetail>();
  private readonly canonicalConversationMessages = new Map<string, DaemonMessage[]>();
  private readonly conversationListeners = new Map<string, Set<(conversation: ConversationDetail) => void>>();
  private readonly conversationEventProjections = new Map<string, ConversationEventProjection>();
  private readonly conversationTerminalPrefixes = new Map<string, ConversationTerminalPrefix>();
  private conversationTerminalPrefixBytes = 0;
  private readonly turnThreads = new Map<string, string>();
  private readonly turnThreadWaiters = new Map<string, Set<TurnThreadWaiter>>();
  private readonly conversationCancellationMutations = new Map<string, {
    expectedRevision: number;
    idempotencyKey: string;
  }>();
  private readonly conversationRetryMutations = new Map<string, {
    expectedRevision: number;
    idempotencyKey: string;
  }>();
  private readonly conversationForkMutations = new Map<string, {
    expectedRevision: number;
    idempotencyKey: string;
  }>();
  private readonly conversationForkDeliveryMutations = new Map<string, {
    expectedRevision: 0;
    idempotencyKey: string;
  }>();
  private readonly conversationStartMutations = new Map<string, {
    content: string;
    idempotencyKey: string;
  }>();
  private newChatMutation: NewChatMutation | undefined;
  private accountState: DaemonAccountState = {
    xaiApiKeyConfigured: false,
    xaiCapabilitiesResolved: false,
  };
  private bootstrapPromise: Promise<void> | undefined;
  private bootstrapped = false;
  private enrollmentIdempotencyKey: string | undefined;
  private desktopPreferences: DesktopPreferences | undefined;
  private desktopPreferenceMutation: {
    expectedRevision: number;
    keepRunningInNotificationArea: boolean;
    idempotencyKey: string;
  } | undefined;
  private chatModelMutation: {
    expectedRevision: number;
    modelId: string;
    idempotencyKey: string;
  } | undefined;
  private readonly artifactRemovalMutations = new Map<string, ArtifactRemovalMutation>();
  private readonly artifactRemovalReconciliationTimers = new Map<string, {
    mutation: ArtifactRemovalMutation;
    timer: ReturnType<typeof setTimeout>;
  }>();

  constructor(private readonly bridge: DesktopBridge) {
    bridge.onDaemonStatus((status) => this.applyStatus(status));
    bridge.onConversationTurnEvents((notification) => this.handleConversationTurnEvents(notification));
  }

  async getSnapshot(): Promise<DesktopSnapshot> {
    await this.ensureBootstrap();
    return structuredClone(this.snapshot);
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  async startRun(input: StartRunInput): Promise<{ runId: string; threadId: string }> {
    await this.ensureBootstrap();
    if (input.mode !== "chat") throw new Error(GROK_EXECUTION_UNAVAILABLE_REASON);
    if (input.searchEnabled || input.researchEnabled) {
      throw new Error("Search and Research are not connected to the durable Chat execution path.");
    }
    this.assertChatAvailable();
    const project = input.projectId
      ? this.projects.get(input.projectId)
      : [...this.projects.values()].find((item) => item.state === "active");
    if (!project || project.state !== "active") {
      throw new Error(input.projectId
        ? "The selected project is no longer available."
        : "Create or select a project before starting Chat.");
    }
    const content = validatedPrompt(input.prompt);
    const title = conversationTitle(content);
    const existingMutation = this.newChatMutation;
    const mutation = existingMutation
      && existingMutation.projectId === project.id
      && existingMutation.content === content
      ? existingMutation
      : {
          projectId: project.id,
          content,
          title,
          createThreadIdempotencyKey: crypto.randomUUID(),
          startTurnIdempotencyKey: crypto.randomUUID(),
        };
    this.newChatMutation = mutation;

    let thread = mutation.thread;
    if (!thread) {
      const threadResponse = await this.bridge.request({
        kind: "daemon.createThread",
        projectId: project.id,
        title: mutation.title,
        idempotencyKey: mutation.createThreadIdempotencyKey,
      });
      if (
        threadResponse.kind !== "daemon.thread"
        || threadResponse.thread.projectId !== project.id
        || threadResponse.thread.state !== "open"
      ) {
        throw new Error("invalid thread bridge response");
      }
      thread = threadResponse.thread;
      mutation.thread = thread;
    }
    this.threads.set(thread.id, thread);
    this.rebuildWorkspaceSnapshot();
    this.emit();

    const turn = await this.startConversationTurn(thread, content, mutation.startTurnIdempotencyKey);
    if (this.newChatMutation === mutation) this.newChatMutation = undefined;
    if (isTerminalConversationState(turn.state) && turn.state !== "completed") {
      throw new Error(conversationTurnReason(turn));
    }
    return { runId: turn.run.id, threadId: thread.id };
  }

  async createProject(input: CreateProjectInput): Promise<ClientResult<DesktopSnapshot["projects"][number]>> {
    await this.ensureBootstrap();
    const response = await this.bridge.request({
      kind: "daemon.createProject",
      name: input.name,
      description: input.description,
      idempotencyKey: crypto.randomUUID(),
    });
    if (response.kind !== "daemon.project") throw new Error("invalid project bridge response");
    this.projects.set(response.project.id, response.project);
    this.rebuildWorkspaceSnapshot();
    this.emit();
    return { status: "success", value: projectSummary(response.project, []) };
  }

  async importArtifact(projectId: string): Promise<ClientResult<LibraryItem>> {
    await this.ensureBootstrap();
    this.assertFilesAvailable();
    const project = this.projects.get(projectId);
    if (!project || project.state !== "active") {
      return { status: "unavailable", reason: "The selected project is no longer available." };
    }
    const response = await this.bridge.request({
      kind: "daemon.importArtifact",
      projectId,
      idempotencyKey: crypto.randomUUID(),
    });
    if (response.kind === "daemon.artifactImportCancelled") {
      return { status: "cancelled", reason: "Import cancelled." };
    }
    if (response.kind !== "daemon.artifactImported") {
      throw new Error("invalid artifact import bridge response");
    }
    const artifact = response.artifact;
    if (
      artifact.projectId !== projectId
      || artifact.state !== "available"
      || artifact.contentVersion === undefined
    ) {
      throw new Error("artifact import bridge response does not match the request");
    }
    this.artifacts.set(artifact.id, artifact);
    this.rebuildWorkspaceSnapshot();
    this.emit();
    const item = this.snapshot.library.find((candidate) => candidate.id === artifact.id);
    if (!item) throw new Error("imported artifact is not present in the canonical Library");
    return { status: "success", value: structuredClone(item) };
  }

  async openArtifact(
    artifactId: string,
    contentVersion: number,
  ): Promise<ClientResult<ArtifactOpenResult>> {
    await this.ensureBootstrap();
    this.assertFilesAvailable();
    const artifact = this.artifacts.get(artifactId);
    if (
      !artifact
      || artifact.state !== "available"
      || artifact.contentVersion !== contentVersion
    ) {
      return {
        status: "unavailable",
        reason: "The selected artifact version is no longer available.",
      };
    }
    const response = await this.bridge.request({
      kind: "daemon.openArtifact",
      artifactId,
      contentVersion,
      idempotencyKey: crypto.randomUUID(),
    });
    if (response.kind !== "daemon.artifactOpened") {
      throw new Error("invalid artifact open bridge response");
    }
    return {
      status: "success",
      value: parseArtifactOpenReceipt(response.receipt, artifactId, contentVersion),
    };
  }

  async removeArtifact(
    artifactId: string,
    expectedRevision: number,
    expectedContentVersion: number,
  ): Promise<ArtifactRemovalResult> {
    await this.ensureBootstrap();
    this.assertFilesAvailable();
    let current = this.artifacts.get(artifactId);
    let pending = this.artifactRemovalMutations.get(artifactId);
    if (!pending && this.artifactRemovalMutations.size >= MAX_PENDING_ARTIFACT_REMOVAL_MUTATIONS) {
      try {
        await this.refreshDaemonSnapshot();
      } catch {
        // Capacity remains fail-closed if canonical reconciliation is unavailable.
      }
      this.assertFilesAvailable();
      current = this.artifacts.get(artifactId);
      pending = this.artifactRemovalMutations.get(artifactId);
    }
    if (pending && (
      pending.expectedRevision !== expectedRevision
      || pending.expectedContentVersion !== expectedContentVersion
    )) {
      return {
        status: "unavailable",
        reason: "A previous removal outcome must be reconciled before this artifact can be removed again.",
      };
    }
    if (pending?.phase === "dispatching") {
      return {
        status: "unavailable",
        reason: "Removal of this local copy is already in progress.",
      };
    }
    if (!pending && (
      !current
      || current.state !== "available"
      || current.revision !== expectedRevision
      || current.contentVersion !== expectedContentVersion
    )) {
      return {
        status: "unavailable",
        reason: "The selected artifact version is no longer available.",
      };
    }
    if (!pending && this.artifactRemovalMutations.size >= MAX_PENDING_ARTIFACT_REMOVAL_MUTATIONS) {
      return {
        status: "unavailable",
        reason: "Too many artifact removals have outcomes awaiting reconciliation.",
      };
    }
    let mutation = pending;
    if (!mutation) {
      if (!current) {
        return {
          status: "unavailable",
          reason: "The selected artifact version is no longer available.",
        };
      }
      mutation = {
        expectedRevision,
        expectedContentVersion,
        expectedProjectId: current.projectId,
        expectedThreadId: current.threadId,
        expectedName: current.name,
        expectedCreatedAtUnixMs: current.createdAtUnixMs,
        expectedUpdatedAtUnixMs: current.updatedAtUnixMs,
        idempotencyKey: crypto.randomUUID(),
        phase: "dispatching",
        reconciliationAttempt: 0,
      };
    }
    const reservationAcknowledged = mutation.phase === "daemon_pending";
    mutation.phase = "dispatching";
    this.artifactRemovalMutations.set(artifactId, mutation);
    try {
      const response = await this.bridge.request({
        kind: "daemon.removeArtifact",
        artifactId,
        expectedRevision,
        expectedContentVersion,
        idempotencyKey: mutation.idempotencyKey,
      });
      if (response.kind === "daemon.artifactRemovalRejected") {
        if (reservationAcknowledged) {
          mutation.phase = "daemon_pending";
          this.scheduleArtifactRemovalReconciliation(artifactId, mutation);
          return { status: "pending" };
        }
        this.deleteArtifactRemovalMutation(artifactId, mutation);
        return {
          status: "unavailable",
          reason: artifactRemovalRejectionMessage(response.reason),
        };
      }
      if (response.kind === "daemon.artifactRemovalPending") {
        if (
          response.artifactId !== artifactId
          || response.expectedRevision !== expectedRevision
          || response.expectedContentVersion !== expectedContentVersion
          || !matchesArtifactRemovalTombstone(response.tombstone, artifactId, mutation)
        ) {
          throw new Error("artifact removal pending bridge response does not match the request");
        }
        this.markArtifactRemovalPending(artifactId, mutation, response.tombstone);
        return { status: "pending" };
      }
      if (response.kind !== "daemon.artifactRemoved") {
        throw new Error("invalid artifact removal bridge response");
      }
      if (!matchesArtifactRemovalTombstone(response.artifact, artifactId, mutation)) {
        throw new Error("artifact removal bridge response does not match the request");
      }
      this.completeArtifactRemoval(artifactId, mutation, response.artifact);
      return { status: "success", value: undefined };
    } catch (error) {
      if (this.artifactRemovalMutations.get(artifactId) === mutation) {
        mutation.phase = reservationAcknowledged ? "daemon_pending" : "ambiguous";
      }
      if (await this.reconcileAmbiguousArtifactRemoval(artifactId, mutation)) {
        return { status: "pending" };
      }
      if (reservationAcknowledged) {
        this.scheduleArtifactRemovalReconciliation(artifactId, mutation);
        return { status: "pending" };
      }
      throw error;
    }
  }

  async getAccountSetup(): Promise<AccountSetupState> {
    await this.ensureBootstrap();
    const capability = (id: string) => this.snapshot.capabilities.find((item) => item.id === id);
    const work = capability("work");
    const apiKeyConfigured = this.accountState.xaiApiKeyConfigured;
    const apiReady = apiKeyConfigured && capability("chat")?.available === true;
    const runtime = this.snapshot.connection.agentRuntime;
    const grokConnected = this.accountState.grokBuildAuthenticated === true;
    const runtimeDetail = grokConnected
      ? "Official Grok Build host authentication is active"
      : runtime?.healthy
        ? `${runtime.name} ${runtime.version} is ready for host authentication`
        : runtime?.reasonCode || "Grok Build runtime is not connected";
    const workAvailable = work?.available === true;
    return {
      grokBuild: grokConnected ? "connected" : "not_connected",
      xaiApiKey: apiKeyConfigured ? "configured" : "not_configured",
      limitedMode: !workAvailable,
      checks: [
        { id: "daemon", label: "Local daemon", state: this.snapshot.connection.state === "online" ? "ready" : "unavailable", detail: this.snapshot.connection.reason ?? `Protocol ${this.snapshot.connection.serviceVersion ?? "not connected"}` },
        {
          id: "grok_auth",
          label: "Grok Build OAuth",
          state: grokConnected ? "ready" : runtime?.healthy ? "action_required" : "unavailable",
          detail: grokConnected
            ? runtimeDetail
            : runtime?.healthy
              ? `${runtimeDetail}. Connect to authenticate through the official component.`
              : `${runtimeDetail}. ${GROK_BUILD_AUTH_UNAVAILABLE_REASON}`,
        },
        { id: "xai_api", label: "xAI API key", state: apiReady ? "ready" : apiKeyConfigured ? "action_required" : "optional", detail: capability("chat")?.reason ?? "Optional for direct xAI API capabilities" },
        { id: "isolation", label: "Protected Work", state: workAvailable ? "ready" : "action_required", detail: work?.reason ?? "Protected Work is not ready" },
        { id: "browser", label: "Managed browser", state: capability("browser_automation")?.available ? "ready" : "action_required", detail: capability("browser_automation")?.reason ?? "Managed browser is not ready" },
        { id: "computer_use", label: "Computer use", state: capability("computer_use")?.available ? "ready" : "optional", detail: capability("computer_use")?.reason ?? "Optional native broker" },
      ],
    };
  }

  async getDesktopPreferences(): Promise<DesktopPreferences> {
    await this.ensureBootstrap();
    const response = await this.bridge.request({ kind: "daemon.getDesktopPreferences" });
    if (response.kind !== "daemon.desktopPreferences") {
      throw new Error("invalid desktop-preferences bridge response");
    }
    this.desktopPreferences = mapDesktopPreferences(response.preferences);
    return structuredClone(this.desktopPreferences);
  }

  async updateDesktopPreferences(input: {
    expectedRevision: number;
    keepRunningInNotificationArea: boolean;
  }): Promise<DesktopPreferences> {
    await this.ensureBootstrap();
    const pending = this.desktopPreferenceMutation;
    const mutation = pending
      && pending.expectedRevision === input.expectedRevision
      && pending.keepRunningInNotificationArea === input.keepRunningInNotificationArea
      ? pending
      : {
          ...input,
          idempotencyKey: crypto.randomUUID(),
        };
    this.desktopPreferenceMutation = mutation;
    const response = await this.bridge.request({
      kind: "daemon.updateDesktopPreferences",
      expectedRevision: input.expectedRevision,
      keepRunningInNotificationArea: input.keepRunningInNotificationArea,
      idempotencyKey: mutation.idempotencyKey,
    });
    if (response.kind !== "daemon.desktopPreferences") {
      throw new Error("invalid desktop-preferences bridge response");
    }
    this.desktopPreferenceMutation = undefined;
    this.desktopPreferences = mapDesktopPreferences(response.preferences);
    return structuredClone(this.desktopPreferences);
  }

  async getChatModelCatalog(): Promise<ChatModelCatalog> {
    await this.ensureBootstrap();
    let catalog: ChatModelCatalog;
    try {
      const response = await this.bridge.request({ kind: "daemon.getChatModelCatalog" });
      if (response.kind !== "daemon.chatModelCatalog") {
        throw new Error("invalid chat-model-catalog bridge response");
      }
      catalog = mapChatModelCatalog(response.catalog);
    } catch (error) {
      this.markChatUnavailable("Live official xAI model discovery is unavailable.");
      throw error;
    }
    if (!catalog.selectedModelReady) {
      this.markChatUnavailable("The persisted xAI Chat model is not ready in the live catalog.");
      return catalog;
    }
    if (!this.snapshot.capabilities.find((capability) => capability.id === "chat")?.available) {
      try {
        await this.refreshDaemonSnapshot();
      } catch (error) {
        this.markChatUnavailable("Live xAI Chat readiness could not be reconciled with the daemon.");
        throw new Error("Live xAI Chat readiness could not be reconciled with the daemon.", { cause: error });
      }
      if (!this.snapshot.capabilities.find((capability) => capability.id === "chat")?.available) {
        this.markChatUnavailable("The daemon did not confirm the selected xAI Chat model as ready.");
        throw new Error("The daemon did not confirm the selected xAI Chat model as ready.");
      }
    }
    return catalog;
  }

  async selectChatModel(input: {
    expectedRevision: number;
    modelId: string;
  }): Promise<ChatModelPreference> {
    await this.ensureBootstrap();
    const pending = this.chatModelMutation;
    const mutation = pending
      && pending.expectedRevision === input.expectedRevision
      && pending.modelId === input.modelId
      ? pending
      : { ...input, idempotencyKey: crypto.randomUUID() };
    this.chatModelMutation = mutation;
    const response = await this.bridge.request({
      kind: "daemon.selectChatModel",
      expectedRevision: input.expectedRevision,
      modelId: input.modelId,
      idempotencyKey: mutation.idempotencyKey,
    });
    if (response.kind !== "daemon.chatModelPreference") {
      throw new Error("invalid chat-model-preference bridge response");
    }
    const preference = mapChatModelPreference(response.preference);
    try {
      await this.refreshDaemonSnapshot();
    } catch (error) {
      const reason = "The model selection outcome could not be reconciled with live daemon readiness.";
      this.markChatUnavailable(reason);
      throw new Error(reason, { cause: error });
    }
    if (!this.snapshot.capabilities.find((capability) => capability.id === "chat")?.available) {
      const reason = "The daemon did not confirm the selected xAI Chat model as ready.";
      this.markChatUnavailable(reason);
      throw new Error(reason);
    }
    this.chatModelMutation = undefined;
    return preference;
  }

  async beginGrokBuildAuth(): Promise<ClientResult<{ verificationUri: string; userCode?: string; state: "browser_opened" | "device_code" }>> {
    await this.ensureBootstrap();
    const response = await this.bridge.request({
      kind: "daemon.startGrokBuildAuth",
      idempotencyKey: crypto.randomUUID(),
    });
    if (response.kind !== "daemon.grokBuildAuthStatus") {
      throw new Error("invalid grok build auth bridge response");
    }
    if (!response.authenticated) {
      return unavailable(
        response.state === "failed"
          ? "Grok Build authentication failed."
          : GROK_BUILD_AUTH_UNAVAILABLE_REASON,
        "configuration_required",
      );
    }
    this.accountState = {
      ...this.accountState,
      grokBuildAuthenticated: true,
    };
    await this.refreshDaemonSnapshot();
    return {
      status: "success",
      value: {
        verificationUri: "https://accounts.x.ai/",
        state: "browser_opened",
      },
    };
  }

  async completeGrokBuildAuth(): Promise<ClientResult<AccountSetupState>> {
    await this.ensureBootstrap();
    const response = await this.bridge.request({ kind: "daemon.getGrokBuildAuthStatus" });
    if (response.kind !== "daemon.grokBuildAuthStatus") {
      throw new Error("invalid grok build auth status bridge response");
    }
    this.accountState = {
      ...this.accountState,
      grokBuildAuthenticated: response.authenticated === true,
    };
    await this.refreshDaemonSnapshot();
    return { status: "success", value: await this.getAccountSetup() };
  }

  async enrollXaiApiKey(): Promise<ClientResult<AccountSetupState>> {
    await this.ensureBootstrap();
    const idempotencyKey = this.enrollmentIdempotencyKey ??= crypto.randomUUID();
    const response = await this.bridge.request({
      kind: "daemon.enrollXaiApiKey",
      idempotencyKey,
    });
    if (response.kind === "daemon.credentialEnrollmentFailure") {
      this.enrollmentIdempotencyKey = undefined;
      return response.reason === "cancelled"
        ? { status: "cancelled", reason: "Credential entry was cancelled." }
        : unavailable("Native credential entry failed a local integrity check. Restart Grok Desktop before trying again.");
    }
    if (response.kind !== "daemon.accountState") throw new Error("invalid account-state bridge response");
    this.enrollmentIdempotencyKey = undefined;
    this.accountState = response.accountState;
    await this.refreshDaemonSnapshot();
    return { status: "success", value: await this.getAccountSetup() };
  }

  async deleteXaiApiKey(): Promise<ClientResult<AccountSetupState>> {
    await this.ensureBootstrap();
    const response = await this.bridge.request({
      kind: "daemon.deleteXaiApiKey",
      idempotencyKey: crypto.randomUUID(),
    });
    if (response.kind !== "daemon.accountState") throw new Error("invalid account-state bridge response");
    this.accountState = response.accountState;
    await this.refreshDaemonSnapshot();
    return { status: "success", value: await this.getAccountSetup() };
  }

  async getConversation(threadId: string): Promise<ClientResult<ConversationDetail>> {
    await this.ensureBootstrap();
    const conversation = await this.fetchConversation(threadId);
    this.installConversation(conversation);
    return { status: "success", value: structuredClone(conversation) };
  }

  async openExternalUrl(url: string): Promise<ClientResult<void>> {
    let response: BridgeResponse;
    try {
      response = await this.bridge.request({ kind: "desktop.openExternalUrl", url });
    } catch {
      return unavailable("The operating system could not open this source.");
    }
    if (response.kind === "desktop.externalUrlOpenFailed") {
      return unavailable({
        rejected: "This source URL is not an allowed canonical public HTTPS address.",
        busy: "Too many source windows were requested. Wait a moment and try again.",
        unavailable: "The operating system could not open this source.",
      }[response.reason]);
    }
    if (response.kind !== "desktop.externalUrlOpened" || response.accepted !== true) {
      throw new Error("invalid external-URL bridge response");
    }
    return { status: "success", value: undefined };
  }

  async searchWorkspace(input: {
    projectId?: string;
    query: string;
    offset?: number;
    limit?: number;
  }): Promise<WorkspaceSearchResults> {
    await this.ensureBootstrap();
    const response = await this.bridge.request({
      kind: "daemon.searchWorkspace",
      projectId: input.projectId,
      query: input.query,
      offset: input.offset ?? 0,
      limit: input.limit ?? 8,
    });
    if (response.kind !== "daemon.workspaceSearchResults") {
      throw new Error("invalid workspace-search bridge response");
    }
    return structuredClone(response.results);
  }

  subscribeConversation(threadId: string, listener: (conversation: ConversationDetail) => void): () => void {
    const listeners = this.conversationListeners.get(threadId) ?? new Set();
    listeners.add(listener);
    this.conversationListeners.set(threadId, listeners);
    const current = this.conversations.get(threadId);
    if (current) listener(structuredClone(current));
    return () => {
      listeners.delete(listener);
      if (listeners.size === 0) {
        this.conversationListeners.delete(threadId);
        this.releaseConversationIfInactive(threadId);
      }
    };
  }

  async sendConversationMessage(
    threadId: string,
    content: string,
    attachments: ConversationAttachment[],
  ): Promise<ClientResult<{ messageId: string; turnId: string }>> {
    await this.ensureBootstrap();
    if (attachments.length > 0) {
      return unavailable("Attachments are not connected to the durable Chat execution path.");
    }
    const capability = this.snapshot.capabilities.find((item) => item.id === "chat");
    if (!capability?.available) {
      return unavailable(capability?.reason ?? GROK_EXECUTION_UNAVAILABLE_REASON, "configuration_required");
    }
    if (!this.threads.has(threadId)) {
      return unavailable("The conversation is no longer available.");
    }
    const thread = this.threads.get(threadId);
    if (!thread) return unavailable("The conversation is no longer available.");
    const turn = await this.startConversationTurn(thread, validatedPrompt(content));
    if (isTerminalConversationState(turn.state) && turn.state !== "completed") {
      return unavailable(
        conversationTurnReason(turn),
        turn.failure?.kind === "authentication" || turn.failure?.kind === "forbidden"
          ? "configuration_required"
          : "unavailable",
      );
    }
    return {
      status: "success",
      value: {
        messageId: turn.assistantMessage?.id ?? turn.userMessage.id,
        turnId: turn.turnId,
      },
    };
  }

  async cancelConversationTurn(input: {
    turnId: string;
    expectedRevision: number;
  }): Promise<ClientResult<ConversationTurnDetail>> {
    await this.ensureBootstrap();
    const threadId = this.turnThreads.get(input.turnId);
    if (!threadId) return unavailable("The active conversation turn is no longer available.");
    const existingMutation = this.conversationCancellationMutations.get(input.turnId);
    const mutation = existingMutation?.expectedRevision === input.expectedRevision
      ? existingMutation
      : {
          expectedRevision: input.expectedRevision,
          idempotencyKey: crypto.randomUUID(),
        };
    this.conversationCancellationMutations.set(input.turnId, mutation);
    let response;
    try {
      response = await this.bridge.request({
        kind: "daemon.cancelConversationTurn",
        turnId: input.turnId,
        expectedRevision: input.expectedRevision,
        idempotencyKey: mutation.idempotencyKey,
      });
    } catch (error) {
      const canonical = await this.reloadConversationAfterExecution(threadId);
      const turn = canonical?.turns.find((item) => item.id === input.turnId);
      if (
        turn
        && isTerminalConversationState(turn.state)
        && turn.revision === input.expectedRevision + 1
      ) {
        this.conversationCancellationMutations.delete(input.turnId);
        return { status: "success", value: structuredClone(turn) };
      }
      throw error;
    }
    if (response.kind !== "daemon.conversationTurn" || response.turn.turnId !== input.turnId) {
      throw new Error("invalid cancelled conversation-turn bridge response");
    }
    if (!isTerminalConversationState(response.turn.state)) {
      throw new Error("daemon returned a nonterminal cancellation outcome");
    }
    if (response.turn.revision !== input.expectedRevision + 1) {
      throw new Error("daemon returned an invalid cancellation revision");
    }
    this.associateTurn(response.turn.turnId, threadId);
    const canonical = await this.fetchConversation(threadId);
    const turn = canonical.turns.find((item) => item.id === input.turnId);
    if (!turn || turn.state !== response.turn.state || turn.revision !== response.turn.revision) {
      throw new Error("canonical conversation does not match the cancellation outcome");
    }
    this.installConversation(canonical);
    this.conversationCancellationMutations.delete(input.turnId);
    return { status: "success", value: structuredClone(turn) };
  }

  async retryConversationTurn(input: {
    sourceTurnId: string;
    expectedRevision: number;
  }): Promise<ClientResult<ConversationTurnDetail>> {
    await this.ensureBootstrap();
    const threadId = this.turnThreads.get(input.sourceTurnId);
    if (!threadId) return unavailable("The retry source is no longer available.");
    const source = this.conversations.get(threadId)?.turns.find(
      (turn) => turn.id === input.sourceTurnId,
    );
    if (!source) return unavailable("The retry source is no longer available.");
    if (source.revision !== input.expectedRevision) {
      return unavailable("The retry source changed. Review the latest conversation state.");
    }
    if (source.retryEligibility !== "allowed") {
      return unavailable(conversationRetryEligibilityReason(source.retryEligibility));
    }

    const existingMutation = this.conversationRetryMutations.get(input.sourceTurnId);
    const mutation = existingMutation?.expectedRevision === input.expectedRevision
      ? existingMutation
      : {
          expectedRevision: input.expectedRevision,
          idempotencyKey: crypto.randomUUID(),
        };
    this.conversationRetryMutations.set(input.sourceTurnId, mutation);

    let response;
    try {
      response = await this.bridge.request({
        kind: "daemon.retryConversationTurn",
        sourceTurnId: input.sourceTurnId,
        expectedRevision: input.expectedRevision,
        idempotencyKey: mutation.idempotencyKey,
      });
    } catch (error) {
      const canonical = await this.reloadConversationAfterExecution(threadId);
      const retry = canonical && canonicalRetryOutcome(
        canonical,
        source,
        input.sourceTurnId,
      );
      if (retry) {
        this.conversationRetryMutations.delete(input.sourceTurnId);
        return { status: "success", value: structuredClone(retry) };
      }
      throw error;
    }

    if (response.kind !== "daemon.conversationTurn") {
      throw new Error("invalid retried conversation-turn bridge response");
    }
    const returned = mapConversationTurnDetail(response.turn);
    validateRetryOutcome(returned, source, input.sourceTurnId);
    if (response.turn.userMessage.threadId !== threadId) {
      throw new Error("retried conversation turn changed owning thread");
    }
    this.associateTurn(response.turn.turnId, threadId);

    const canonical = await this.fetchConversation(threadId);
    const retry = canonicalRetryOutcome(
      canonical,
      source,
      input.sourceTurnId,
      returned.id,
    );
    if (!retry) {
      throw new Error("canonical conversation does not match the retry outcome");
    }
    this.installConversation(canonical);
    this.conversationRetryMutations.delete(input.sourceTurnId);
    return { status: "success", value: structuredClone(retry) };
  }

  async editConversationMessage(threadId: string, messageId: string, content: string): Promise<ClientResult<ConversationDetail>> {
    await this.ensureBootstrap();
    const conversation = this.conversations.get(threadId);
    const source = conversation?.turns.find((turn) => turn.userMessageId === messageId);
    if (!conversation || !source) {
      return unavailable("This submitted prompt is no longer available.");
    }
    if (!matchesEditableForkState(source.state)) {
      return unavailable("Only a completed, cancelled, or known failed prompt can be edited into a branch.");
    }
    const sourceMessage = conversation.messages.find((message) => message.id === messageId);
    if (!sourceMessage || sourceMessage.role !== "user") {
      return unavailable("This submitted prompt is no longer available.");
    }
    const editedContent = validatedPrompt(content);
    if (editedContent === sourceMessage.content) {
      return unavailable("Change the prompt before creating an edited branch.");
    }
    this.assertChatAvailable();
    return this.executeConversationFork(
      "edit_and_branch",
      conversation,
      source,
      sourceMessage,
      editedContent,
    );
  }

  async regenerateConversationMessage(threadId: string, messageId: string): Promise<ClientResult<ConversationDetail>> {
    await this.ensureBootstrap();
    const conversation = this.conversations.get(threadId);
    const source = conversation?.turns.find((turn) => turn.assistantMessageId === messageId);
    if (!conversation || !source || source.state !== "completed") {
      return unavailable("Only a completed Grok response can be regenerated.");
    }
    const sourceMessage = conversation.messages.find((message) => message.id === messageId);
    if (!sourceMessage || sourceMessage.role !== "assistant") {
      return unavailable("This completed response is no longer available.");
    }
    this.assertChatAvailable();
    return this.executeConversationFork(
      "regenerate",
      conversation,
      source,
      sourceMessage,
    );
  }

  async branchConversation(threadId: string, messageId: string): Promise<ClientResult<ConversationDetail>> {
    await this.ensureBootstrap();
    const conversation = this.conversations.get(threadId);
    const source = conversation?.turns.find((turn) => turn.assistantMessageId === messageId);
    if (!conversation || !source || source.state !== "completed") {
      return unavailable("Only a completed Grok response can be branched.");
    }
    const sourceMessage = conversation.messages.find((message) => message.id === messageId);
    if (!sourceMessage || sourceMessage.role !== "assistant") {
      return unavailable("This completed response is no longer available.");
    }
    return this.executeConversationFork(
      "branch",
      conversation,
      source,
      sourceMessage,
    );
  }

  private async executeConversationFork(
    kind: "branch" | "edit_and_branch" | "regenerate",
    parent: ConversationDetail,
    source: ConversationTurnDetail,
    sourceMessage: ConversationMessage,
    content?: string,
  ): Promise<ClientResult<ConversationDetail>> {
    const parentThread = this.threads.get(parent.id);
    const parentCanonicalMessages = this.canonicalConversationMessages.get(parent.id);
    if (!parentThread) throw new Error("conversation fork parent is unavailable");
    if (!parentCanonicalMessages) {
      throw new Error("conversation fork parent is missing its canonical message snapshot");
    }
    const mutationKey = await conversationForkMutationKey(kind, source.id, content);
    const existing = this.conversationForkMutations.get(mutationKey);
    if (!existing && this.conversationForkMutations.size >= MAX_PENDING_CONVERSATION_FORK_MUTATIONS) {
      throw new Error(
        "Too many conversation branch requests have uncertain outcomes. Reload the app before trying again.",
      );
    }
    const mutation = existing?.expectedRevision === source.revision
      ? existing
      : {
          expectedRevision: source.revision,
          idempotencyKey: crypto.randomUUID(),
        };
    this.conversationForkMutations.set(mutationKey, mutation);

    let response: BridgeResponse;
    if (kind === "branch") {
      response = await this.bridge.request({
        kind: "daemon.branchConversationThread",
        sourceTurnId: source.id,
        expectedRevision: source.revision,
        idempotencyKey: mutation.idempotencyKey,
      });
    } else if (kind === "edit_and_branch") {
      response = await this.bridge.request({
        kind: "daemon.editAndBranchConversationTurn",
        sourceTurnId: source.id,
        expectedRevision: source.revision,
        content: content ?? "",
        idempotencyKey: mutation.idempotencyKey,
      });
    } else {
      response = await this.bridge.request({
        kind: "daemon.regenerateConversationTurn",
        sourceTurnId: source.id,
        expectedRevision: source.revision,
        idempotencyKey: mutation.idempotencyKey,
      });
    }
    if (response.kind !== "daemon.conversationFork") {
      throw new Error("invalid conversation-fork bridge response");
    }
    validateConversationForkResponse(
      response.fork,
      kind,
      parentThread,
      parent,
      source,
      sourceMessage,
      content,
    );
    this.threads.set(response.fork.childThread.id, response.fork.childThread);
    if (response.fork.startedTurn) {
      this.associateTurn(response.fork.startedTurn.turnId, response.fork.childThread.id);
    }
    const canonical = await this.fetchConversation(response.fork.childThread.id);
    const canonicalThread = this.threads.get(canonical.id);
    if (
      !canonicalThread
      || canonicalThread.projectId !== response.fork.childThread.projectId
      || canonicalThread.createdAtUnixMs !== response.fork.childThread.createdAtUnixMs
    ) {
      throw new Error("canonical conversation changed immutable fork ownership");
    }
    const childCanonicalMessages = this.canonicalConversationMessages.get(canonical.id);
    if (!childCanonicalMessages) {
      throw new Error("conversation fork child is missing its canonical message snapshot");
    }
    validateCanonicalConversationFork(
      canonical,
      childCanonicalMessages,
      response.fork,
      kind,
      parentThread,
      parent,
      parentCanonicalMessages,
      source,
      sourceMessage,
      content,
    );
    this.installConversation(canonical, { retainIfInactive: true });
    try {
      if (response.fork.delivery.state === "pending") {
        let acknowledgement = this.conversationForkDeliveryMutations.get(canonical.id);
        if (!acknowledgement) {
          if (
            this.conversationForkDeliveryMutations.size
            >= MAX_PENDING_CONVERSATION_FORK_MUTATIONS
          ) {
            throw new Error(
              "Too many conversation branch deliveries have uncertain acknowledgement outcomes.",
            );
          }
          acknowledgement = { expectedRevision: 0, idempotencyKey: crypto.randomUUID() };
          this.conversationForkDeliveryMutations.set(canonical.id, acknowledgement);
        }
        const acknowledgementResponse = await this.bridge.request({
          kind: "daemon.acknowledgeConversationForkDelivery",
          childThreadId: canonical.id,
          expectedRevision: acknowledgement.expectedRevision,
          idempotencyKey: acknowledgement.idempotencyKey,
        });
        if (
          acknowledgementResponse.kind !== "daemon.conversationForkDelivery"
          || acknowledgementResponse.delivery.childThreadId !== canonical.id
          || acknowledgementResponse.delivery.state !== "acknowledged"
          || acknowledgementResponse.delivery.revision !== 1
        ) {
          throw new Error("invalid conversation-fork delivery acknowledgement response");
        }
        if (this.conversationForkDeliveryMutations.get(canonical.id) === acknowledgement) {
          this.conversationForkDeliveryMutations.delete(canonical.id);
        }
      } else if (response.fork.delivery.state === "acknowledged") {
        this.conversationForkDeliveryMutations.delete(canonical.id);
      } else {
        throw new Error("invalid conversation-fork delivery state");
      }
    } finally {
      this.releaseConversationIfInactive(canonical.id);
    }
    if (this.conversationForkMutations.get(mutationKey) === mutation) {
      this.conversationForkMutations.delete(mutationKey);
    }
    return { status: "success", value: structuredClone(canonical) };
  }

  async listMediaCreations(kind: "image" | "video"): Promise<ClientResult<MediaCreation[]>> {
    const capability = this.snapshot.capabilities.find((item) => item.id === (kind === "image" ? "imagine_image" : "imagine_video"));
    return unavailable(capability?.reason ?? "Imagine requires a configured xAI API key.", capability?.available ? "unavailable" : "configuration_required");
  }

  subscribeMediaCreations(_kind: "image" | "video", _listener: (creations: MediaCreation[]) => void): () => void {
    return () => undefined;
  }

  async createMedia(input: { kind: "image" | "video"; prompt: string; aspectRatio: string; duration?: string }): Promise<ClientResult<MediaCreation>> {
    return this.listMediaCreations(input.kind).then((result) => result.status === "success" ? unavailable("Imagine creation is not exposed by the current daemon protocol.") : result);
  }

  async cancelMedia(_creationId: string): Promise<ClientResult<MediaCreation>> {
    return unavailable("Imagine queue control is not exposed by the current daemon protocol.");
  }

  async getVoiceSetup(): Promise<VoiceSetup> {
    const capability = this.snapshot.capabilities.find((item) => item.id === "realtime_voice");
    return {
      capability: capability?.available ? "unavailable" : "configuration_required",
      reason: capability?.available ? "Realtime Voice sessions are not exposed by the current daemon protocol." : capability?.reason ?? "Voice requires a configured xAI API key.",
      inputDevices: [], outputDevices: [], selectedInputId: "", selectedOutputId: "",
    };
  }

  async startVoiceSession(_inputDeviceId: string, _outputDeviceId: string): Promise<ClientResult<VoiceSession>> {
    const setup = await this.getVoiceSetup();
    return unavailable(setup.reason ?? "Realtime Voice is unavailable.", setup.capability === "configuration_required" ? "configuration_required" : "unavailable");
  }

  async setVoiceSessionState(_sessionId: string, _state: "listening" | "interrupted" | "ended"): Promise<ClientResult<VoiceSession>> {
    return unavailable("Realtime Voice sessions are not exposed by the current daemon protocol.");
  }

  async saveAutomation(draft: AutomationDraft): Promise<ClientResult<AutomationSummary>> {
    await this.ensureBootstrap();
    const project = this.projects.get(draft.projectId);
    if (!project) return unavailable("Select an existing project before saving this automation.");
    const common = {
      projectId: project.id,
      title: draft.name,
      prompt: draft.prompt,
      schedule: serializeSchedule(draft.schedule),
      timezone: draft.schedule.timeZoneIana,
      missedRunPolicy: draft.missedRunPolicy,
      overlapPolicy: draft.overlapPolicy,
      idempotencyKey: crypto.randomUUID(),
    } as const;
    const current = draft.id ? this.automations.get(draft.id) : undefined;
    const response = await this.bridge.request(current
      ? { kind: "daemon.updateAutomation", automationId: current.id, expectedRevision: current.revision, ...common }
      : { kind: "daemon.createAutomation", ...common });
    if (response.kind !== "daemon.automation") throw new Error("invalid automation bridge response");
    this.automations.set(response.automation.id, response.automation);
    this.rebuildWorkspaceSnapshot();
    this.emit();
    return {
      status: "success",
      value: automationSummary(response.automation, project.name),
    };
  }

  async getManagedIntegration(_integrationId: "wisp"): Promise<ClientResult<ManagedIntegrationDetail>> {
    return { status: "success", value: productionWispDetail() };
  }

  async changeManagedIntegration(_integrationId: "wisp", _action: "install" | "update" | "rollback"): Promise<ClientResult<ManagedIntegrationDetail>> {
    return unavailable("Managed add-on installation is not exposed by the current daemon protocol.", "configuration_required");
  }

  private async startConversationTurn(
    thread: DaemonThread,
    content: string,
    retainedIdempotencyKey?: string,
  ): Promise<DaemonConversationTurn> {
    const existingMutation = this.conversationStartMutations.get(thread.id);
    const mutation = retainedIdempotencyKey
      ? existingMutation?.content === content && existingMutation.idempotencyKey === retainedIdempotencyKey
        ? existingMutation
        : { content, idempotencyKey: retainedIdempotencyKey }
      : existingMutation?.content === content
      ? existingMutation
      : { content, idempotencyKey: crypto.randomUUID() };
    this.conversationStartMutations.set(thread.id, mutation);
    let response;
    try {
      response = await this.bridge.request({
        kind: "daemon.startConversationTurn",
        threadId: thread.id,
        content,
        idempotencyKey: mutation.idempotencyKey,
      });
    } catch (error) {
      await this.reloadConversationAfterExecution(thread.id);
      throw error;
    }
    if (response.kind !== "daemon.conversationTurn" || response.turn.userMessage.threadId !== thread.id) {
      throw new Error("invalid started conversation-turn bridge response");
    }
    this.associateTurn(response.turn.turnId, thread.id);
    this.upsertConversationTurn(thread, response.turn);
    if (this.conversationStartMutations.get(thread.id) === mutation) {
      this.conversationStartMutations.delete(thread.id);
    }
    return response.turn;
  }

  private async fetchConversation(threadId: string): Promise<ConversationDetail> {
    const response = await this.bridge.request({ kind: "daemon.getConversation", threadId });
    if (response.kind !== "daemon.conversation" || response.thread.id !== threadId) {
      throw new Error("invalid conversation bridge response");
    }
    for (const familyThread of response.forkMetadata.familyThreads) {
      this.threads.set(familyThread.id, familyThread);
    }
    this.canonicalConversationMessages.set(
      threadId,
      response.messages.map((message) => structuredClone(message)),
    );
    for (const turn of response.turns) this.associateTurn(turn.turnId, threadId);
    const conversation = mapConversation(
      response.thread,
      response.messages,
      response.turns,
      response.forkMetadata,
      this.projects.get(response.thread.projectId)?.name ?? "Unknown project",
    );
    const reconciled = this.reconcileConversationProjections(conversation);
    for (const turn of reconciled.turns) {
      const projection = this.conversationEventProjections.get(turn.id);
      if (projection?.state && isTerminalConversationState(projection.state)) {
        validateTerminalProjection(reconciled, turn.id, projection);
        try {
          this.retainTerminalPrefix(projection);
        } finally {
          // Whether retained or rejected at the aggregate bound, a terminal
          // full event history must not accumulate across historical turns.
          this.conversationEventProjections.delete(turn.id);
        }
      }
    }
    return reconciled;
  }

  private installConversation(
    conversation: ConversationDetail,
    options: { retainIfInactive?: boolean } = {},
  ): void {
    for (const turn of conversation.turns) {
      if (turn.lineage.origin === "retry") {
        // A validated canonical child proves the one allowed retry command for
        // this source materialized, even if its IPC acknowledgement was lost.
        this.conversationRetryMutations.delete(turn.lineage.sourceTurnId);
      }
    }
    this.conversations.set(conversation.id, conversation);
    this.notifyConversation(conversation.id, conversation);
    this.rebuildWorkspaceSnapshot();
    this.emit();
    if (!options.retainIfInactive) this.releaseConversationIfInactive(conversation.id);
  }

  private releaseConversationIfInactive(threadId: string): void {
    if (this.conversationListeners.has(threadId)) return;
    const conversation = this.conversations.get(threadId);
    if (!conversation || conversation.turns.some((turn) => !isTerminalConversationState(turn.state))) return;
    this.conversations.delete(threadId);
    this.canonicalConversationMessages.delete(threadId);
    this.conversationStartMutations.delete(threadId);
    for (const turn of conversation.turns) {
      this.conversationEventProjections.delete(turn.id);
      const prefix = this.conversationTerminalPrefixes.get(turn.id);
      if (prefix) {
        this.conversationTerminalPrefixes.delete(turn.id);
        this.conversationTerminalPrefixBytes -= prefix.textUtf8Bytes;
      }
      this.conversationCancellationMutations.delete(turn.id);
      this.turnThreads.delete(turn.id);
    }
  }

  private upsertConversationTurn(thread: DaemonThread, turn: DaemonConversationTurn): void {
    const current = structuredClone(this.conversations.get(thread.id) ?? emptyConversation(
      thread,
      this.projects.get(thread.projectId)?.name ?? "Unknown project",
    ));
    const turnDetail = mapConversationTurnDetail(turn);
    const existingTurn = current.turns.findIndex((item) => item.id === turn.turnId);
    if (existingTurn >= 0) current.turns[existingTurn] = turnDetail;
    else current.turns.push(turnDetail);

    upsertConversationMessage(current, mapDaemonMessage(turn.userMessage));
    if (turn.assistantMessage) {
      upsertConversationMessage(current, mapDaemonMessage(
        turn.assistantMessage,
        mappedCitations(turn),
      ));
    }
    this.applyProjectionToConversation(current, turn.turnId);
    this.installConversation(current);
  }

  private async handleConversationTurnEvents(
    notification: DesktopConversationTurnEventNotification,
  ): Promise<void> {
    const applied = applyConversationEventNotification(
      this.conversationEventProjections.get(notification.turnId),
      notification,
    );
    this.conversationEventProjections.set(notification.turnId, applied.projection);

    // The watcher can beat the start/get response that establishes ownership.
    // Retain the validated projection, but do not resolve the preload listener
    // (and therefore ACK) until the exact turn has been linked to its thread.
    const threadId = this.turnThreads.get(notification.turnId)
      ?? await this.waitForTurnThread(notification.turnId);
    const current = this.conversations.get(threadId);
    if (current) {
      const projected = structuredClone(current);
      this.applyProjectionToConversation(projected, notification.turnId);
      this.conversations.set(threadId, projected);
      this.notifyConversation(threadId, projected);
    }

    if (!applied.reachedTerminal) return;
    const canonical = await this.fetchConversation(threadId);
    validateTerminalProjection(canonical, notification.turnId, applied.projection);
    // `canonical` already contains the validated partial projection for a
    // failed/review outcome. Drop duplicate event history and concatenated text
    // once no further event can legally follow this terminal edge.
    this.retainTerminalPrefix(applied.projection);
    this.conversationEventProjections.delete(notification.turnId);
    this.installConversation(canonical);
  }

  private reconcileConversationProjections(conversation: ConversationDetail): ConversationDetail {
    const reconciled = structuredClone(conversation);
    for (const turn of reconciled.turns) {
      this.applyProjectionToConversation(reconciled, turn.id);
    }
    return reconciled;
  }

  private applyProjectionToConversation(conversation: ConversationDetail, turnId: string): void {
    const turn = conversation.turns.find((item) => item.id === turnId);
    if (!turn) return;
    const projection = this.conversationEventProjections.get(turnId);
    const terminalPrefix = this.conversationTerminalPrefixes.get(turnId);
    for (const candidate of [terminalPrefix, projection]) {
      if (candidate?.state) validateProjectionAgainstTurnPrefix(turn, candidate);
    }
    // Once canonical reconciliation succeeds, keep displaying the compact
    // terminal evidence while a later replay is independently revalidated.
    const displayed = terminalPrefix ?? projection;
    if (displayed?.state) {
      if (displayed.revision > turn.revision) {
        turn.state = displayed.state;
        turn.revision = displayed.revision;
      }
    }

    const state = displayed?.revision === turn.revision && displayed.state
      ? displayed.state
      : turn.state;
    const text = displayed?.text ?? "";
    const syntheticId = streamingMessageId(turn.id);
    const existingIndex = conversation.messages.findIndex((message) => message.id === syntheticId);
    const hasCanonicalAssistant = Boolean(
      turn.assistantMessageId
      && conversation.messages.some((message) => message.id === turn.assistantMessageId),
    );
    const shouldProjectMessage = !hasCanonicalAssistant && (
      state === "reserved"
      || state === "provider_started"
      || ((state === "completed" || state === "failed" || state === "interrupted_needs_review") && text.length > 0)
    );
    if (!shouldProjectMessage) {
      if (existingIndex >= 0) conversation.messages.splice(existingIndex, 1);
      return;
    }

    const user = conversation.messages.find((message) => message.id === turn.userMessageId);
    const projectedMessage: ConversationMessage = {
      id: syntheticId,
      role: "assistant",
      content: text,
      state: state === "failed"
        ? "error"
        : state === "interrupted_needs_review"
          ? "stopped"
          : state === "completed"
            ? "complete"
            : "streaming",
      createdAt: user?.createdAt ?? "Now",
      citations: [],
      attachments: [],
    };
    if (existingIndex >= 0) conversation.messages[existingIndex] = projectedMessage;
    else {
      const userIndex = conversation.messages.findIndex((message) => message.id === turn.userMessageId);
      conversation.messages.splice(userIndex >= 0 ? userIndex + 1 : conversation.messages.length, 0, projectedMessage);
    }
  }

  private retainTerminalPrefix(projection: ConversationEventProjection): void {
    const shouldRetain = (
      projection.state === "failed" || projection.state === "interrupted_needs_review"
    ) && projection.textUtf8Bytes > 0;
    const existing = this.conversationTerminalPrefixes.get(projection.turnId);
    if (!shouldRetain) {
      if (existing) throw new Error("conversation terminal replay conflicts with retained evidence");
      return;
    }
    const prefix: ConversationTerminalPrefix = {
      turnId: projection.turnId,
      state: projection.state,
      revision: projection.revision,
      text: projection.text,
      textUtf8Bytes: projection.textUtf8Bytes,
    };
    if (existing) {
      if (
        existing.state !== prefix.state
        || existing.revision !== prefix.revision
        || existing.textUtf8Bytes !== prefix.textUtf8Bytes
        || existing.text !== prefix.text
      ) {
        throw new Error("conversation terminal replay conflicts with retained evidence");
      }
      return;
    }
    if (
      this.conversationTerminalPrefixes.size >= MAX_TERMINAL_PREFIX_COUNT
      || this.conversationTerminalPrefixBytes + prefix.textUtf8Bytes > MAX_TERMINAL_PREFIX_BYTES
    ) {
      throw new Error("conversation terminal evidence exceeded the renderer recovery limit");
    }
    this.conversationTerminalPrefixes.set(prefix.turnId, prefix);
    this.conversationTerminalPrefixBytes += prefix.textUtf8Bytes;
  }

  private associateTurn(turnId: string, threadId: string): void {
    const existing = this.turnThreads.get(turnId);
    if (existing && existing !== threadId) throw new Error("conversation turn changed owning thread");
    this.turnThreads.set(turnId, threadId);
    const waiters = this.turnThreadWaiters.get(turnId);
    this.turnThreadWaiters.delete(turnId);
    for (const waiter of waiters ?? []) {
      clearTimeout(waiter.timer);
      waiter.resolve(threadId);
    }
  }

  private waitForTurnThread(turnId: string): Promise<string> {
    const existing = this.turnThreads.get(turnId);
    if (existing) return Promise.resolve(existing);
    return new Promise((resolve, reject) => {
      const waiters = this.turnThreadWaiters.get(turnId) ?? new Set();
      const waiter: TurnThreadWaiter = {
        resolve,
        timer: setTimeout(() => {
          const active = this.turnThreadWaiters.get(turnId);
          active?.delete(waiter);
          if (active?.size === 0) this.turnThreadWaiters.delete(turnId);
          reject(new Error("conversation event ownership was not established"));
        }, TURN_OWNERSHIP_WAIT_MS),
      };
      waiters.add(waiter);
      this.turnThreadWaiters.set(turnId, waiters);
    });
  }

  private ensureBootstrap(): Promise<void> {
    if (this.bootstrapped) return Promise.resolve();
    if (this.bootstrapPromise) return this.bootstrapPromise;
    this.bootstrapPromise = this.bridge.request({ kind: "daemon.bootstrap" })
      .then((response) => {
        if (response.kind !== "daemon.bootstrap") throw new Error("invalid daemon bootstrap response");
        this.snapshot.capabilities = response.capabilities.map((value) => mapCapability(
          value,
          response.status.automationScheduler?.state,
        ));
        this.applyStatus(response.status);
        this.snapshot.extensions = extensionCatalog(response.capabilities);
        this.accountState = response.accountState;
        this.applyWorkspace(response.workspace);
        this.bootstrapped = true;
      })
      .catch((error: unknown) => {
        const reason = error instanceof Error ? error.message : "The local daemon is unavailable.";
        this.snapshot.connection = {
          state: "degraded",
          profile: "Local workspace",
          plan: "Limited mode",
          reason,
        };
        this.snapshot.capabilities = degradedCapabilities(reason);
        this.emit();
      })
      .finally(() => {
        this.bootstrapPromise = undefined;
      });
    return this.bootstrapPromise;
  }

  private async refreshDaemonSnapshot(): Promise<void> {
    const response = await this.bridge.request({ kind: "daemon.bootstrap" });
    if (response.kind !== "daemon.bootstrap") throw new Error("invalid daemon bootstrap response");
    this.snapshot.capabilities = response.capabilities.map((value) => mapCapability(
      value,
      response.status.automationScheduler?.state,
    ));
    this.snapshot.extensions = extensionCatalog(response.capabilities);
    this.accountState = response.accountState;
    this.applyStatus(response.status);
    this.applyWorkspace(response.workspace);
    this.emit();
  }

  private deleteArtifactRemovalMutation(
    artifactId: string,
    mutation: ArtifactRemovalMutation,
  ): void {
    if (this.artifactRemovalMutations.get(artifactId) === mutation) {
      this.artifactRemovalMutations.delete(artifactId);
      this.cancelArtifactRemovalReconciliation(artifactId, mutation);
    }
  }

  private cancelArtifactRemovalReconciliation(
    artifactId: string,
    mutation: ArtifactRemovalMutation,
  ): void {
    const scheduled = this.artifactRemovalReconciliationTimers.get(artifactId);
    if (!scheduled || scheduled.mutation !== mutation) return;
    clearTimeout(scheduled.timer);
    this.artifactRemovalReconciliationTimers.delete(artifactId);
  }

  private completeArtifactRemoval(
    artifactId: string,
    mutation: ArtifactRemovalMutation,
    tombstone: DaemonArtifact,
  ): void {
    this.deleteArtifactRemovalMutation(artifactId, mutation);
    this.artifacts.set(artifactId, tombstone);
    this.rebuildWorkspaceSnapshot();
    this.emit();
  }

  private markArtifactRemovalPending(
    artifactId: string,
    mutation: ArtifactRemovalMutation,
    tombstone: DaemonArtifact,
  ): void {
    if (this.artifactRemovalMutations.get(artifactId) !== mutation) return;
    mutation.phase = "daemon_pending";
    this.artifacts.set(artifactId, tombstone);
    this.rebuildWorkspaceSnapshot();
    this.emit();
    this.scheduleArtifactRemovalReconciliation(artifactId, mutation);
  }

  private scheduleArtifactRemovalReconciliation(
    artifactId: string,
    mutation: ArtifactRemovalMutation,
  ): void {
    if (
      this.artifactRemovalMutations.get(artifactId) !== mutation
      || mutation.phase !== "daemon_pending"
      || mutation.reconciliationAttempt >= ARTIFACT_REMOVAL_RECONCILIATION_DELAYS_MS.length
      || this.artifactRemovalReconciliationTimers.has(artifactId)
    ) {
      return;
    }
    const delay = ARTIFACT_REMOVAL_RECONCILIATION_DELAYS_MS[mutation.reconciliationAttempt];
    const timer = setTimeout(() => {
      const scheduled = this.artifactRemovalReconciliationTimers.get(artifactId);
      if (!scheduled || scheduled.mutation !== mutation) return;
      this.artifactRemovalReconciliationTimers.delete(artifactId);
      void this.retryDaemonPendingArtifactRemoval(artifactId, mutation);
    }, delay);
    this.artifactRemovalReconciliationTimers.set(artifactId, { mutation, timer });
  }

  private async retryDaemonPendingArtifactRemoval(
    artifactId: string,
    mutation: ArtifactRemovalMutation,
  ): Promise<void> {
    if (
      this.artifactRemovalMutations.get(artifactId) !== mutation
      || mutation.phase !== "daemon_pending"
    ) {
      return;
    }
    mutation.reconciliationAttempt += 1;
    mutation.phase = "dispatching";
    try {
      const response = await this.bridge.request({
        kind: "daemon.removeArtifact",
        artifactId,
        expectedRevision: mutation.expectedRevision,
        expectedContentVersion: mutation.expectedContentVersion,
        idempotencyKey: mutation.idempotencyKey,
      });
      if (
        response.kind === "daemon.artifactRemoved"
        && matchesArtifactRemovalTombstone(response.artifact, artifactId, mutation)
      ) {
        this.completeArtifactRemoval(artifactId, mutation, response.artifact);
        return;
      }
      if (
        response.kind === "daemon.artifactRemovalPending"
        && response.artifactId === artifactId
        && response.expectedRevision === mutation.expectedRevision
        && response.expectedContentVersion === mutation.expectedContentVersion
        && matchesArtifactRemovalTombstone(response.tombstone, artifactId, mutation)
      ) {
        this.markArtifactRemovalPending(artifactId, mutation, response.tombstone);
        return;
      }
    } catch {
      // Acknowledged cleanup remains daemon-owned; bounded same-key retries are safe.
    }
    if (this.artifactRemovalMutations.get(artifactId) === mutation) {
      mutation.phase = "daemon_pending";
      this.scheduleArtifactRemovalReconciliation(artifactId, mutation);
    }
  }

  private async reconcileAmbiguousArtifactRemoval(
    artifactId: string,
    mutation: ArtifactRemovalMutation,
  ): Promise<boolean> {
    try {
      await this.refreshDaemonSnapshot();
    } catch {
      return false;
    }
    const canonical = this.artifacts.get(artifactId);
    if (!canonical || !matchesArtifactRemovalTombstone(canonical, artifactId, mutation)) {
      return false;
    }
    mutation.phase = "daemon_pending";
    this.scheduleArtifactRemovalReconciliation(artifactId, mutation);
    return true;
  }

  private markChatUnavailable(reason: string): void {
    const current = this.snapshot.capabilities.find((capability) => capability.id === "chat");
    const chatUnavailable: CapabilityStatus = {
      id: "chat",
      label: current?.label ?? "Grok chat",
      source: "xai_api",
      available: false,
      availability: "unavailable",
      authentication: "xai_api_key",
      reasonCode: "xai_model_selection_unavailable",
      reason,
    };
    this.snapshot.capabilities = current
      ? this.snapshot.capabilities.map((capability) => capability.id === "chat" ? chatUnavailable : capability)
      : [...this.snapshot.capabilities, chatUnavailable];
    this.emit();
  }

  private applyWorkspace(workspace: DaemonWorkspaceSnapshot): void {
    this.projects.clear();
    this.threads.clear();
    this.artifacts.clear();
    this.automations.clear();
    for (const project of workspace.projects) this.projects.set(project.id, project);
    for (const thread of workspace.threads) this.threads.set(thread.id, thread);
    for (const artifact of workspace.artifacts) this.artifacts.set(artifact.id, artifact);
    for (const automation of workspace.automations) this.automations.set(automation.id, automation);
    this.reconcileArtifactRemovalMutations();
    this.rebuildWorkspaceSnapshot();
  }

  private reconcileArtifactRemovalMutations(): void {
    for (const [artifactId, mutation] of this.artifactRemovalMutations) {
      const canonical = this.artifacts.get(artifactId);
      if (canonical && matchesArtifactRemovalTombstone(canonical, artifactId, mutation)) {
        if (mutation.phase !== "dispatching") {
          mutation.phase = "daemon_pending";
          this.scheduleArtifactRemovalReconciliation(artifactId, mutation);
        }
        continue;
      }
      if (
        mutation.phase === "ambiguous"
        && canonical?.state === "available"
        && (
          canonical.revision !== mutation.expectedRevision
          || canonical.contentVersion !== mutation.expectedContentVersion
        )
      ) {
        this.deleteArtifactRemovalMutation(artifactId, mutation);
      }
    }
  }

  private rebuildWorkspaceSnapshot(): void {
    const threads = [...this.threads.values()];
    this.snapshot.projects = [...this.projects.values()]
      .filter((project) => project.state === "active")
      .map((project) => projectSummary(project, threads));
    this.snapshot.threads = threads
      .filter((thread) => thread.state === "open")
      .map((thread) => threadSummary(thread, this.projects.get(thread.projectId)?.name ?? "Unknown project"));
    this.snapshot.library = [...this.artifacts.values()]
      .filter((artifact) => artifact.state === "available")
      .map((artifact) => libraryItem(artifact, this.projects.get(artifact.projectId)?.name ?? "Unknown project"));
    this.snapshot.automations = [...this.automations.values()]
      .filter((automation) => automation.state !== "archived")
      .map((automation) => automationSummary(
        automation,
        this.projects.get(automation.projectId)?.name ?? "Unknown project",
      ));
  }

  private notifyConversation(threadId: string, conversation: ConversationDetail): void {
    for (const listener of this.conversationListeners.get(threadId) ?? []) {
      listener(structuredClone(conversation));
    }
  }

  private assertChatAvailable(): void {
    const capability = this.snapshot.capabilities.find((item) => item.id === "chat");
    if (!capability?.available) {
      throw new Error(capability?.reason ?? GROK_EXECUTION_UNAVAILABLE_REASON);
    }
  }

  private assertFilesAvailable(): void {
    const capability = this.snapshot.capabilities.find((item) => item.id === "files");
    if (!capability?.available) {
      throw new Error(capability?.reason ?? "Local artifact content is unavailable.");
    }
  }

  private async reloadConversationAfterExecution(threadId: string): Promise<ConversationDetail | undefined> {
    try {
      const conversation = await this.fetchConversation(threadId);
      this.installConversation(conversation);
      return conversation;
    } catch {
      // Preserve the original execution result when a best-effort refresh fails.
      return undefined;
    }
  }

  private applyStatus(status: DaemonStatus): void {
    this.snapshot.connection = status.state === "connected"
      ? {
          state: "online",
          profile: "Local workspace",
          plan: "Local daemon connected",
          serviceVersion: status.serviceVersion,
          agentRuntime: status.agentRuntime,
          automationScheduler: status.automationScheduler,
        }
      : {
          state: status.state === "starting" ? "connecting" : status.state === "stopped" ? "offline" : "degraded",
          profile: "Local workspace",
          plan: status.state === "starting" ? "Connecting" : "Limited mode",
          reason: status.reason,
          serviceVersion: status.serviceVersion,
          agentRuntime: status.agentRuntime,
          automationScheduler: status.automationScheduler,
        };
    this.emit();
  }

  private emit(): void {
    for (const listener of this.listeners) listener();
  }
}

function mapCapability(
  value: DaemonCapabilityStatus,
  schedulerState?: NonNullable<DaemonStatus["automationScheduler"]>["state"],
): CapabilityStatus {
  if (value.id === "automations") {
    const scheduler = schedulerState === "kernel_initialized_execution_disabled"
      ? {
          reasonCode: "automation_execution_unqualified",
          reason: "The scheduler journal is initialized, but isolated automation execution is not qualified.",
        }
      : schedulerState === "recovery_pending_execution_disabled"
        ? {
            reasonCode: "automation_scheduler_recovery_pending",
            reason: "The scheduler journal is recovering. Saved definitions remain inactive.",
          }
        : {
            reasonCode: "automation_scheduler_unavailable",
            reason: value.reason || AUTOMATION_DEFINITION_ONLY_REASON,
          };
    return {
      id: value.id,
      label: value.label,
      source: value.source,
      available: false,
      availability: "limited",
      authentication: value.authentication,
      ...scheduler,
    };
  }
  if (value.id === "work" && value.availability === "available") {
    return {
      id: value.id,
      label: value.label,
      source: value.source,
      available: false,
      availability: "unavailable",
      authentication: value.authentication,
      reasonCode: "execution_use_case_unavailable",
      reason: GROK_EXECUTION_UNAVAILABLE_REASON,
    };
  }
  return {
    id: value.id,
    label: value.label,
    source: value.source,
    available: value.availability === "available",
    availability: value.availability,
    authentication: value.authentication,
    reasonCode: value.reasonCode,
    reason: value.reason,
  };
}

function mapDesktopPreferences(value: DaemonDesktopPreferences): DesktopPreferences {
  return {
    keepRunningInNotificationArea: value.keepRunningInNotificationArea,
    revision: value.revision,
    updatedAtUnixMs: value.updatedAtUnixMs,
  };
}

function mapChatModelPreference(value: DaemonChatModelPreference): ChatModelPreference {
  return {
    selectedModelId: value.selectedModelId,
    revision: value.revision,
    updatedAtUnixMs: value.updatedAtUnixMs,
  };
}

function mapChatModelCatalog(value: DaemonChatModelCatalog): ChatModelCatalog {
  return {
    models: value.models.map((model) => ({
      id: model.id,
      aliases: [...model.aliases],
      inputModalities: [...model.inputModalities],
      outputModalities: [...model.outputModalities],
      textConversationReady: model.textConversationReady,
    })),
    preference: mapChatModelPreference(value.preference),
    defaultModelId: value.defaultModelId,
    selectedModelReady: value.selectedModelReady,
    defaultModelReady: value.defaultModelReady,
  };
}

function projectSummary(
  project: DaemonProject,
  threads: DaemonThread[],
): DesktopSnapshot["projects"][number] {
  return {
    id: project.id,
    name: project.name,
    description: project.description,
    accent: stableAccent(project.id),
    folders: 0,
    threads: threads.filter((thread) => thread.projectId === project.id && thread.state === "open").length,
    updatedAt: relativeTime(project.updatedAtUnixMs),
    activeRunCount: 0,
    instructions: "",
  };
}

function threadSummary(thread: DaemonThread, projectName: string): DesktopSnapshot["threads"][number] {
  return {
    id: thread.id,
    title: thread.title,
    projectName,
    preview: "Open to load the persisted conversation.",
    updatedAt: relativeTime(thread.updatedAtUnixMs),
    mode: "chat",
  };
}

function libraryItem(artifact: DaemonArtifact, projectName: string): DesktopSnapshot["library"][number] {
  return {
    id: artifact.id,
    name: artifact.name,
    type: artifactType(artifact),
    projectName,
    updatedAt: relativeTime(artifact.updatedAtUnixMs),
    size: artifact.byteSize === undefined ? "Size unavailable" : formatBytes(artifact.byteSize),
    ...(artifact.contentVersion === undefined ? {} : { contentVersion: artifact.contentVersion }),
    metadataRevision: artifact.revision,
  };
}

function automationSummary(automation: DaemonAutomation, projectName: string): AutomationSummary {
  const scheduleConfig = parseSchedule(automation.schedule, automation.timezone);
  return {
    id: automation.id,
    name: automation.title,
    projectId: automation.projectId,
    projectName,
    schedule: scheduleConfig ? scheduleLabel(scheduleConfig) : "Schedule unavailable",
    nextRun: "Not scheduled",
    lastResult: "never",
    enabled: false,
    scheduleConfig,
    prompt: automation.prompt,
    missedRunPolicy: automation.missedRunPolicy,
    overlapPolicy: automation.overlapPolicy,
    history: [],
  };
}

function mapConversation(
  thread: DaemonThread,
  messages: DaemonMessage[],
  turns: DaemonConversationTurn[],
  forkMetadata: DaemonConversationForkMetadata,
  projectName: string,
): ConversationDetail {
  const citationsByMessage = new Map<string, ConversationDetail["messages"][number]["citations"]>();
  for (const turn of turns) {
    if (!turn.assistantMessage) continue;
    citationsByMessage.set(
      turn.assistantMessage.id,
      mapConversationCitations(turn.citations, turn.turnId),
    );
  }
  for (const outcome of forkMetadata.inheritedAssistantOutcomes) {
    citationsByMessage.set(
      outcome.childAssistantMessageId,
      mapConversationCitations(
        outcome.citations,
        `${outcome.sourceTurnId}-${outcome.childAssistantMessageId}`,
      ),
    );
  }
  const family = forkMetadata.familyThreads.toSorted((left, right) => {
    if (left.id === forkMetadata.lineage.rootThreadId) return -1;
    if (right.id === forkMetadata.lineage.rootThreadId) return 1;
    return left.createdAtUnixMs - right.createdAtUnixMs || left.id.localeCompare(right.id);
  });
  let branchNumber = 0;
  const branches = family.map((familyThread) => {
    const lineage = familyThread.lineage;
    if (lineage.origin === "original") {
      return {
        threadId: familyThread.id,
        label: "Main",
        title: familyThread.title,
        kind: "main" as const,
        forkDepth: 0,
        current: familyThread.id === thread.id,
      };
    }
    branchNumber += 1;
    const label = lineage.kind === "edit_and_branch"
      ? `Edit ${branchNumber}`
      : lineage.kind === "regenerate"
        ? `Regenerate ${branchNumber}`
        : `Branch ${branchNumber}`;
    return {
      threadId: familyThread.id,
      label,
      title: familyThread.title,
      kind: lineage.kind,
      forkDepth: lineage.forkDepth,
      current: familyThread.id === thread.id,
    };
  });
  const currentBranch = branches.find((branch) => branch.current);
  if (!currentBranch) throw new Error("daemon conversation fork family omits the current thread");
  return {
    id: thread.id,
    title: thread.title,
    projectName,
    mode: "chat",
    branchName: currentBranch.label,
    branchCount: branches.length,
    branches,
    lineage: structuredClone(thread.lineage),
    messages: messages
      .filter((message) => message.state === "active" && message.role !== "system")
      .toSorted((left, right) => left.sequence - right.sequence)
      .map((message) => ({
        id: message.id,
        role: message.role as "user" | "assistant",
        content: message.content,
        state: "complete" as const,
        createdAt: new Date(message.createdAtUnixMs).toLocaleString(),
        citations: citationsByMessage.get(message.id) ?? [],
        attachments: [],
      })),
    turns: turns.map(mapConversationTurnDetail),
  };
}

function mapConversationCitations(
  citations: { title: string; url: string }[],
  identityPrefix: string,
): ConversationDetail["messages"][number]["citations"] {
  return citations.map((citation, index) => ({
    id: `${identityPrefix}-citation-${index + 1}`,
    title: citation.title || citationDomain(citation.url),
    url: citation.url,
    domain: citationDomain(citation.url),
    excerpt: "Source returned by the official xAI response.",
  }));
}

function mapConversationTurnDetail(turn: DaemonConversationTurn): ConversationTurnDetail {
  const lineage = validatedConversationLineage(turn);
  validateConversationRetryEligibility(turn);
  return {
    id: turn.turnId,
    state: turn.state,
    revision: turn.revision,
    modelId: turn.modelId,
    userMessageId: turn.userMessage.id,
    assistantMessageId: turn.assistantMessage?.id,
    failure: turn.failure,
    usage: turn.usage,
    zeroDataRetention: turn.zeroDataRetention,
    lineage,
    retryEligibility: turn.retryEligibility,
  };
}

function validatedConversationLineage(
  turn: DaemonConversationTurn,
): ConversationTurnDetail["lineage"] {
  if (turn.lineage.origin === "original") {
    if (turn.lineage.retryDepth !== 0 || "sourceTurnId" in turn.lineage) {
      throw new Error("daemon returned invalid original conversation lineage");
    }
    return { origin: "original", retryDepth: 0 };
  }
  if (
    turn.lineage.origin === "edit_and_branch"
    || turn.lineage.origin === "regenerate"
  ) {
    if (
      turn.lineage.sourceTurnId.length === 0
      || new TextEncoder().encode(turn.lineage.sourceTurnId).byteLength > 512
      || turn.lineage.sourceTurnId === turn.turnId
      || turn.lineage.retryDepth !== 0
    ) {
      throw new Error("daemon returned invalid fork conversation lineage");
    }
    return {
      origin: turn.lineage.origin,
      sourceTurnId: turn.lineage.sourceTurnId,
      retryDepth: 0,
    };
  }
  if (
    turn.lineage.origin !== "retry"
    || turn.lineage.sourceTurnId.length === 0
    || new TextEncoder().encode(turn.lineage.sourceTurnId).byteLength > 512
    || turn.lineage.sourceTurnId === turn.turnId
    || !Number.isSafeInteger(turn.lineage.retryDepth)
    || turn.lineage.retryDepth < 1
    || turn.lineage.retryDepth > 64
  ) {
    throw new Error("daemon returned invalid retry conversation lineage");
  }
  return {
    origin: "retry",
    sourceTurnId: turn.lineage.sourceTurnId,
    retryDepth: turn.lineage.retryDepth,
  };
}

function validateConversationRetryEligibility(turn: DaemonConversationTurn): void {
  const safeRetrySource = turn.state === "cancelled"
    || (turn.state === "failed" && turn.failure?.retryable === true);
  const retryDepthAvailable = turn.lineage.retryDepth < 64;
  const valid = {
    allowed: safeRetrySource && retryDepthAvailable,
    not_newest: safeRetrySource && retryDepthAvailable,
    source_in_progress: turn.state === "reserved" || turn.state === "provider_started",
    source_completed: turn.state === "completed",
    source_interrupted_needs_review: turn.state === "interrupted_needs_review",
    failure_not_retryable: turn.state === "failed" && turn.failure?.retryable === false,
    source_account_unavailable: safeRetrySource && retryDepthAvailable,
    depth_exhausted: safeRetrySource && turn.lineage.retryDepth === 64,
    source_read_only: safeRetrySource && retryDepthAvailable,
  }[turn.retryEligibility];
  if (!valid) throw new Error("daemon returned inconsistent conversation retry eligibility");
}

function validateRetryOutcome(
  retry: ConversationTurnDetail,
  source: ConversationTurnDetail,
  sourceTurnId: string,
): void {
  if (
    retry.id === sourceTurnId
    || retry.userMessageId === source.userMessageId
    || retry.lineage.origin !== "retry"
    || retry.lineage.sourceTurnId !== sourceTurnId
    || retry.lineage.retryDepth !== source.lineage.retryDepth + 1
    || retry.modelId !== source.modelId
  ) {
    throw new Error("daemon returned a retry with invalid lineage");
  }
}

function matchesEditableForkState(
  state: ConversationTurnDetail["state"],
): boolean {
  return state === "completed" || state === "cancelled" || state === "failed";
}

function validateConversationForkResponse(
  fork: DaemonConversationFork,
  kind: "branch" | "edit_and_branch" | "regenerate",
  parentThread: DaemonThread,
  parent: ConversationDetail,
  source: ConversationTurnDetail,
  sourceMessage: ConversationMessage,
  editedContent?: string,
): void {
  const lineage = fork.childThread.lineage;
  if (
    fork.delivery.childThreadId !== fork.childThread.id
    || (fork.delivery.state === "pending" && fork.delivery.revision !== 0)
    || (fork.delivery.state === "acknowledged" && fork.delivery.revision !== 1)
  ) {
    throw new Error("daemon returned a conversation fork with invalid delivery state");
  }
  const sourceUser = parent.messages.find((message) => message.id === source.userMessageId);
  const expectedSourceMessageId = kind === "edit_and_branch"
    ? source.userMessageId
    : source.assistantMessageId;
  if (
    !expectedSourceMessageId
    || fork.childThread.id === parent.id
    || fork.childThread.projectId !== parentThread.projectId
    || lineage.origin !== "fork"
    || lineage.parentThreadId !== parent.id
    || lineage.sourceTurnId !== source.id
    || lineage.sourceMessageId !== expectedSourceMessageId
    || lineage.kind !== kind
    || lineage.rootThreadId !== parent.lineage.rootThreadId
    || lineage.forkDepth !== parent.lineage.forkDepth + 1
  ) {
    throw new Error("daemon returned a conversation fork with invalid thread lineage");
  }
  if ((kind === "branch") !== (fork.startedTurn === undefined)) {
    throw new Error("daemon returned a conversation fork with invalid turn presence");
  }
  const started = fork.startedTurn;
  if (!started) return;
  const expectedOrigin = kind === "edit_and_branch" ? "edit_and_branch" : "regenerate";
  const expectedContent = kind === "edit_and_branch" ? editedContent : sourceUser?.content;
  const expectedDerivation = kind === "edit_and_branch" ? "edited_user" : "context_copy";
  if (
    expectedContent === undefined
    || started.turnId === source.id
    || started.userMessage.id === source.userMessageId
    || started.userMessage.threadId !== fork.childThread.id
    || started.run.threadId !== fork.childThread.id
    || started.run.projectId !== fork.childThread.projectId
    || started.modelId !== source.modelId
    || started.userMessage.content !== expectedContent
    || started.lineage.origin !== expectedOrigin
    || started.lineage.sourceTurnId !== source.id
    || started.userMessage.derivation.origin !== "fork"
    || started.userMessage.derivation.kind !== expectedDerivation
    || started.userMessage.derivation.sourceMessageId !== source.userMessageId
    || started.userMessage.derivation.sourceTurnId !== source.id
  ) {
    throw new Error("daemon returned a conversation fork with invalid child turn");
  }
  if (kind === "edit_and_branch" && sourceMessage.id !== source.userMessageId) {
    throw new Error("edited conversation fork selected the wrong source message");
  }
}

function validateCanonicalConversationFork(
  canonical: ConversationDetail,
  childCanonicalMessages: DaemonMessage[],
  returned: DaemonConversationFork,
  kind: "branch" | "edit_and_branch" | "regenerate",
  parentThread: DaemonThread,
  parent: ConversationDetail,
  parentCanonicalMessages: DaemonMessage[],
  source: ConversationTurnDetail,
  sourceMessage: ConversationMessage,
  editedContent?: string,
): void {
  const returnedLineage = returned.childThread.lineage;
  if (
    canonical.id !== returned.childThread.id
    || canonical.lineage.origin !== "fork"
    || returnedLineage.origin !== "fork"
    || canonical.lineage.parentThreadId !== parent.id
    || canonical.lineage.sourceTurnId !== source.id
    || canonical.lineage.sourceMessageId !== returnedLineage.sourceMessageId
    || canonical.lineage.kind !== kind
    || canonical.lineage.rootThreadId !== parent.lineage.rootThreadId
    || canonical.lineage.forkDepth !== parent.lineage.forkDepth + 1
    || !canonical.branches.some((branch) => branch.threadId === parent.id)
    || !canonical.branches.some((branch) => branch.threadId === canonical.id && branch.current)
    || parentThread.id !== parent.id
  ) {
    throw new Error("canonical conversation does not match the fork result");
  }
  const parentById = new Map(parentCanonicalMessages.map((message) => [message.id, message]));
  if (childCanonicalMessages.some((message) => parentById.has(message.id))) {
    throw new Error("conversation fork reused parent-owned message identity");
  }
  const creationPrefix: DaemonMessage[] = [];
  let reachedOrdinarySuffix = false;
  for (const message of childCanonicalMessages) {
    if (message.derivation.origin === "original") {
      reachedOrdinarySuffix = true;
      continue;
    }
    if (reachedOrdinarySuffix || message.derivation.sourceTurnId !== source.id) {
      throw new Error("canonical conversation has invalid fork message ancestry");
    }
    creationPrefix.push(message);
  }
  const finalCreationMessage = creationPrefix.at(-1);
  const sourceUser = parentById.get(source.userMessageId);
  const nonCompletedPromptIds = new Set(parent.turns
    .filter((turn) => turn.state !== "completed")
    .map((turn) => turn.userMessageId));
  const expectedSourceContext = parentCanonicalMessages
    .filter((message) => (
      message.state === "active"
      && message.sequence <= (sourceUser?.sequence ?? 0)
      && (message.id === source.userMessageId || !nonCompletedPromptIds.has(message.id))
    ))
    .toSorted((left, right) => left.sequence - right.sequence);
  if (
    !finalCreationMessage
    || !sourceUser
    || expectedSourceContext.at(-1)?.id !== sourceUser.id
  ) {
    throw new Error("canonical conversation omits its fork creation prefix");
  }
  for (const childMessage of creationPrefix) {
    if (childMessage.role !== "assistant" || childMessage.derivation.origin !== "fork") continue;
    const derivation = childMessage.derivation;
    const parentPresentation = parent.messages.find((message) => (
      message.id === derivation.sourceMessageId
    ));
    const childPresentation = canonical.messages.find((message) => message.id === childMessage.id);
    if (
      !parentPresentation
      || !childPresentation
      || !sameConversationCitations(parentPresentation.citations, childPresentation.citations)
    ) {
      throw new Error("canonical conversation changed inherited response citations");
    }
  }
  if (kind === "branch") {
    const sourceAssistant = parentById.get(sourceMessage.id);
    const contextPrefix = creationPrefix.slice(0, -1);
    if (
      returned.startedTurn
      || contextPrefix.length !== expectedSourceContext.length
      || !contextPrefix.every((message, index) => (
        contextCopyMatches(message, expectedSourceContext[index], index)
      ))
      || !sourceAssistant
      || finalCreationMessage.role !== "assistant"
      || finalCreationMessage.content !== sourceAssistant.content
      || finalCreationMessage.derivation.origin !== "fork"
      || finalCreationMessage.derivation.kind !== "source_assistant_copy"
      || finalCreationMessage.derivation.sourceMessageId !== sourceAssistant.id
      || finalCreationMessage.derivation.contextPosition !== undefined
      || canonical.turns.some((turn) => (
        childCanonicalMessages.findIndex((message) => message.id === turn.userMessageId)
          < creationPrefix.length
      ))
    ) {
      throw new Error("canonical Branch does not preserve the source response");
    }
    return;
  }
  const expectedContextPrefix = kind === "edit_and_branch"
    ? expectedSourceContext.slice(0, -1)
    : expectedSourceContext;
  const contextPrefix = kind === "edit_and_branch" ? creationPrefix.slice(0, -1) : creationPrefix;
  if (
    contextPrefix.length !== expectedContextPrefix.length
    || !contextPrefix.every((message, index) => (
      contextCopyMatches(message, expectedContextPrefix[index], index)
    ))
    || finalCreationMessage.role !== "user"
    || finalCreationMessage.derivation.origin !== "fork"
    || finalCreationMessage.derivation.sourceMessageId !== source.userMessageId
    || finalCreationMessage.derivation.contextPosition !== creationPrefix.length
    || (kind === "edit_and_branch"
      ? finalCreationMessage.derivation.kind !== "edited_user"
        || finalCreationMessage.content !== editedContent
      : finalCreationMessage.derivation.kind !== "context_copy"
        || finalCreationMessage.content !== sourceUser.content)
  ) {
    throw new Error("canonical conversation does not preserve the exact frozen fork context");
  }
  const returnedTurn = returned.startedTurn;
  const canonicalTurn = returnedTurn
    ? canonical.turns.find((turn) => turn.id === returnedTurn.turnId)
    : undefined;
  const canonicalUser = canonicalTurn
    && canonical.messages.find((message) => message.id === canonicalTurn.userMessageId);
  const expectedContent = kind === "edit_and_branch" ? editedContent : sourceUser?.content;
  if (
    !returnedTurn
    || !canonicalTurn
    || !canonicalUser
    || canonicalTurn.id !== returnedTurn.turnId
    || canonicalTurn.userMessageId !== returnedTurn.userMessage.id
    || canonicalTurn.userMessageId !== finalCreationMessage.id
    || canonicalTurn.modelId !== source.modelId
    || canonicalTurn.revision < returnedTurn.revision
    || canonicalTurn.lineage.origin !== kind
    || canonicalTurn.lineage.sourceTurnId !== source.id
    || canonicalUser.content !== expectedContent
  ) {
    throw new Error("canonical conversation omits the forked provider turn");
  }
  if (canonical.turns.some((turn) => {
    if (turn.id === canonicalTurn.id) return false;
    const userIndex = childCanonicalMessages.findIndex((message) => message.id === turn.userMessageId);
    return userIndex >= 0 && userIndex < creationPrefix.length;
  })) {
    throw new Error("canonical conversation exposes inherited context as an actionable turn");
  }
  if (canonicalTurn.assistantMessageId !== undefined && !canonical.messages.some((message) => (
    message.id === canonicalTurn.assistantMessageId && message.role === "assistant"
  ))) {
    throw new Error("canonical conversation fork contains an invalid provider output");
  }
}

function canonicalRetryOutcome(
  conversation: ConversationDetail,
  observedSource: ConversationTurnDetail,
  sourceTurnId: string,
  expectedRetryTurnId?: string,
): ConversationTurnDetail | undefined {
  const source = conversation.turns.find((turn) => turn.id === sourceTurnId);
  if (
    !source
    || source.revision !== observedSource.revision
    || source.modelId !== observedSource.modelId
    || source.lineage.retryDepth !== observedSource.lineage.retryDepth
  ) {
    return undefined;
  }
  const retries = conversation.turns.filter((turn) => (
    turn.lineage.origin === "retry"
    && turn.lineage.sourceTurnId === sourceTurnId
    && (!expectedRetryTurnId || turn.id === expectedRetryTurnId)
  ));
  if (retries.length !== 1) return undefined;
  const retry = retries[0];
  try {
    validateRetryOutcome(retry, source, sourceTurnId);
  } catch {
    return undefined;
  }
  const sourceMessage = conversation.messages.find((message) => message.id === source.userMessageId);
  const retryMessage = conversation.messages.find((message) => message.id === retry.userMessageId);
  if (!sourceMessage || !retryMessage || sourceMessage.content !== retryMessage.content) {
    return undefined;
  }
  return retry;
}

function conversationRetryEligibilityReason(
  eligibility: ConversationTurnDetail["retryEligibility"],
): string {
  return {
    allowed: "This request can be retried.",
    not_newest: "Only the newest conversation request can be retried.",
    source_in_progress: "The conversation request is still in progress.",
    source_completed: "Completed responses cannot use Retry. Use Regenerate on the response instead.",
    source_interrupted_needs_review: "An uncertain dispatched request must be reviewed and cannot be retried.",
    failure_not_retryable: "The provider reported that this failure is not retryable.",
    source_account_unavailable: "The original local xAI credential binding is no longer available.",
    depth_exhausted: "This conversation has reached the maximum safe Retry depth.",
    source_read_only: "This conversation or its project is archived and cannot accept a Retry.",
  }[eligibility];
}

function mapDaemonMessage(
  message: DaemonMessage,
  citations: ConversationMessage["citations"] = [],
): ConversationMessage {
  return {
    id: message.id,
    role: message.role as "user" | "assistant",
    content: message.content,
    state: "complete",
    createdAt: new Date(message.createdAtUnixMs).toLocaleString(),
    citations,
    attachments: [],
  };
}

function mappedCitations(turn: DaemonConversationTurn): ConversationMessage["citations"] {
  return turn.citations.map((citation, index) => ({
    id: `${turn.turnId}-citation-${index + 1}`,
    title: citation.title || citationDomain(citation.url),
    url: citation.url,
    domain: citationDomain(citation.url),
    excerpt: "Source returned by the official xAI response.",
  }));
}

function emptyConversation(thread: DaemonThread, projectName: string): ConversationDetail {
  return {
    id: thread.id,
    title: thread.title,
    projectName,
    mode: "chat",
    branchName: "Main",
    branchCount: 1,
    branches: [{
      threadId: thread.id,
      label: "Main",
      title: thread.title,
      kind: "main",
      forkDepth: 0,
      current: true,
    }],
    lineage: structuredClone(thread.lineage),
    messages: [],
    turns: [],
  };
}

function upsertConversationMessage(conversation: ConversationDetail, message: ConversationMessage): void {
  const index = conversation.messages.findIndex((item) => item.id === message.id);
  if (index >= 0) conversation.messages[index] = message;
  else conversation.messages.push(message);
}

function streamingMessageId(turnId: string): string {
  return `conversation-stream-${turnId}`;
}

function validateProjectionAgainstTurnPrefix(
  turn: ConversationTurnDetail,
  projection: ConversationTerminalPrefix,
): void {
  const expectedTurnRevision = turn.state === "reserved"
    ? 0
    : turn.state === "provider_started" || turn.state === "cancelled"
      ? 1
      : 2;
  if (projection.turnId !== turn.id || turn.revision !== expectedTurnRevision || projection.revision > 2) {
    throw new Error("conversation event projection does not match its canonical turn");
  }
  if (projection.revision === turn.revision && projection.state !== turn.state) {
    throw new Error("conversation event projection conflicts with canonical turn state");
  }
  if (projection.revision < turn.revision) {
    const validPrefix = projection.state === "reserved"
      && (turn.state === "provider_started" || isTerminalConversationState(turn.state))
      || projection.state === "provider_started" && isTerminalConversationState(turn.state);
    if (!validPrefix) throw new Error("conversation event projection is not a canonical prefix");
  }
  if (projection.revision > turn.revision) {
    const canonicalIsPrefix = turn.state === "reserved"
      && (projection.state === "provider_started" || Boolean(projection.state && isTerminalConversationState(projection.state)))
      || turn.state === "provider_started"
        && Boolean(projection.state && isTerminalConversationState(projection.state));
    if (!canonicalIsPrefix) throw new Error("canonical turn is not a prefix of its event projection");
  }
}

function validateTerminalProjection(
  conversation: ConversationDetail,
  turnId: string,
  projection: ConversationEventProjection,
): void {
  const turn = conversation.turns.find((item) => item.id === turnId);
  if (
    !turn
    || !projection.state
    || !isTerminalConversationState(projection.state)
    || turn.state !== projection.state
    || turn.revision !== projection.revision
  ) {
    throw new Error("canonical conversation does not match its terminal event projection");
  }
  if (turn.state === "completed") {
    const assistant = conversation.messages.find((message) => message.id === turn.assistantMessageId);
    if (!assistant || assistant.content !== projection.text) {
      throw new Error("canonical assistant does not match durable conversation text events");
    }
  } else if (turn.assistantMessageId) {
    throw new Error("non-completed conversation turn exposed an assistant message");
  }
  if (turn.state === "cancelled" && projection.text.length > 0) {
    throw new Error("cancelled conversation turn retained provider text");
  }
}

function conversationTurnReason(turn: DaemonConversationTurn): string {
  if (turn.failure?.message) return turn.failure.message;
  if (turn.state === "interrupted_needs_review") {
    return "The connection ended after dispatch. The request may have reached xAI; review is required and it cannot be retried automatically.";
  }
  if (turn.state === "cancelled") return "The request was cancelled before a response was committed.";
  if (turn.state === "reserved" || turn.state === "provider_started") {
    return "The request is still in progress. Reload the conversation before retrying.";
  }
  return "Grok did not return a completed response.";
}

function validatedPrompt(value: string): string {
  const prompt = value.trim();
  if (!prompt) throw new Error("Enter a message for Grok.");
  if ([...prompt].some((character) => {
    const codePoint = character.codePointAt(0) ?? 0;
    return codePoint <= 8
      || codePoint === 11
      || codePoint === 12
      || (codePoint >= 14 && codePoint <= 31)
      || (codePoint >= 127 && codePoint <= 159);
  })) {
    throw new Error("The message contains unsupported control characters.");
  }
  if (new TextEncoder().encode(prompt).byteLength > 1024 * 1024) {
    throw new Error("The message exceeds the 1 MiB limit.");
  }
  return prompt;
}

function contextCopyMatches(
  message: DaemonMessage,
  expected: DaemonMessage,
  index: number,
): boolean {
  if (message.derivation.origin !== "fork" || message.derivation.kind !== "context_copy") {
    return false;
  }
  return message.derivation.contextPosition === index + 1
    && message.derivation.sourceMessageId === expected.id
    && message.role === expected.role
    && message.content === expected.content;
}

async function conversationForkMutationKey(
  kind: "branch" | "edit_and_branch" | "regenerate",
  sourceTurnId: string,
  content?: string,
): Promise<string> {
  const material = new TextEncoder().encode(JSON.stringify([kind, sourceTurnId, content ?? null]));
  const digest = await crypto.subtle.digest("SHA-256", material);
  return Array.from(new Uint8Array(digest), (byte) => byte.toString(16).padStart(2, "0")).join("");
}

function sameConversationCitations(
  left: ConversationMessage["citations"],
  right: ConversationMessage["citations"],
): boolean {
  return left.length === right.length && left.every((citation, index) => {
    const candidate = right[index];
    return candidate !== undefined
      && citation.title === candidate.title
      && citation.url === candidate.url
      && citation.domain === candidate.domain
      && citation.excerpt === candidate.excerpt;
  });
}

function conversationTitle(prompt: string): string {
  let title = "";
  for (const character of prompt) {
    if (new TextEncoder().encode(title + character).byteLength > 160) break;
    title += character;
    if (["\n", ".", "?", "!"].includes(character) || [...title].length >= 80) break;
  }
  return title.replace(/\s+/g, " ").trim() || "New conversation";
}

function citationDomain(value: string): string {
  try {
    return new URL(value).hostname || "xAI source";
  } catch {
    return "xAI source";
  }
}

function serializeSchedule(schedule: AutomationSchedule): string {
  const time = schedule.localTime;
  if (!isCanonicalAutomationTime(time)) throw new Error("invalid automation local time");
  if (schedule.frequency === "daily" || schedule.frequency === "weekdays") {
    if (schedule.weekday !== undefined || schedule.dayOfMonth !== undefined) throw new Error("invalid automation cadence fields");
    return `v1;${schedule.frequency};${time}`;
  }
  if (schedule.frequency === "weekly") {
    if (schedule.dayOfMonth !== undefined || schedule.weekday === undefined || !Number.isInteger(schedule.weekday) || schedule.weekday < 0 || schedule.weekday > 6) {
      throw new Error("invalid weekly automation schedule");
    }
    return `v1;weekly;${schedule.weekday};${time}`;
  }
  if (schedule.weekday !== undefined || schedule.dayOfMonth === undefined || !Number.isInteger(schedule.dayOfMonth) || schedule.dayOfMonth < 1 || schedule.dayOfMonth > 31) {
    throw new Error("invalid monthly automation schedule");
  }
  return `v1;monthly;${schedule.dayOfMonth};${time}`;
}

function parseSchedule(value: string, timezone: string): AutomationSchedule | undefined {
  const canonical = parseCanonicalSchedule(value, timezone);
  if (canonical) return canonical;
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return undefined;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return undefined;
  const record = parsed as Record<string, unknown>;
  const frequency = record.frequency;
  const localTime = record.localTime;
  const weekday = record.weekday;
  const dayOfMonth = record.dayOfMonth;
  const timeZoneIana = record.timeZoneIana;
  const timeZoneWindows = record.timeZoneWindows;
  if (
    (frequency !== "daily" && frequency !== "weekdays" && frequency !== "weekly" && frequency !== "monthly")
    || typeof localTime !== "string"
    || !/^(?:[01]\d|2[0-3]):[0-5]\d$/.test(localTime)
    || (weekday !== undefined && (typeof weekday !== "number" || !Number.isInteger(weekday) || weekday < 0 || weekday > 6))
    || (dayOfMonth !== undefined && (typeof dayOfMonth !== "number" || !Number.isInteger(dayOfMonth) || dayOfMonth < 1 || dayOfMonth > 31))
    || (frequency === "weekly" && weekday === undefined)
    || (frequency === "monthly" && dayOfMonth === undefined)
    || ((frequency === "daily" || frequency === "weekdays") && (weekday !== undefined || dayOfMonth !== undefined))
    || (frequency === "weekly" && dayOfMonth !== undefined)
    || (frequency === "monthly" && weekday !== undefined)
    || (timeZoneIana !== undefined && typeof timeZoneIana !== "string")
    || (timeZoneWindows !== undefined && typeof timeZoneWindows !== "string")
  ) {
    return undefined;
  }
  const canonicalTimezone = canonicalAutomationTimezone(timezone);
  if (!canonicalTimezone) return undefined;
  if (
    typeof timeZoneIana === "string"
    && canonicalAutomationTimezone(timeZoneIana) !== canonicalTimezone
  ) {
    return undefined;
  }
  return {
    frequency,
    localTime,
    ...(weekday === undefined ? {} : { weekday: weekday as AutomationSchedule["weekday"] }),
    ...(dayOfMonth === undefined ? {} : { dayOfMonth }),
    timeZoneIana: canonicalTimezone,
    ...(timeZoneWindows === undefined ? {} : { timeZoneWindows }),
  };
}

function parseCanonicalSchedule(value: string, timezone: string): AutomationSchedule | undefined {
  const canonicalTimezone = canonicalAutomationTimezone(timezone);
  if (!canonicalTimezone) return undefined;
  const fields = value.split(";");
  if (fields[0] !== "v1") return undefined;
  const frequency = fields[1];
  if (frequency === "daily" || frequency === "weekdays") {
    if (fields.length !== 3 || !isCanonicalAutomationTime(fields[2])) return undefined;
    return { frequency, localTime: fields[2], timeZoneIana: canonicalTimezone };
  }
  if (frequency === "weekly") {
    if (fields.length !== 4 || !/^[0-6]$/.test(fields[2] ?? "") || !isCanonicalAutomationTime(fields[3])) return undefined;
    return {
      frequency,
      localTime: fields[3],
      weekday: Number(fields[2]) as AutomationSchedule["weekday"],
      timeZoneIana: canonicalTimezone,
    };
  }
  if (frequency === "monthly") {
    if (fields.length !== 4 || !/^(?:[1-9]|[12]\d|3[01])$/.test(fields[2] ?? "") || !isCanonicalAutomationTime(fields[3])) return undefined;
    return {
      frequency,
      localTime: fields[3],
      dayOfMonth: Number(fields[2]),
      timeZoneIana: canonicalTimezone,
    };
  }
  return undefined;
}

function isCanonicalAutomationTime(value: string | undefined): value is string {
  return typeof value === "string" && /^(?:[01]\d|2[0-3]):[0-5]\d$/.test(value);
}

function canonicalAutomationTimezone(value: string): string | undefined {
  try {
    return new Intl.DateTimeFormat("en", { timeZone: value }).resolvedOptions().timeZone;
  } catch {
    return undefined;
  }
}

function scheduleLabel(schedule: AutomationSchedule): string {
  const frequency = {
    daily: "Daily",
    weekdays: "Weekdays",
    weekly: "Weekly",
    monthly: "Monthly",
  }[schedule.frequency];
  return `${frequency} at ${schedule.localTime}`;
}

function relativeTime(timestamp: number): string {
  const delta = Math.max(0, Date.now() - timestamp);
  if (delta < 60_000) return "Now";
  if (delta < 3_600_000) return `${Math.floor(delta / 60_000)}m`;
  if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)}h`;
  return `${Math.floor(delta / 86_400_000)}d`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  return `${(bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`;
}

const DAEMON_ARTIFACT_KEYS = new Set([
  "id",
  "projectId",
  "threadId",
  "name",
  "mediaType",
  "byteSize",
  "contentVersion",
  "state",
  "revision",
  "createdAtUnixMs",
  "updatedAtUnixMs",
]);

function matchesArtifactRemovalTombstone(
  artifact: DaemonArtifact,
  artifactId: string,
  mutation: ArtifactRemovalMutation,
): boolean {
  return Object.keys(artifact).every((key) => DAEMON_ARTIFACT_KEYS.has(key))
    && artifact.id === artifactId
    && artifact.projectId === mutation.expectedProjectId
    && artifact.threadId === mutation.expectedThreadId
    && artifact.name === mutation.expectedName
    && artifact.state === "deleted"
    && artifact.revision === mutation.expectedRevision + 1
    && artifact.contentVersion === undefined
    && artifact.mediaType === undefined
    && artifact.byteSize === undefined
    && artifact.createdAtUnixMs === mutation.expectedCreatedAtUnixMs
    && artifact.updatedAtUnixMs >= mutation.expectedUpdatedAtUnixMs;
}

function artifactRemovalRejectionMessage(
  reason: Extract<BridgeResponse, { kind: "daemon.artifactRemovalRejected" }>["reason"],
): string {
  if (reason === "not_found" || reason === "conflict" || reason === "invalid_state") {
    return "The selected artifact version is no longer available.";
  }
  if (reason === "invalid_argument") {
    return "The removal request was rejected. Refresh the Library before trying again.";
  }
  return "The local imported copy could not be removed. Refresh the Library before trying again.";
}

const ARTIFACT_OPEN_FAILURE_CODES = new Set([
  "content_unavailable",
  "platform_unavailable",
  "deadline_exceeded",
  "integrity_failure",
  "interrupted_before_dispatch",
]);

function parseArtifactOpenReceipt(
  value: unknown,
  expectedArtifactId: string,
  expectedContentVersion: number,
): ArtifactOpenResult {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("invalid artifact open bridge response");
  }
  const receipt = value as Record<string, unknown>;
  if (
    receipt.artifactId !== expectedArtifactId
    || receipt.contentVersion !== expectedContentVersion
  ) {
    throw new Error("invalid artifact open bridge response");
  }
  const hasFailureCode = Object.hasOwn(receipt, "failureCode");
  const expectedKeys = hasFailureCode
    ? ["artifactId", "contentVersion", "failureCode", "status"]
    : ["artifactId", "contentVersion", "status"];
  if (
    Object.keys(receipt).length !== expectedKeys.length
    || expectedKeys.some((key) => !Object.hasOwn(receipt, key))
  ) {
    throw new Error("invalid artifact open bridge response");
  }
  if (receipt.status === "opened" || receipt.status === "interrupted_needs_review") {
    if (hasFailureCode) throw new Error("invalid artifact open bridge response");
    return {
      artifactId: expectedArtifactId,
      contentVersion: expectedContentVersion,
      status: receipt.status,
    };
  }
  if (
    receipt.status !== "failed"
    || !hasFailureCode
    || typeof receipt.failureCode !== "string"
    || !ARTIFACT_OPEN_FAILURE_CODES.has(receipt.failureCode)
  ) {
    throw new Error("invalid artifact open bridge response");
  }
  return {
    artifactId: expectedArtifactId,
    contentVersion: expectedContentVersion,
    status: "failed",
    failureCode: receipt.failureCode as Extract<ArtifactOpenResult, { status: "failed" }>["failureCode"],
  };
}

function artifactType(artifact: DaemonArtifact): DesktopSnapshot["library"][number]["type"] {
  const mediaType = artifact.mediaType?.toLocaleLowerCase() ?? "";
  if (mediaType.startsWith("image/")) return "image";
  if (mediaType.startsWith("video/")) return "video";
  if (mediaType.includes("json") || mediaType.includes("csv") || mediaType.includes("spreadsheet")) return "data";
  if (mediaType.includes("javascript") || mediaType.includes("typescript") || mediaType.includes("source")) return "code";
  return "document";
}

function stableAccent(id: string): string {
  const accents = ["#3f6758", "#526779", "#865f4b", "#715a78", "#5e6741"];
  let hash = 0;
  for (const character of id) hash = (hash * 31 + character.charCodeAt(0)) >>> 0;
  return accents[hash % accents.length] ?? accents[0];
}

function extensionCatalog(capabilities: DaemonCapabilityStatus[]): DesktopSnapshot["extensions"] {
  const browser = capabilities.find((item) => item.id === "browser_automation");
  return [
    {
      id: "browser",
      name: "Managed browser",
      description: "A private browser profile for research and web tasks.",
      kind: "built-in",
      status: browser?.availability === "available" ? "connected" : "attention",
      permissions: [browser?.reason ?? "Managed browser is not ready"],
    },
    {
      id: "filesystem",
      name: "Local files",
      description: "Local artifact metadata stored by the desktop daemon.",
      kind: "built-in",
      status: "attention",
      permissions: ["Folder linking, import, open, and export are not connected"],
    },
    {
      id: "wisp",
      name: "Wisp",
      description: "Optional managed desktop and virtual-machine automation backend.",
      kind: "managed",
      status: "available",
      permissions: ["Install required"],
      recommended: true,
      version: "Not installed",
    },
  ];
}

function degradedCapabilities(reason: string): CapabilityStatus[] {
  return [
    {
      id: "chat",
      label: "Grok chat",
      source: "desktop",
      available: false,
      availability: "limited",
      authentication: "either",
      reasonCode: "daemon_unavailable",
      reason: `${reason} Grok requests cannot be sent.`,
    },
    {
      id: "work",
      label: "Work runtime",
      source: "desktop",
      available: false,
      availability: "unavailable",
      authentication: "subscription_oauth",
      reasonCode: "daemon_unavailable",
      reason: "Work is disabled until the trusted local daemon is available.",
    },
    {
      id: "search",
      label: "Web & X search",
      source: "desktop",
      available: false,
      availability: "unavailable",
      authentication: "either",
      reasonCode: "daemon_unavailable",
      reason: "Search is disabled until the trusted local daemon is available.",
    },
    {
      id: "research",
      label: "Research",
      source: "desktop",
      available: false,
      availability: "unavailable",
      authentication: "xai_api_key",
      reasonCode: "daemon_unavailable",
      reason: "Research is disabled until the trusted local daemon is available.",
    },
  ];
}

function unavailable<T>(reason: string, status: "configuration_required" | "unavailable" = "unavailable"): ClientResult<T> {
  return { status, reason };
}

function productionWispDetail(): ManagedIntegrationDetail {
  return {
    id: "wisp",
    name: "Wisp",
    recommended: true,
    state: "available",
    availableVersion: "Not installed",
    checks: [
      { label: "Managed add-on service", state: "action_required", detail: "Installer support is not available in the current daemon protocol" },
      { label: "Signed component", state: "ready", detail: "Only signed compatibility metadata will be accepted" },
    ],
    permissions: ["Observe approved applications", "Send input after scoped approval", "Manage isolated VM sessions"],
    releaseNotes: ["Installation and update metadata will appear after the managed add-on service is connected."],
  };
}
