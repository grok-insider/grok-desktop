export type DaemonConnectionState = "starting" | "connected" | "degraded" | "stopped";

export interface DaemonStatus {
  state: DaemonConnectionState;
  serviceVersion?: string;
  protocolVersion?: number;
  instanceId?: string;
  reason?: string;
  agentRuntime?: {
    configured: boolean;
    healthy: boolean;
    protocolVersion: number;
    name: string;
    version: string;
    reasonCode: string;
    authMethods: { id: string; name: string; description: string }[];
    capabilities: {
      loadSession: boolean;
      embeddedContext: boolean;
      imageInput: boolean;
      audioInput: boolean;
      mcpHttp: boolean;
      mcpSse: boolean;
    };
  };
  automationScheduler?: {
    state:
      | "kernel_initialized_execution_disabled"
      | "kernel_initialized_execution_enabled"
      | "recovery_pending_execution_disabled"
      | "degraded_execution_disabled";
  };
  updatedAtUnixMs: number;
}

export interface DaemonSuperGrokEnrollmentStatus {
  state: "disconnected" | "starting" | "awaiting_user" | "connected" | "failed";
  verificationUri: string;
  userCode: string;
  expiresAtUnixMs: number;
  credentialGeneration: number;
  reasonCode: string;
}

export type DaemonCapabilityId =
  | "chat"
  | "work"
  | "files"
  | "shell"
  | "mcp"
  | "browser_automation"
  | "computer_use"
  | "search"
  | "research"
  | "imagine_image"
  | "imagine_video"
  | "realtime_voice"
  | "automations";

export interface DaemonCapabilityStatus {
  id: DaemonCapabilityId;
  label: string;
  source: "subscription_acp" | "xai_api" | "desktop" | "managed_addon" | "web_handoff";
  authentication: "none" | "subscription_oauth" | "xai_api_key" | "either";
  availability: "available" | "limited" | "unavailable";
  reasonCode: string;
  reason: string;
}

export type DaemonRunState =
  | "queued"
  | "planning"
  | "awaiting_approval"
  | "running"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted_needs_review";

export interface DaemonRun {
  id: string;
  projectId: string;
  threadId: string;
  state: DaemonRunState;
  revision: number;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
}

export interface DaemonApproval {
  id: string;
  runId: string;
  status: "pending" | "granted" | "denied" | "expired" | "cancelled";
  revision: number;
  action: { action: string; target: string; dataSummary: string; risk: "low" | "elevated" | "high" | "critical" };
  scope: "once" | "run" | "resource";
  resourceId?: string;
  expiresAtUnixMs: number;
}

export interface DaemonProject {
  id: string;
  name: string;
  description: string;
  state: "active" | "archived";
  revision: number;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
}

export type DaemonConversationForkKind = "branch" | "edit_and_branch" | "regenerate";

export interface DaemonConversationForkDelivery {
  childThreadId: string;
  state: "pending" | "acknowledged";
  revision: number;
}

export type DaemonConversationThreadLineage =
  | { origin: "original"; rootThreadId: string; forkDepth: 0 }
  | {
      origin: "fork";
      rootThreadId: string;
      parentThreadId: string;
      sourceTurnId: string;
      sourceMessageId: string;
      kind: DaemonConversationForkKind;
      forkDepth: number;
    };

export interface DaemonThread {
  id: string;
  projectId: string;
  title: string;
  state: "open" | "archived";
  revision: number;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
  lineage: DaemonConversationThreadLineage;
}

export type DaemonConversationMessageDerivation =
  | { origin: "original" }
  | {
      origin: "fork";
      sourceMessageId: string;
      sourceTurnId: string;
      contextPosition?: number;
      kind: "context_copy" | "source_assistant_copy" | "edited_user";
    };

export interface DaemonMessage {
  id: string;
  threadId: string;
  sequence: number;
  role: "system" | "user" | "assistant";
  content: string;
  state: "active" | "deleted";
  revision: number;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
  derivation: DaemonConversationMessageDerivation;
}

export interface DaemonArtifact {
  id: string;
  projectId: string;
  threadId?: string;
  name: string;
  mediaType?: string;
  byteSize?: number;
  contentVersion?: number;
  state: "unavailable" | "available" | "deleted";
  revision: number;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
}

export type DaemonArtifactOpenFailureCode =
  | "content_unavailable"
  | "platform_unavailable"
  | "deadline_exceeded"
  | "integrity_failure"
  | "interrupted_before_dispatch";

interface DaemonArtifactOpenReceiptIdentity {
  artifactId: string;
  contentVersion: number;
}

export type DaemonArtifactOpenReceipt = DaemonArtifactOpenReceiptIdentity & (
  | { status: "opened"; failureCode?: never }
  | { status: "failed"; failureCode: DaemonArtifactOpenFailureCode }
  | { status: "interrupted_needs_review"; failureCode?: never }
);

export type DaemonWorkspaceSearchKind = "project" | "thread" | "message" | "artifact" | "automation";

export interface DaemonWorkspaceSearchHit {
  id: string;
  projectId: string;
  threadId?: string;
  kind: DaemonWorkspaceSearchKind;
  title: string;
  snippet: string;
  updatedAtUnixMs: number;
}

export interface DaemonWorkspaceSearchResults {
  hits: DaemonWorkspaceSearchHit[];
  nextOffset?: number;
  hasMore: boolean;
}

/** Closed navigation payload emitted only after a v1 OS deep link is validated in Electron main. */
export type DesktopNavigationRoute =
  | { readonly version: 1; readonly route: "home" | "projects" | "activity" | "library" | "automations" | "extensions" | "settings" }
  | { readonly version: 1; readonly route: "project"; readonly projectId: string }
  | { readonly version: 1; readonly route: "conversation"; readonly threadId: string };

export interface DesktopNavigationDelivery {
  readonly deliveryId: number;
  readonly route: DesktopNavigationRoute;
}

export interface DaemonAutomation {
  id: string;
  projectId: string;
  title: string;
  prompt: string;
  schedule: string;
  timezone: string;
  missedRunPolicy: "run_once" | "skip";
  overlapPolicy: "queue_one" | "skip";
  state: "enabled" | "disabled" | "archived";
  revision: number;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
}

export interface DaemonWorkspaceSnapshot {
  projects: DaemonProject[];
  threads: DaemonThread[];
  messages: DaemonMessage[];
  artifacts: DaemonArtifact[];
  automations: DaemonAutomation[];
}

export interface DaemonAutomationInput {
  projectId: string;
  title: string;
  prompt: string;
  schedule: string;
  timezone: string;
  missedRunPolicy: DaemonAutomation["missedRunPolicy"];
  overlapPolicy: DaemonAutomation["overlapPolicy"];
  /** Epoch 18: request enabled scheduling when the daemon kernel is live. */
  scheduleActive?: boolean;
}

export interface DaemonAccountState {
  xaiApiKeyConfigured: boolean;
  xaiCapabilitiesResolved: boolean;
  grokBuildAuthenticated?: boolean;
}

export interface DaemonDesktopPreferences {
  keepRunningInNotificationArea: boolean;
  revision: number;
  updatedAtUnixMs: number;
}

export interface DaemonChatModelPreference {
  selectedModelId: string;
  revision: number;
  updatedAtUnixMs: number;
}

export interface DaemonChatModelDescriptor {
  id: string;
  aliases: string[];
  inputModalities: string[];
  outputModalities: string[];
  textConversationReady: boolean;
}

export interface DaemonChatModelCatalog {
  models: DaemonChatModelDescriptor[];
  preference: DaemonChatModelPreference;
  defaultModelId: string;
  selectedModelReady: boolean;
  defaultModelReady: boolean;
}

export type DaemonConversationTurnState =
  | "reserved"
  | "provider_started"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted_needs_review";

export type DaemonConversationFailureKind =
  | "authentication"
  | "forbidden"
  | "invalid_request"
  | "rate_limited"
  | "unavailable"
  | "protocol";

export type DaemonConversationRetryEligibility =
  | "allowed"
  | "not_newest"
  | "source_in_progress"
  | "source_completed"
  | "source_interrupted_needs_review"
  | "failure_not_retryable"
  | "source_account_unavailable"
  | "depth_exhausted"
  | "source_read_only";

export type DaemonConversationTurnLineage =
  | { origin: "original"; retryDepth: 0 }
  | { origin: "retry"; sourceTurnId: string; retryDepth: number }
  | { origin: "edit_and_branch"; sourceTurnId: string; retryDepth: 0 }
  | { origin: "regenerate"; sourceTurnId: string; retryDepth: 0 };

export interface DaemonConversationTurn {
  turnId: string;
  state: DaemonConversationTurnState;
  revision: number;
  modelId: string;
  userMessage: DaemonMessage;
  assistantMessage?: DaemonMessage;
  run: DaemonRun;
  failure?: {
    kind: DaemonConversationFailureKind;
    message: string;
    retryable: boolean;
  };
  citations: { title: string; url: string }[];
  usage: {
    inputTokens: number;
    outputTokens: number;
    costInUsdTicks: number;
  };
  zeroDataRetention?: boolean;
  lineage: DaemonConversationTurnLineage;
  retryEligibility: DaemonConversationRetryEligibility;
}

export interface DaemonConversationFork {
  childThread: DaemonThread;
  startedTurn?: DaemonConversationTurn;
  delivery: DaemonConversationForkDelivery;
}

export interface DaemonConversationInheritedOutcome {
  childAssistantMessageId: string;
  sourceTurnId: string;
  modelId: string;
  citations: { title: string; url: string }[];
  usage: {
    inputTokens: number;
    outputTokens: number;
    costInUsdTicks: number;
  };
  zeroDataRetention?: boolean;
}

export interface DaemonConversationForkMetadata {
  lineage: DaemonConversationThreadLineage;
  inheritedAssistantOutcomes: DaemonConversationInheritedOutcome[];
  familyThreads: DaemonThread[];
}

export type DaemonConversationTurnEvent =
  | { sequence: number; turnId: string; kind: "created" }
  | {
      sequence: number;
      turnId: string;
      kind: "state_changed";
      fromState: DaemonConversationTurnState;
      toState: DaemonConversationTurnState;
    }
  | {
      sequence: number;
      turnId: string;
      kind: "text_appended";
      startUtf8Offset: number;
      text: string;
    };

export interface DaemonConversationTurnEventBatch {
  events: DaemonConversationTurnEvent[];
  nextSequence: number;
  hasMore: boolean;
}

export interface DesktopConversationTurnEventNotification {
  turnId: string;
  batch: DaemonConversationTurnEventBatch;
}

export interface DesktopConversationTurnEventDelivery extends DesktopConversationTurnEventNotification {
  deliveryId: number;
}

export type BridgeRequest =
  | { kind: "runtime.info" }
  | { kind: "desktop.openExternalUrl"; url: string }
  | { kind: "window.minimize" }
  | { kind: "window.maximize" }
  | { kind: "window.close" }
  | { kind: "daemon.bootstrap" }
  | { kind: "daemon.getAccountState" }
  | { kind: "daemon.startGrokBuildAuth"; idempotencyKey: string }
  | { kind: "daemon.getGrokBuildAuthStatus" }
  | { kind: "daemon.beginSuperGrokDeviceEnrollment"; idempotencyKey: string }
  | { kind: "daemon.getSuperGrokEnrollmentStatus" }
  | { kind: "daemon.cancelSuperGrokEnrollment"; idempotencyKey: string }
  | { kind: "daemon.disconnectSuperGrok"; idempotencyKey: string }
  | { kind: "daemon.getManagedIntegration"; integrationId: string }
  | {
      kind: "daemon.changeManagedIntegration";
      integrationId: string;
      action: "install" | "update" | "rollback";
      expectedRevision: number;
      idempotencyKey: string;
    }
  | { kind: "daemon.getDesktopPreferences" }
  | { kind: "daemon.updateDesktopPreferences"; expectedRevision: number; keepRunningInNotificationArea: boolean; idempotencyKey: string }
  | { kind: "daemon.getChatModelCatalog" }
  | { kind: "daemon.selectChatModel"; expectedRevision: number; modelId: string; idempotencyKey: string }
  | { kind: "daemon.enrollXaiApiKey"; idempotencyKey: string }
  | { kind: "daemon.deleteXaiApiKey"; idempotencyKey: string }
  | { kind: "daemon.createProject"; name: string; description: string; idempotencyKey: string }
  | { kind: "daemon.createThread"; projectId: string; title: string; idempotencyKey: string }
  | { kind: "daemon.importArtifact"; projectId: string; idempotencyKey: string }
  | { kind: "daemon.openArtifact"; artifactId: string; contentVersion: number; idempotencyKey: string }
  | {
      kind: "daemon.removeArtifact";
      artifactId: string;
      expectedRevision: number;
      expectedContentVersion: number;
      idempotencyKey: string;
    }
  | { kind: "daemon.getConversation"; threadId: string }
  | { kind: "daemon.searchWorkspace"; projectId?: string; query: string; offset: number; limit: number }
  | { kind: "daemon.startConversationTurn"; threadId: string; content: string; idempotencyKey: string }
  | { kind: "daemon.cancelConversationTurn"; turnId: string; expectedRevision: number; idempotencyKey: string }
  | { kind: "daemon.retryConversationTurn"; sourceTurnId: string; expectedRevision: number; idempotencyKey: string }
  | { kind: "daemon.branchConversationThread"; sourceTurnId: string; expectedRevision: number; idempotencyKey: string }
  | { kind: "daemon.editAndBranchConversationTurn"; sourceTurnId: string; expectedRevision: number; content: string; idempotencyKey: string }
  | { kind: "daemon.regenerateConversationTurn"; sourceTurnId: string; expectedRevision: number; idempotencyKey: string }
  | { kind: "daemon.getConversationForkMetadata"; threadId: string }
  | { kind: "daemon.acknowledgeConversationForkDelivery"; childThreadId: string; expectedRevision: number; idempotencyKey: string }
  | ({ kind: "daemon.createAutomation"; idempotencyKey: string } & DaemonAutomationInput)
  | ({ kind: "daemon.updateAutomation"; automationId: string; expectedRevision: number; idempotencyKey: string } & DaemonAutomationInput)
  | { kind: "daemon.decideApproval"; approvalId: string; expectedRevision: number; approved: boolean; idempotencyKey: string };

export type BridgeResponse =
  | { kind: "runtime.info"; platform: string; version: string }
  | { kind: "desktop.externalUrlOpened"; accepted: true }
  | { kind: "desktop.externalUrlOpenFailed"; reason: "rejected" | "busy" | "unavailable" }
  | { kind: "window.action"; accepted: true }
  | { kind: "daemon.bootstrap"; status: DaemonStatus; capabilities: DaemonCapabilityStatus[]; accountState: DaemonAccountState; workspace: DaemonWorkspaceSnapshot }
  | { kind: "daemon.accountState"; accountState: DaemonAccountState }
  | { kind: "daemon.grokBuildAuthStatus"; state: string; authenticated: boolean }
  | { kind: "daemon.superGrokEnrollmentStatus"; status: DaemonSuperGrokEnrollmentStatus }
  | {
      kind: "daemon.managedIntegration";
      integration: {
        id: string;
        state: string;
        installedVersion: string;
        availableVersion: string;
        rollbackVersion: string;
        revision: number;
        signatureVerified: boolean;
      };
    }
  | { kind: "daemon.desktopPreferences"; preferences: DaemonDesktopPreferences }
  | { kind: "daemon.chatModelCatalog"; catalog: DaemonChatModelCatalog }
  | { kind: "daemon.chatModelPreference"; preference: DaemonChatModelPreference }
  | { kind: "daemon.credentialEnrollmentFailure"; reason: "cancelled" | "integrity_failure" }
  | { kind: "daemon.project"; project: DaemonProject }
  | { kind: "daemon.thread"; thread: DaemonThread }
  | { kind: "daemon.artifactImported"; artifact: DaemonArtifact }
  | { kind: "daemon.artifactImportCancelled" }
  | { kind: "daemon.artifactOpened"; receipt: DaemonArtifactOpenReceipt }
  | { kind: "daemon.artifactRemoved"; artifact: DaemonArtifact }
  | {
      kind: "daemon.artifactRemovalPending";
      artifactId: string;
      expectedRevision: number;
      expectedContentVersion: number;
      tombstone: DaemonArtifact;
    }
  | {
      kind: "daemon.artifactRemovalRejected";
      reason: "invalid_argument" | "not_found" | "conflict" | "invalid_state";
    }
  | {
      kind: "daemon.conversation";
      thread: DaemonThread;
      messages: DaemonMessage[];
      turns: DaemonConversationTurn[];
      forkMetadata: DaemonConversationForkMetadata;
    }
  | { kind: "daemon.workspaceSearchResults"; results: DaemonWorkspaceSearchResults }
  | { kind: "daemon.conversationTurn"; turn: DaemonConversationTurn }
  | { kind: "daemon.conversationFork"; fork: DaemonConversationFork }
  | { kind: "daemon.conversationForkMetadata"; metadata: DaemonConversationForkMetadata }
  | { kind: "daemon.conversationForkDelivery"; delivery: DaemonConversationForkDelivery }
  | { kind: "daemon.automation"; automation: DaemonAutomation }
  | { kind: "daemon.approval"; approval: DaemonApproval };

export type DesktopBridge = {
  request(request: BridgeRequest): Promise<BridgeResponse>;
  onDaemonStatus(listener: (status: DaemonStatus) => void): () => void;
  onConversationTurnEvents(
    listener: (notification: DesktopConversationTurnEventNotification) => void | Promise<void>,
  ): () => void;
  onNavigationRoute(listener: (route: DesktopNavigationRoute) => void): () => void;
};
