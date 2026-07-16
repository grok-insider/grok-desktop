import type {
  AccountSetupState,
  ArtifactOpenResult,
  ArtifactRemovalResult,
  AutomationDraft,
  AutomationSummary,
  ClientResult,
  ChatModelCatalog,
  ChatModelPreference,
  ConversationAttachment,
  ConversationDetail,
  ConversationTurnDetail,
  CreateProjectInput,
  DesktopClient,
  DesktopPreferences,
  DesktopSnapshot,
  HostExecutionEnrollment,
  HostExecutionPolicy,
  ManagedIntegrationDetail,
  LibraryItem,
  MediaCreation,
  StartRunInput,
  SuperGrokEnrollmentStatus,
  UpdateState,
  VoiceSession,
  VoiceSetup,
  WorkspaceSearchHit,
  WorkspaceSearchResults,
} from "./desktopClient";
import { initialSnapshot } from "./mockData";
import { formatAutomationSchedule } from "./automationSchedule";

const cloneSnapshot = (): DesktopSnapshot => structuredClone(initialSnapshot);

type MockConversationForkKind = "branch" | "edit_and_branch" | "regenerate";

export class MockDesktopClient implements DesktopClient {
  private snapshot = cloneSnapshot();
  private readonly listeners = new Set<() => void>();
  private readonly conversationListeners = new Map<string, Set<(conversation: ConversationDetail) => void>>();
  private readonly mediaListeners = new Map<"image" | "video", Set<(creations: MediaCreation[]) => void>>();
  private readonly conversations = seedConversations();
  private media = seedMedia();
  private account: AccountSetupState = accountState(true, true);
  private superGrokStatus: SuperGrokEnrollmentStatus = {
    state: "disconnected",
    verificationUri: "",
    userCode: "",
    expiresAtUnixMs: 0,
    credentialGeneration: 0,
    reasonCode: "",
  };
  private voiceSession: VoiceSession | undefined;
  private desktopPreferences: DesktopPreferences = {
    keepRunningInNotificationArea: true,
    updateChannel: "stable",
    revision: 0,
    updatedAtUnixMs: 0,
  };
  private chatModelCatalog: ChatModelCatalog = {
    models: [
      {
        id: "grok-4.3",
        aliases: ["grok-latest"],
        inputModalities: ["text"],
        outputModalities: ["text"],
        textConversationReady: true,
      },
      {
        id: "grok-4.3-fast",
        aliases: [],
        inputModalities: ["text"],
        outputModalities: ["text"],
        textConversationReady: true,
      },
    ],
    preference: { selectedModelId: "grok-4.3", revision: 0, updatedAtUnixMs: 0 },
    defaultModelId: "grok-4.3",
    selectedModelReady: true,
    defaultModelReady: true,
  };
  private conversationMutationSequence = 0;
  private hostPolicy: HostExecutionPolicy = {
    revision: 0,
    active: false,
    acknowledgmentVersion: 0,
    requiredAcknowledgmentVersion: 1,
    acknowledgedAtUnixMs: 0,
    filesystemRead: false,
    filesystemWrite: false,
    processExecute: false,
    pathRoots: [],
    broadScopeAcknowledged: false,
    updatedAtUnixMs: 0,
    runtimePrepared: false,
    unavailableReasonCode: "host_tools_not_enrolled",
  };

  constructor(options: { firstRun?: boolean } = {}) {
    if (options.firstRun) this.account = accountState(false, false);
  }

  async getSnapshot(): Promise<DesktopSnapshot> {
    return structuredClone(this.snapshot);
  }

  async selectHostWorkFolder(): Promise<string | undefined> {
    return "/home/friend/Work";
  }

  async getHostExecutionPolicy(): Promise<HostExecutionPolicy> {
    return structuredClone(this.hostPolicy);
  }

  async enrollHostExecution(input: HostExecutionEnrollment): Promise<HostExecutionPolicy> {
    this.hostPolicy = {
      ...this.hostPolicy,
      ...input,
      revision: input.expectedRevision + 1,
      active: true,
      acknowledgedAtUnixMs: Date.now(),
      updatedAtUnixMs: Date.now(),
      runtimePrepared: false,
      unavailableReasonCode: "host_tools_runtime_not_prepared",
    };
    return structuredClone(this.hostPolicy);
  }

  async revokeHostExecution(expectedRevision: number): Promise<HostExecutionPolicy> {
    this.hostPolicy = {
      ...this.hostPolicy,
      revision: expectedRevision + 1,
      active: false,
      runtimePrepared: false,
      unavailableReasonCode: "host_tools_not_enrolled",
    };
    return structuredClone(this.hostPolicy);
  }

  async prepareHostWorkRuntime(): Promise<HostExecutionPolicy> {
    this.hostPolicy = { ...this.hostPolicy, runtimePrepared: true, unavailableReasonCode: "" };
    return structuredClone(this.hostPolicy);
  }

  async deactivateHostWorkRuntime(): Promise<HostExecutionPolicy> {
    this.hostPolicy = {
      ...this.hostPolicy,
      runtimePrepared: false,
      unavailableReasonCode: "host_tools_runtime_not_prepared",
    };
    return structuredClone(this.hostPolicy);
  }

  async cancelHostWork(_runId: string): Promise<void> {}

  async decideHostWorkApproval(_input: {
    approvalId: string;
    expectedRevision: number;
    approved: boolean;
  }): Promise<void> {}

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  async startRun(input: StartRunInput): Promise<{ runId: string; threadId: string }> {
    const runId = `run-${Date.now()}`;
    const threadId = `thread-${Date.now()}`;
    const project = this.snapshot.projects.find((item) => item.id === input.projectId);
    this.snapshot.runs.unshift({
      id: runId,
      title: input.prompt,
      projectName: project?.name ?? "No project",
      state: input.mode === "work" ? "planning" : "running",
      progress: input.mode === "work" ? 8 : 18,
      updatedAt: "Now",
      detail: input.researchEnabled ? "Planning research sources" : "Preparing a response",
      steps: [
        { label: "Understand request", state: "active" },
        { label: input.searchEnabled ? "Review sources" : "Develop response", state: "waiting" },
        { label: "Deliver result", state: "waiting" },
      ],
    });
    const title = input.prompt.slice(0, 80);
    if (input.mode === "chat") {
      this.snapshot.threads.unshift({
        id: threadId,
        title,
        projectName: project?.name ?? "No project",
        preview: input.prompt,
        updatedAt: "Now",
        mode: "chat",
      });
      this.conversations.set(threadId, {
        id: threadId,
        title,
        projectName: project?.name ?? "No project",
        mode: "chat",
        branchName: "Main",
        branchCount: 1,
        branches: [{ threadId, label: "Main", title, kind: "main", forkDepth: 0, current: true }],
        lineage: { origin: "original", rootThreadId: threadId, forkDepth: 0 },
        messages: [
          { id: `${threadId}-user`, role: "user", content: input.prompt, state: "complete", createdAt: "Now", citations: [], attachments: [] },
          { id: `${threadId}-assistant`, role: "assistant", content: "I’m reviewing the available context", state: "streaming", createdAt: "Now", citations: [], attachments: [] },
        ],
        turns: [{
          id: `${threadId}-turn`,
          state: "provider_started",
          revision: 1,
          modelId: this.chatModelCatalog.preference.selectedModelId,
          userMessageId: `${threadId}-user`,
          usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
          lineage: { origin: "original", retryDepth: 0 },
          retryEligibility: "source_in_progress",
        }],
      });
      setTimeout(() => {
        const conversation = this.conversations.get(threadId);
        const response = conversation?.messages.find((message) => message.id === `${threadId}-assistant`);
        if (!conversation || !response || response.state !== "streaming") return;
        response.content = "I reviewed the sample context and organized the response into the key release decisions and follow-up actions.";
        response.state = "complete";
        const turn = conversation.turns.find((item) => item.id === `${threadId}-turn`);
        if (turn) {
          turn.state = "completed";
          turn.revision = 2;
          turn.assistantMessageId = response.id;
          turn.retryEligibility = "source_completed";
        }
        this.emitConversation(threadId);
      }, 120);
    } else {
      this.snapshot.threads.unshift({
        id: threadId,
        title,
        projectName: project?.name ?? "No project",
        preview: input.prompt,
        updatedAt: "Now",
        mode: "work",
      });
      const now = Date.now();
      this.conversations.set(threadId, {
        id: threadId,
        title,
        projectName: project?.name ?? "No project",
        mode: "work",
        branchName: "Main",
        branchCount: 1,
        branches: [{ threadId, label: "Main", title, kind: "main", forkDepth: 0, current: true }],
        lineage: { origin: "original", rootThreadId: threadId, forkDepth: 0 },
        messages: [
          { id: `${threadId}-user`, role: "user", content: input.prompt, state: "complete", createdAt: "Now", citations: [], attachments: [] },
        ],
        turns: [],
        workTurns: [{ runId, state: "planning", revision: 1, createdAtUnixMs: now, updatedAtUnixMs: now }],
      });
    }
    this.emit();
    return { runId, threadId };
  }

  async createProject(input: CreateProjectInput): Promise<ClientResult<DesktopSnapshot["projects"][number]>> {
    const project: DesktopSnapshot["projects"][number] = {
      id: `project-${Date.now()}`,
      name: input.name,
      description: input.description,
      accent: "#4f6f61",
      folders: 0,
      threads: 0,
      updatedAt: "Now",
      activeRunCount: 0,
      instructions: "",
    };
    this.snapshot.projects.unshift(project);
    this.emit();
    return success(structuredClone(project));
  }

  async importArtifact(_projectId: string): Promise<ClientResult<LibraryItem>> {
    return {
      status: "unavailable",
      reason: "File import is available only in the installed desktop application.",
    };
  }

  async openArtifact(
    _artifactId: string,
    _contentVersion: number,
  ): Promise<ClientResult<ArtifactOpenResult>> {
    return {
      status: "unavailable",
      reason: "Artifact opening is unavailable in the interface preview.",
    };
  }

  async removeArtifact(
    _artifactId: string,
    _expectedRevision: number,
    _expectedContentVersion: number,
  ): Promise<ArtifactRemovalResult> {
    return {
      status: "unavailable",
      reason: "Artifact removal is unavailable in the interface preview.",
    };
  }

  async getAccountSetup(): Promise<AccountSetupState> {
    this.account = {
      ...this.account,
      superGrok: this.superGrokStatus.state === "connected" ? "connected" : "not_connected",
    };
    return structuredClone(this.account);
  }

  async getDesktopPreferences(): Promise<DesktopPreferences> {
    return structuredClone(this.desktopPreferences);
  }

  async getUpdateState(): Promise<UpdateState> {
    return {
      phase: "unsupported", currentVersion: import.meta.env.VITE_APP_VERSION, targetVersion: "", channel: "beta",
      checkedAtUnixMs: 0, reasonCode: "development_install",
    };
  }

  async checkForUpdates(): Promise<UpdateState> {
    return this.getUpdateState();
  }

  async installUpdate(): Promise<boolean> {
    return false;
  }

  async updateDesktopPreferences(input: {
    expectedRevision: number;
    keepRunningInNotificationArea: boolean;
    updateChannel: "stable" | "beta";
  }): Promise<DesktopPreferences> {
    if (input.expectedRevision !== this.desktopPreferences.revision) {
      throw new Error("revision conflict");
    }
    this.desktopPreferences = {
      keepRunningInNotificationArea: input.keepRunningInNotificationArea,
      updateChannel: input.updateChannel,
      revision: input.expectedRevision + 1,
      updatedAtUnixMs: Date.now(),
    };
    return structuredClone(this.desktopPreferences);
  }

  async getChatModelCatalog(): Promise<ChatModelCatalog> {
    if (this.account.xaiApiKey !== "configured") {
      throw new Error("A user-owned xAI API key is not configured.");
    }
    return structuredClone(this.chatModelCatalog);
  }

  async getUsageSummary(input: import("./desktopClient").GetUsageSummaryInput): Promise<import("./desktopClient").UsageSummary> {
    return {
      inputTokens: 1_200,
      outputTokens: 340,
      costInUsdTicks: 0,
      turnCount: 3,
      scopeKind: input.scopeKind,
      scopeId: input.scopeId ?? "",
      window: input.window,
      asOfUnixMs: Date.now(),
    };
  }

  async selectChatModel(input: {
    expectedRevision: number;
    modelId: string;
  }): Promise<ChatModelPreference> {
    if (this.account.xaiApiKey !== "configured") throw new Error("xAI API key is not configured");
    if (input.expectedRevision !== this.chatModelCatalog.preference.revision) {
      throw new Error("revision conflict");
    }
    const model = this.chatModelCatalog.models.find((item) =>
      item.id === input.modelId || item.aliases.includes(input.modelId)
    );
    if (!model?.textConversationReady) throw new Error("model unavailable");
    this.chatModelCatalog.preference = {
      selectedModelId: model.id,
      revision: input.expectedRevision + 1,
      updatedAtUnixMs: Date.now(),
    };
    this.chatModelCatalog.selectedModelReady = true;
    return structuredClone(this.chatModelCatalog.preference);
  }

  async beginGrokBuildAuth(): Promise<ClientResult<{ verificationUri: string; userCode?: string; state: "browser_opened" | "device_code" }>> {
    this.account.grokBuild = "checking";
    return success({ verificationUri: "https://grok.com/", userCode: "GROK-DESKTOP", state: "device_code" });
  }

  async completeGrokBuildAuth(): Promise<ClientResult<AccountSetupState>> {
    this.account = accountState(true, this.account.xaiApiKey === "configured");
    return success(structuredClone(this.account));
  }

  async beginSuperGrokDeviceEnrollment(): Promise<SuperGrokEnrollmentStatus> {
    this.superGrokStatus = {
      state: "awaiting_user",
      verificationUri: "https://accounts.x.ai/device",
      userCode: "GROK-DESKTOP",
      expiresAtUnixMs: Date.now() + 600_000,
      credentialGeneration: 0,
      reasonCode: "",
    };
    return structuredClone(this.superGrokStatus);
  }

  async getSuperGrokEnrollmentStatus(): Promise<SuperGrokEnrollmentStatus> {
    return structuredClone(this.superGrokStatus);
  }

  async cancelSuperGrokEnrollment(): Promise<SuperGrokEnrollmentStatus> {
    this.superGrokStatus = { ...this.superGrokStatus, state: "disconnected", verificationUri: "", userCode: "" };
    return structuredClone(this.superGrokStatus);
  }

  async disconnectSuperGrok(): Promise<SuperGrokEnrollmentStatus> {
    this.superGrokStatus = { ...this.superGrokStatus, state: "disconnected", verificationUri: "", userCode: "" };
    this.account = { ...this.account, superGrok: "not_connected" };
    return structuredClone(this.superGrokStatus);
  }

  async enrollXaiApiKey(): Promise<ClientResult<AccountSetupState>> {
    this.account = accountState(this.account.grokBuild === "connected", true);
    return success(structuredClone(this.account));
  }

  async deleteXaiApiKey(): Promise<ClientResult<AccountSetupState>> {
    this.account = accountState(this.account.grokBuild === "connected", false);
    return success(structuredClone(this.account));
  }

  async getConversation(threadId: string): Promise<ClientResult<ConversationDetail>> {
    const conversation = this.conversations.get(threadId);
    return conversation ? success(structuredClone(conversation)) : { status: "unavailable", reason: "Conversation not found." };
  }

  async openExternalUrl(_url: string): Promise<ClientResult<void>> {
    return {
      status: "unavailable",
      reason: "External sources can be opened only from the installed desktop application.",
    };
  }

  async searchWorkspace(input: {
    projectId?: string;
    query: string;
    offset?: number;
    limit?: number;
  }): Promise<WorkspaceSearchResults> {
    const projectIds = new Map(this.snapshot.projects.map((project) => [project.name, project.id]));
    const hits: WorkspaceSearchHit[] = [
      ...this.snapshot.projects.map((project) => ({
        id: project.id,
        projectId: project.id,
        kind: "project" as const,
        title: project.name,
        snippet: project.description,
        updatedAtUnixMs: 0,
      })),
      ...this.snapshot.threads.map((thread) => ({
        id: thread.id,
        projectId: projectIds.get(thread.projectName) ?? "preview-project",
        threadId: thread.id,
        kind: "thread" as const,
        title: thread.title,
        snippet: thread.preview,
        updatedAtUnixMs: 0,
      })),
      ...this.snapshot.library.map((item) => ({
        id: item.id,
        projectId: projectIds.get(item.projectName) ?? "preview-project",
        kind: "artifact" as const,
        title: item.name,
        snippet: item.type,
        updatedAtUnixMs: 0,
      })),
      ...this.snapshot.automations.map((automation) => ({
        id: automation.id,
        projectId: automation.projectId,
        kind: "automation" as const,
        title: automation.name,
        snippet: automation.prompt ?? automation.schedule,
        updatedAtUnixMs: 0,
      })),
    ];
    const terms = input.query.trim().toLocaleLowerCase().split(/\s+/u);
    const matched = hits.filter((hit) => (
      (!input.projectId || hit.projectId === input.projectId)
      && terms.every((term) => `${hit.title} ${hit.snippet}`.toLocaleLowerCase().includes(term))
    ));
    const offset = input.offset ?? 0;
    const limit = input.limit ?? 8;
    const page = matched.slice(offset, offset + limit);
    const nextOffset = offset + limit;
    return {
      hits: structuredClone(page),
      nextOffset: nextOffset < matched.length ? nextOffset : undefined,
      hasMore: nextOffset < matched.length,
    };
  }

  subscribeConversation(threadId: string, listener: (conversation: ConversationDetail) => void): () => void {
    const listeners = this.conversationListeners.get(threadId) ?? new Set();
    listeners.add(listener);
    this.conversationListeners.set(threadId, listeners);
    return () => listeners.delete(listener);
  }

  async sendConversationMessage(
    threadId: string,
    content: string,
    attachments: ConversationAttachment[],
    _searchEnabled = false,
  ): Promise<ClientResult<{ messageId: string; turnId: string }>> {
    const conversation = this.conversations.get(threadId);
    if (!conversation) return { status: "unavailable", reason: "Conversation not found." };
    const userId = `message-${Date.now()}`;
    conversation.messages.push({ id: userId, role: "user", content, state: "complete", createdAt: "Now", citations: [], attachments: attachments.map((item) => ({ ...item, state: "ready", detail: "Ready" })) });
    if (conversation.mode === "work") {
      const runId = `${userId}-run`;
      const now = Date.now();
      conversation.workTurns ??= [];
      conversation.workTurns.push({ runId, state: "running", revision: 2, createdAtUnixMs: now, updatedAtUnixMs: now });
      this.emitConversation(threadId);
      setTimeout(() => {
        const turn = conversation.workTurns?.find((item) => item.runId === runId);
        if (!turn || turn.state !== "running") return;
        conversation.messages.push({
          id: `${userId}-response`,
          role: "assistant",
          content: "I completed the sample Host Work turn within the enrolled workspace.",
          state: "complete",
          createdAt: "Now",
          citations: [],
          attachments: [],
        });
        turn.state = "completed";
        turn.revision = 3;
        turn.updatedAtUnixMs = Date.now();
        this.emitConversation(threadId);
      }, 120);
      return success({ messageId: userId, turnId: runId });
    }
    const assistantId = `${userId}-response`;
    const turnId = `${userId}-turn`;
    const previous = conversation.turns.at(-1);
    if (previous && previous.state !== "reserved" && previous.state !== "provider_started") {
      previous.retryEligibility = "not_newest";
    }
    conversation.messages.push({ id: assistantId, role: "assistant", content: "I’m reviewing the available context", state: "streaming", createdAt: "Now", citations: [], attachments: [] });
    conversation.turns.push({
      id: turnId,
      state: "provider_started",
      revision: 1,
      modelId: this.chatModelCatalog.preference.selectedModelId,
      userMessageId: userId,
      usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
      lineage: { origin: "original", retryDepth: 0 },
      retryEligibility: "source_in_progress",
    });
    this.emitConversation(threadId);
    setTimeout(() => {
      const message = conversation.messages.find((item) => item.id === assistantId);
      if (!message || message.state !== "streaming") return;
      message.content = "I reviewed the available context and organized the response into the most relevant decisions and next actions.";
      message.state = "complete";
      const turn = conversation.turns.find((item) => item.id === turnId);
      if (turn) {
        turn.state = "completed";
        turn.revision = 2;
        turn.assistantMessageId = assistantId;
        turn.retryEligibility = "source_completed";
      }
      this.emitConversation(threadId);
    }, 120);
    return success({ messageId: assistantId, turnId });
  }

  async cancelConversationTurn(input: {
    turnId: string;
    expectedRevision: number;
  }): Promise<ClientResult<ConversationTurnDetail>> {
    for (const [threadId, conversation] of this.conversations) {
      const turn = conversation.turns.find((item) => item.id === input.turnId);
      if (!turn) continue;
      if (turn.revision !== input.expectedRevision) throw new Error("revision conflict");
      if (turn.state !== "reserved" && turn.state !== "provider_started") {
        return { status: "unavailable", reason: "The conversation turn is already terminal." };
      }
      turn.state = turn.state === "reserved" ? "cancelled" : "interrupted_needs_review";
      turn.revision += 1;
      turn.retryEligibility = turn.state === "cancelled"
        ? "allowed"
        : "source_interrupted_needs_review";
      const message = conversation.messages.find((item) => item.id === turn.assistantMessageId)
        ?? conversation.messages.find((item) => item.id === `${turn.userMessageId}-response`);
      if (message?.state === "streaming") message.state = "stopped";
      this.emitConversation(threadId);
      return success(structuredClone(turn));
    }
    return { status: "unavailable", reason: "The conversation turn is no longer available." };
  }

  async retryConversationTurn(input: {
    sourceTurnId: string;
    expectedRevision: number;
  }): Promise<ClientResult<ConversationTurnDetail>> {
    for (const [threadId, conversation] of this.conversations) {
      const sourceIndex = conversation.turns.findIndex((turn) => turn.id === input.sourceTurnId);
      if (sourceIndex < 0) continue;
      const source = conversation.turns[sourceIndex];
      if (source.revision !== input.expectedRevision) throw new Error("revision conflict");
      if (source.retryEligibility !== "allowed" || sourceIndex !== conversation.turns.length - 1) {
        return { status: "unavailable", reason: "This request is no longer eligible for retry." };
      }
      const sourceMessage = conversation.messages.find((message) => message.id === source.userMessageId);
      if (!sourceMessage) return { status: "unavailable", reason: "The retry source is incomplete." };

      this.conversationMutationSequence += 1;
      const suffix = `${Date.now()}-${this.conversationMutationSequence}`;
      const userId = `message-retry-${suffix}`;
      const assistantId = `${userId}-response`;
      const turnId = `${userId}-turn`;
      source.retryEligibility = "not_newest";
      conversation.messages.push({
        id: userId,
        role: "user",
        content: sourceMessage.content,
        state: "complete",
        createdAt: "Now",
        citations: [],
        attachments: [],
      });
      conversation.messages.push({
        id: assistantId,
        role: "assistant",
        content: "I’m retrying the same durable request",
        state: "streaming",
        createdAt: "Now",
        citations: [],
        attachments: [],
      });
      const retry: ConversationTurnDetail = {
        id: turnId,
        state: "provider_started",
        revision: 1,
        modelId: source.modelId,
        userMessageId: userId,
        usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
        lineage: {
          origin: "retry",
          sourceTurnId: source.id,
          retryDepth: source.lineage.retryDepth + 1,
        },
        retryEligibility: "source_in_progress",
      };
      conversation.turns.push(retry);
      this.emitConversation(threadId);
      setTimeout(() => {
        const current = conversation.turns.find((turn) => turn.id === turnId);
        const response = conversation.messages.find((message) => message.id === assistantId);
        if (!current || !response || current.state !== "provider_started") return;
        response.content = "The durable retry completed with a revised response.";
        response.state = "complete";
        current.state = "completed";
        current.revision = 2;
        current.assistantMessageId = assistantId;
        current.retryEligibility = "source_completed";
        this.emitConversation(threadId);
      }, 120);
      return success(structuredClone(retry));
    }
    return { status: "unavailable", reason: "The retry source is no longer available." };
  }

  async editConversationMessage(threadId: string, messageId: string, content: string): Promise<ClientResult<ConversationDetail>> {
    const parent = this.conversations.get(threadId);
    const source = parent?.turns.find((turn) => turn.userMessageId === messageId);
    const sourceMessage = parent?.messages.find((message) => message.id === messageId);
    if (
      !parent
      || !source
      || !sourceMessage
      || sourceMessage.role !== "user"
      || (
        source.state !== "completed"
        && source.state !== "cancelled"
        && source.state !== "failed"
      )
    ) {
      return { status: "unavailable", reason: "Only a completed, cancelled, or known failed prompt can be edited into a branch." };
    }
    if (content === sourceMessage.content) {
      return { status: "unavailable", reason: "Change the prompt before creating an edited branch." };
    }
    return this.createConversationFork(parent, source, sourceMessage, "edit_and_branch", content);
  }

  async regenerateConversationMessage(threadId: string, messageId: string): Promise<ClientResult<ConversationDetail>> {
    const parent = this.conversations.get(threadId);
    const source = parent?.turns.find((turn) => turn.assistantMessageId === messageId);
    const sourceMessage = parent?.messages.find((message) => message.id === messageId);
    if (!parent || !source || source.state !== "completed" || !sourceMessage || sourceMessage.role !== "assistant") {
      return { status: "unavailable", reason: "Only a completed Grok response can be regenerated." };
    }
    return this.createConversationFork(parent, source, sourceMessage, "regenerate");
  }

  async branchConversation(threadId: string, messageId: string): Promise<ClientResult<ConversationDetail>> {
    const parent = this.conversations.get(threadId);
    const source = parent?.turns.find((turn) => turn.assistantMessageId === messageId);
    const sourceMessage = parent?.messages.find((message) => message.id === messageId);
    if (!parent || !source || source.state !== "completed" || !sourceMessage || sourceMessage.role !== "assistant") {
      return { status: "unavailable", reason: "Only a completed Grok response can be branched." };
    }
    return this.createConversationFork(parent, source, sourceMessage, "branch");
  }

  async listMediaCreations(kind: "image" | "video"): Promise<ClientResult<MediaCreation[]>> {
    return success(structuredClone(this.media.filter((item) => item.kind === kind)));
  }

  subscribeMediaCreations(kind: "image" | "video", listener: (creations: MediaCreation[]) => void): () => void {
    const listeners = this.mediaListeners.get(kind) ?? new Set();
    listeners.add(listener);
    this.mediaListeners.set(kind, listeners);
    return () => listeners.delete(listener);
  }

  async createMedia(input: { kind: "image" | "video"; prompt: string; aspectRatio: string; duration?: string }): Promise<ClientResult<MediaCreation>> {
    const creation: MediaCreation = {
      id: `creation-${Date.now()}`,
      kind: input.kind,
      prompt: input.prompt,
      status: "queued",
      progress: 0,
      createdAt: "Now",
      duration: input.duration,
      aspectRatio: input.aspectRatio,
      provenance: { generator: "Grok Imagine", watermark: true, createdWithGrok: true },
      palette: input.kind === "image" ? "linear-gradient(135deg, #dce8e0, #587367)" : "linear-gradient(135deg, #e7dfd9, #725f58)",
    };
    this.media.unshift(creation);
    this.emitMedia(input.kind);
    setTimeout(() => { if (creation.status === "queued") { creation.status = "generating"; creation.progress = 48; this.emitMedia(input.kind); } }, 80);
    setTimeout(() => { if (creation.status === "generating") { creation.status = "completed"; creation.progress = 100; this.emitMedia(input.kind); } }, 220);
    return success(structuredClone(creation));
  }

  async cancelMedia(creationId: string): Promise<ClientResult<MediaCreation>> {
    const creation = this.media.find((item) => item.id === creationId);
    if (!creation) return { status: "unavailable", reason: "Creation not found." };
    creation.status = "cancelled";
    creation.progress = 0;
    this.emitMedia(creation.kind);
    return success(structuredClone(creation));
  }

  async getVoiceSetup(): Promise<VoiceSetup> {
    return {
      capability: "ready",
      inputDevices: [{ id: "default-mic", label: "Default microphone" }, { id: "studio-mic", label: "Studio microphone" }],
      outputDevices: [{ id: "default-speaker", label: "Default speakers" }, { id: "headphones", label: "Headphones" }],
      selectedInputId: "default-mic",
      selectedOutputId: "default-speaker",
    };
  }

  async startVoiceSession(_inputDeviceId: string, _outputDeviceId: string): Promise<ClientResult<VoiceSession>> {
    this.voiceSession = {
      id: `voice-${Date.now()}`,
      state: "listening",
      captions: [
        { speaker: "you", text: "Summarize the current launch risks.", final: true },
        { speaker: "grok", text: "I’m reviewing the latest project context now.", final: false },
      ],
    };
    return success(structuredClone(this.voiceSession));
  }

  async setVoiceSessionState(sessionId: string, state: "listening" | "interrupted" | "ended"): Promise<ClientResult<VoiceSession>> {
    if (!this.voiceSession || this.voiceSession.id !== sessionId) return { status: "unavailable", reason: "Voice session is not active." };
    this.voiceSession.state = state;
    if (state === "interrupted") this.voiceSession.captions.push({ speaker: "grok", text: "Response interrupted.", final: true });
    return success(structuredClone(this.voiceSession));
  }

  async saveAutomation(draft: AutomationDraft): Promise<ClientResult<AutomationSummary>> {
    const existing = draft.id ? this.snapshot.automations.find((item) => item.id === draft.id) : undefined;
    const project = this.snapshot.projects.find((item) => item.id === draft.projectId);
    if (!project) return { status: "unavailable", reason: "Select an existing project." };
    const automation: AutomationSummary = {
      id: existing?.id ?? `auto-${Date.now()}`,
      name: draft.name,
      projectId: project.id,
      projectName: project.name,
      prompt: draft.prompt,
      schedule: formatAutomationSchedule(draft.schedule),
      scheduleConfig: structuredClone(draft.schedule),
      nextRun: "Not scheduled",
      lastResult: "never",
      enabled: false,
      missedRunPolicy: draft.missedRunPolicy,
      overlapPolicy: draft.overlapPolicy,
      history: [],
    };
    if (existing) Object.assign(existing, automation);
    else this.snapshot.automations.unshift(automation);
    this.emit();
    return success(structuredClone(automation));
  }

  async getManagedIntegration(_integrationId: "wisp"): Promise<ClientResult<ManagedIntegrationDetail>> {
    return {
      status: "configuration_required",
      reason: "Wisp is not offered as a product install surface in this build.",
    };
  }

  async changeManagedIntegration(_integrationId: "wisp", _action: "install" | "update" | "rollback"): Promise<ClientResult<ManagedIntegrationDetail>> {
    return {
      status: "configuration_required",
      reason: "Wisp install, update, and rollback are not product surfaces until signed lifecycle IPC ships.",
    };
  }

  private emit(): void {
    for (const listener of this.listeners) listener();
  }

  private createConversationFork(
    parent: ConversationDetail,
    source: ConversationTurnDetail,
    sourceMessage: ConversationDetail["messages"][number],
    kind: MockConversationForkKind,
    editedContent?: string,
  ): ClientResult<ConversationDetail> {
    const sourceUserIndex = parent.messages.findIndex((message) => message.id === source.userMessageId);
    const sourceMessageIndex = parent.messages.findIndex((message) => message.id === sourceMessage.id);
    if (
      sourceUserIndex < 0
      || sourceMessageIndex < 0
      || (kind !== "edit_and_branch" && sourceUserIndex >= sourceMessageIndex)
    ) {
      return { status: "unavailable", reason: "The fork source is incomplete." };
    }

    this.conversationMutationSequence += 1;
    const suffix = `${Date.now()}-${this.conversationMutationSequence}`;
    const childThreadId = `thread-fork-${suffix}`;
    const contextEnd = kind === "branch"
      ? sourceMessageIndex + 1
      : kind === "regenerate"
        ? sourceUserIndex + 1
        : sourceUserIndex;
    const childMessages = parent.messages.slice(0, contextEnd).map((message, index) => {
      const copy = structuredClone(message);
      copy.id = `${childThreadId}-message-${index + 1}`;
      copy.state = "complete";
      copy.citations = copy.citations.map((citation, citationIndex) => ({
        ...citation,
        id: `${copy.id}-citation-${citationIndex + 1}`,
      }));
      return copy;
    });
    const childTurns: ConversationTurnDetail[] = [];
    let generatedTurn: {
      turnId: string;
      assistantMessageId: string;
      kind: Exclude<MockConversationForkKind, "branch">;
    } | undefined;

    if (kind === "edit_and_branch" || kind === "regenerate") {
      const userMessage = kind === "edit_and_branch"
        ? {
            id: `${childThreadId}-user`,
            role: "user" as const,
            content: editedContent ?? "",
            state: "complete" as const,
            createdAt: "Now",
            citations: [],
            attachments: [],
          }
        : childMessages.at(-1);
      if (!userMessage || userMessage.role !== "user") {
        return { status: "unavailable", reason: "The fork source is incomplete." };
      }
      if (kind === "edit_and_branch") childMessages.push(userMessage);

      const assistantMessageId = `${userMessage.id}-response`;
      const turnId = `${childThreadId}-turn`;
      childMessages.push({
        id: assistantMessageId,
        role: "assistant",
        content: kind === "edit_and_branch"
          ? "I’m responding to the edited prompt in a new branch"
          : "I’m regenerating this response in a new branch",
        state: "streaming",
        createdAt: "Now",
        citations: [],
        attachments: [],
      });
      childTurns.push({
        id: turnId,
        state: "provider_started",
        revision: 1,
        modelId: source.modelId,
        userMessageId: userMessage.id,
        usage: { inputTokens: 0, outputTokens: 0, costInUsdTicks: 0 },
        lineage: {
          origin: kind,
          sourceTurnId: source.id,
          retryDepth: 0,
        },
        retryEligibility: "source_in_progress",
      });
      generatedTurn = { turnId, assistantMessageId, kind };
    }

    const child: ConversationDetail = {
      id: childThreadId,
      title: parent.title,
      projectName: parent.projectName,
      mode: parent.mode,
      branchName: "",
      branchCount: 0,
      branches: [],
      lineage: {
        origin: "fork",
        rootThreadId: parent.lineage.rootThreadId,
        parentThreadId: parent.id,
        sourceTurnId: source.id,
        sourceMessageId: sourceMessage.id,
        kind,
        forkDepth: parent.lineage.forkDepth + 1,
      },
      messages: childMessages,
      turns: childTurns,
    };
    this.conversations.set(childThreadId, child);
    this.snapshot.threads.unshift({
      id: childThreadId,
      title: child.title,
      projectName: child.projectName,
      preview: child.messages.at(-1)?.content ?? child.title,
      updatedAt: "Now",
      mode: child.mode,
    });
    const project = this.snapshot.projects.find((item) => item.name === child.projectName);
    if (project) project.threads += 1;

    const familyThreadIds = this.refreshConversationFamily(parent.lineage.rootThreadId);
    this.emit();
    for (const familyThreadId of familyThreadIds) this.emitConversation(familyThreadId);
    if (generatedTurn) {
      this.scheduleForkCompletion(
        childThreadId,
        generatedTurn.turnId,
        generatedTurn.assistantMessageId,
        generatedTurn.kind,
      );
    }
    return success(structuredClone(child));
  }

  private refreshConversationFamily(rootThreadId: string): string[] {
    const family = [...this.conversations.values()]
      .filter((conversation) => conversation.lineage.rootThreadId === rootThreadId)
      .toSorted((left, right) => {
        if (left.id === rootThreadId) return -1;
        if (right.id === rootThreadId) return 1;
        return 0;
      });
    let branchNumber = 0;
    const summaries = family.map((conversation) => {
      if (conversation.lineage.origin === "original") {
        return {
          threadId: conversation.id,
          label: "Main",
          title: conversation.title,
          kind: "main" as const,
          forkDepth: 0,
        };
      }
      branchNumber += 1;
      return {
        threadId: conversation.id,
        label: conversation.lineage.kind === "edit_and_branch"
          ? `Edit ${branchNumber}`
          : conversation.lineage.kind === "regenerate"
            ? `Regenerate ${branchNumber}`
            : `Branch ${branchNumber}`,
        title: conversation.title,
        kind: conversation.lineage.kind,
        forkDepth: conversation.lineage.forkDepth,
      };
    });
    for (const conversation of family) {
      conversation.branches = summaries.map((summary) => ({
        ...summary,
        current: summary.threadId === conversation.id,
      }));
      conversation.branchCount = summaries.length;
      conversation.branchName = conversation.branches.find((branch) => branch.current)?.label ?? "Main";
    }
    return family.map((conversation) => conversation.id);
  }

  private scheduleForkCompletion(
    threadId: string,
    turnId: string,
    assistantMessageId: string,
    kind: Exclude<MockConversationForkKind, "branch">,
  ): void {
    setTimeout(() => {
      const conversation = this.conversations.get(threadId);
      const turn = conversation?.turns.find((item) => item.id === turnId);
      const response = conversation?.messages.find((message) => message.id === assistantMessageId);
      if (!conversation || !turn || !response || turn.state !== "provider_started" || response.state !== "streaming") return;
      response.content = kind === "edit_and_branch"
        ? "The edited branch response is complete, with its parent conversation preserved."
        : "Here is a revised response that emphasizes the evidence, unresolved assumptions, and concrete next actions.";
      response.state = "complete";
      turn.state = "completed";
      turn.revision = 2;
      turn.assistantMessageId = response.id;
      turn.retryEligibility = "source_completed";
      const thread = this.snapshot.threads.find((item) => item.id === threadId);
      if (thread) {
        thread.preview = response.content;
        thread.updatedAt = "Now";
      }
      this.emitConversation(threadId);
      this.emit();
    }, 120);
  }

  private emitConversation(threadId: string): void {
    const conversation = this.conversations.get(threadId);
    if (!conversation) return;
    for (const listener of this.conversationListeners.get(threadId) ?? []) listener(structuredClone(conversation));
  }

  private emitMedia(kind: "image" | "video"): void {
    const creations = structuredClone(this.media.filter((item) => item.kind === kind));
    for (const listener of this.mediaListeners.get(kind) ?? []) listener(creations);
  }
}

function success<T>(value: T): ClientResult<T> {
  return { status: "success", value };
}

function accountState(grok: boolean, api: boolean): AccountSetupState {
  return {
    grokBuild: grok ? "connected" : "not_connected",
    superGrok: "not_connected",
    xaiApiKey: api ? "configured" : "not_configured",
    limitedMode: !grok,
    checks: [
      { id: "daemon", label: "Local daemon", state: "ready", detail: "Connected and protocol compatible" },
      { id: "grok_auth", label: "Grok Build OAuth", state: grok ? "ready" : "action_required", detail: grok ? "Official Grok account connected" : "Connect an official Grok account" },
      { id: "xai_api", label: "xAI API key", state: api ? "ready" : "optional", detail: api ? "Stored in the operating system vault" : "Optional for Imagine, Voice, Files, and direct API capabilities" },
      { id: "isolation", label: "Protected Work", state: "ready", detail: "Utility environment qualified" },
      { id: "browser", label: "Managed browser", state: "ready", detail: "Dedicated browser profile ready" },
      { id: "computer_use", label: "Computer use", state: "optional", detail: "Enabled per application when requested" },
    ],
  };
}

function seedConversations(): Map<string, ConversationDetail> {
  const conversations = new Map<string, ConversationDetail>([
    ["thread-1", {
      id: "thread-1",
      title: "Q3 launch narrative",
      projectName: "Atlas launch",
      mode: "chat",
      branchName: "Main",
      branchCount: 1,
      branches: [{ threadId: "thread-1", label: "Main", title: "Q3 launch narrative", kind: "main", forkDepth: 0, current: true }],
      lineage: { origin: "original", rootThreadId: "thread-1", forkDepth: 0 },
      turns: [],
      messages: [
        { id: "m1", role: "user", content: "Turn the customer research into a concise launch narrative. Separate strong evidence from assumptions.", state: "complete", createdAt: "10:14 AM", citations: [], attachments: [{ id: "a1", name: "Interview themes.pdf", kind: "document", state: "ready", detail: "24 pages" }] },
        { id: "m2", role: "assistant", content: "The strongest launch narrative is built around faster operational clarity, not another analytics dashboard. Across the interviews, teams repeatedly described the cost of switching between disconnected tools and the uncertainty that follows.", state: "complete", createdAt: "10:15 AM", attachments: [], citations: [
          { id: "c1", title: "Customer interview synthesis", url: "https://example.com/research/interviews", domain: "Project file", excerpt: "Eight of twelve teams described context switching as the primary source of reporting delay." },
          { id: "c2", title: "2026 workflow benchmark", url: "https://example.com/benchmark", domain: "example.com", excerpt: "Operational teams spend a measurable portion of each week reconciling state between systems.", publishedAt: "May 2026" },
        ], artifact: { id: "artifact-1", title: "Launch narrative.md", kind: "markdown", version: 3, content: "# Atlas launch narrative\n\n## The operational clarity layer\n\nAtlas gives teams a trustworthy view of work without forcing another reporting ritual.\n\n### Evidence\n- Customer interviews consistently identify context switching as the main delay.\n- Existing tools expose activity but rarely explain what needs attention.\n\n### Assumptions to validate\n- Buyers will prioritize time-to-clarity over breadth of dashboards.\n- The initial integrations cover the highest-frequency workflows." } },
        { id: "m3", role: "assistant", content: "I’m checking the last two claims against the cited benchmark", state: "streaming", createdAt: "Now", attachments: [], citations: [] },
      ],
    }],
  ]);
  for (const thread of initialSnapshot.threads) {
    if (conversations.has(thread.id)) continue;
    conversations.set(thread.id, {
      id: thread.id,
      title: thread.title,
      projectName: thread.projectName,
      mode: thread.mode,
      branchName: "Main",
      branchCount: 1,
      branches: [{ threadId: thread.id, label: "Main", title: thread.title, kind: "main", forkDepth: 0, current: true }],
      lineage: { origin: "original", rootThreadId: thread.id, forkDepth: 0 },
      turns: [],
      messages: [{ id: `${thread.id}-message`, role: "assistant", content: thread.preview, state: "complete", createdAt: thread.updatedAt, citations: [], attachments: [] }],
    });
  }
  return conversations;
}

function seedMedia(): MediaCreation[] {
  return [
    { id: "media-1", kind: "image", prompt: "Editorial product launch visual with a clear workspace and human-scale detail", status: "completed", progress: 100, createdAt: "Today, 9:42 AM", aspectRatio: "16:9", provenance: { generator: "Grok Imagine", watermark: true, createdWithGrok: true }, palette: "linear-gradient(135deg, #d8e5dd, #526d62)" },
    { id: "media-2", kind: "image", prompt: "Clean operations control room with daylight and realistic screens", status: "generating", progress: 64, createdAt: "Now", aspectRatio: "16:9", provenance: { generator: "Grok Imagine", watermark: true, createdWithGrok: true }, palette: "linear-gradient(135deg, #dce4ea, #50677a)" },
    { id: "media-3", kind: "video", prompt: "Slow camera move through a collaborative launch review", status: "completed", progress: 100, createdAt: "Yesterday", duration: "8s", aspectRatio: "16:9", provenance: { generator: "Grok Imagine", watermark: true, createdWithGrok: true }, palette: "linear-gradient(135deg, #ebe2dc, #7b6255)" },
    { id: "media-4", kind: "video", prompt: "Animate the launch storyboard with subtle interface movement", status: "queued", progress: 0, createdAt: "Now", duration: "6s", aspectRatio: "16:9", provenance: { generator: "Grok Imagine", watermark: true, createdWithGrok: true }, palette: "linear-gradient(135deg, #e5e1ec, #665d79)" },
  ];
}
