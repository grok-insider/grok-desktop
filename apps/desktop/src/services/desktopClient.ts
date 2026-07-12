import type { DaemonArtifactOpenFailureCode, DaemonStatus } from "../contracts/bridge";

export type CapabilitySource = "subscription_acp" | "xai_api" | "desktop" | "managed_addon" | "web_handoff";
export type RunState =
  | "queued"
  | "planning"
  | "awaiting_approval"
  | "running"
  | "paused"
  | "completed"
  | "failed"
  | "cancelled"
  | "interrupted_needs_review";

export interface CapabilityStatus {
  id: string;
  label: string;
  source: CapabilitySource;
  available: boolean;
  availability?: "available" | "limited" | "unavailable";
  authentication?: "none" | "subscription_oauth" | "xai_api_key" | "either";
  reasonCode?: string;
  reason?: string;
}

export interface ProjectSummary {
  id: string;
  name: string;
  description: string;
  accent: string;
  folders: number;
  threads: number;
  updatedAt: string;
  activeRunCount: number;
  instructions: string;
}

export interface CreateProjectInput {
  name: string;
  description: string;
}

export interface RunSummary {
  id: string;
  title: string;
  projectName: string;
  state: RunState;
  progress: number;
  updatedAt: string;
  detail: string;
  steps: { label: string; state: "done" | "active" | "waiting" }[];
  approval?: { title: string; detail: string; risk: "low" | "medium" | "high" };
}

export interface ThreadSummary {
  id: string;
  title: string;
  projectName: string;
  preview: string;
  updatedAt: string;
  pinned?: boolean;
  mode: "chat" | "work";
}

export interface LibraryItem {
  id: string;
  name: string;
  type: "document" | "code" | "image" | "video" | "data";
  projectName: string;
  updatedAt: string;
  size: string;
  contentVersion?: number;
  metadataRevision: number;
  palette?: string;
}

interface ArtifactOpenResultIdentity {
  artifactId: string;
  contentVersion: number;
}

export type ArtifactOpenResult = ArtifactOpenResultIdentity & (
  | { status: "opened"; failureCode?: never }
  | { status: "failed"; failureCode: DaemonArtifactOpenFailureCode }
  | { status: "interrupted_needs_review"; failureCode?: never }
);

export type ArtifactRemovalResult =
  | ClientResult<void>
  | { status: "pending" };

export type WorkspaceSearchKind = "project" | "thread" | "message" | "artifact" | "automation";

export interface WorkspaceSearchHit {
  id: string;
  projectId: string;
  threadId?: string;
  kind: WorkspaceSearchKind;
  title: string;
  snippet: string;
  updatedAtUnixMs: number;
}

export interface WorkspaceSearchResults {
  hits: WorkspaceSearchHit[];
  nextOffset?: number;
  hasMore: boolean;
}

export interface AutomationSummary {
  id: string;
  name: string;
  projectId: string;
  projectName: string;
  schedule: string;
  nextRun: string;
  lastResult: "succeeded" | "failed" | "missed" | "never";
  enabled: boolean;
  scheduleConfig?: AutomationSchedule;
  prompt?: string;
  missedRunPolicy?: "run_once" | "skip";
  overlapPolicy?: "queue_one" | "skip";
  history?: AutomationRunRecord[];
}

export interface AutomationSchedule {
  frequency: "daily" | "weekdays" | "weekly" | "monthly";
  localTime: string;
  weekday?: 0 | 1 | 2 | 3 | 4 | 5 | 6;
  dayOfMonth?: number;
  timeZoneIana: string;
  timeZoneWindows?: string;
}

export interface AutomationRunRecord {
  id: string;
  startedAt: string;
  status: "succeeded" | "failed" | "missed" | "running";
  detail: string;
  duration?: string;
}

export interface ExtensionSummary {
  id: string;
  name: string;
  description: string;
  kind: "managed" | "mcp" | "built-in";
  status: "connected" | "available" | "attention";
  permissions: string[];
  recommended?: boolean;
  version?: string;
  updateVersion?: string;
}

export type ClientResult<T> =
  | { status: "success"; value: T }
  | { status: "cancelled" | "configuration_required" | "unavailable"; reason: string };

export interface ReadinessCheck {
  id: "daemon" | "grok_auth" | "xai_api" | "isolation" | "browser" | "computer_use";
  label: string;
  state: "ready" | "optional" | "action_required" | "unavailable";
  detail: string;
}

export interface AccountSetupState {
  grokBuild: "connected" | "not_connected" | "checking";
  superGrok: "connected" | "not_connected";
  xaiApiKey: "configured" | "not_configured";
  limitedMode: boolean;
  checks: ReadinessCheck[];
}

export interface SuperGrokEnrollmentStatus {
  state: "disconnected" | "starting" | "awaiting_user" | "connected" | "failed";
  verificationUri: string;
  userCode: string;
  expiresAtUnixMs: number;
  credentialGeneration: number;
  reasonCode: string;
}

export interface GrokAuthChallenge {
  verificationUri: string;
  userCode?: string;
  state: "browser_opened" | "device_code";
}

export interface ConversationCitation {
  id: string;
  title: string;
  url: string;
  domain: string;
  excerpt: string;
  publishedAt?: string;
}

export interface ConversationAttachment {
  id: string;
  name: string;
  kind: "document" | "image" | "data";
  state: "uploading" | "scanning" | "ready" | "failed";
  detail: string;
}

export interface ConversationArtifact {
  id: string;
  title: string;
  kind: "markdown" | "code" | "html" | "chart";
  content: string;
  version: number;
}

export interface ConversationMessage {
  id: string;
  role: "user" | "assistant";
  content: string;
  state: "sending" | "streaming" | "complete" | "error" | "stopped";
  createdAt: string;
  citations: ConversationCitation[];
  attachments: ConversationAttachment[];
  artifact?: ConversationArtifact;
}

export type ConversationRetryEligibility =
  | "allowed"
  | "not_newest"
  | "source_in_progress"
  | "source_completed"
  | "source_interrupted_needs_review"
  | "failure_not_retryable"
  | "source_account_unavailable"
  | "depth_exhausted"
  | "source_read_only";

export type ConversationTurnLineage =
  | { origin: "original"; retryDepth: 0 }
  | { origin: "retry"; sourceTurnId: string; retryDepth: number }
  | { origin: "edit_and_branch"; sourceTurnId: string; retryDepth: 0 }
  | { origin: "regenerate"; sourceTurnId: string; retryDepth: 0 };

export interface ConversationBranchSummary {
  threadId: string;
  label: string;
  title: string;
  kind: "main" | "branch" | "edit_and_branch" | "regenerate";
  forkDepth: number;
  current: boolean;
}

export type ConversationThreadLineage =
  | { origin: "original"; rootThreadId: string; forkDepth: 0 }
  | {
      origin: "fork";
      rootThreadId: string;
      parentThreadId: string;
      sourceTurnId: string;
      sourceMessageId: string;
      kind: "branch" | "edit_and_branch" | "regenerate";
      forkDepth: number;
    };

export interface ConversationTurnDetail {
  id: string;
  state: "reserved" | "provider_started" | "completed" | "failed" | "cancelled" | "interrupted_needs_review";
  revision: number;
  modelId: string;
  userMessageId: string;
  assistantMessageId?: string;
  failure?: {
    kind: "authentication" | "forbidden" | "invalid_request" | "rate_limited" | "unavailable" | "protocol";
    message: string;
    retryable: boolean;
  };
  usage: { inputTokens: number; outputTokens: number; costInUsdTicks: number };
  zeroDataRetention?: boolean;
  lineage: ConversationTurnLineage;
  retryEligibility: ConversationRetryEligibility;
}

export interface ConversationDetail {
  id: string;
  title: string;
  projectName: string;
  mode: "chat" | "work";
  branchName: string;
  branchCount: number;
  branches: ConversationBranchSummary[];
  lineage: ConversationThreadLineage;
  messages: ConversationMessage[];
  turns: ConversationTurnDetail[];
}

export interface MediaCreation {
  id: string;
  kind: "image" | "video";
  prompt: string;
  status: "queued" | "generating" | "completed" | "failed" | "cancelled";
  progress: number;
  createdAt: string;
  duration?: string;
  aspectRatio: string;
  provenance: { generator: "Grok Imagine"; watermark: boolean; createdWithGrok: boolean };
  palette: string;
}

export interface VoiceSetup {
  capability: "ready" | "configuration_required" | "unavailable";
  reason?: string;
  inputDevices: { id: string; label: string }[];
  outputDevices: { id: string; label: string }[];
  selectedInputId: string;
  selectedOutputId: string;
}

export interface VoiceSession {
  id: string;
  state: "connecting" | "listening" | "speaking" | "interrupted" | "ended";
  captions: { speaker: "you" | "grok"; text: string; final: boolean }[];
}

export interface AutomationDraft {
  id?: string;
  name: string;
  projectId: string;
  prompt: string;
  schedule: AutomationSchedule;
  missedRunPolicy: "run_once" | "skip";
  overlapPolicy: "queue_one" | "skip";
  /** When true and the daemon scheduler is armed, request an enabled definition. */
  enabled?: boolean;
}

export interface ManagedIntegrationDetail {
  id: "wisp";
  name: "Wisp";
  recommended: true;
  state: "available" | "installing" | "installed" | "update_available" | "rollback_available";
  installedVersion?: string;
  availableVersion: string;
  rollbackVersion?: string;
  progress?: number;
  revision?: number;
  signatureVerified?: boolean;
  checks: { label: string; state: "ready" | "action_required"; detail: string }[];
  permissions: string[];
  releaseNotes: string[];
}

export interface DesktopSnapshot {
  connection: {
    state: "online" | "offline" | "connecting" | "degraded";
    profile: string;
    plan: string;
    reason?: string;
    serviceVersion?: string;
    agentRuntime?: DaemonStatus["agentRuntime"];
    automationScheduler?: DaemonStatus["automationScheduler"];
    interfacePreview?: boolean;
  };
  capabilities: CapabilityStatus[];
  projects: ProjectSummary[];
  runs: RunSummary[];
  threads: ThreadSummary[];
  library: LibraryItem[];
  automations: AutomationSummary[];
  extensions: ExtensionSummary[];
}

export interface DesktopPreferences {
  keepRunningInNotificationArea: boolean;
  revision: number;
  updatedAtUnixMs: number;
}

export interface ChatModelPreference {
  selectedModelId: string;
  revision: number;
  updatedAtUnixMs: number;
}

export interface ChatModelDescriptor {
  id: string;
  aliases: string[];
  inputModalities: string[];
  outputModalities: string[];
  textConversationReady: boolean;
}

export interface ChatModelCatalog {
  models: ChatModelDescriptor[];
  preference: ChatModelPreference;
  defaultModelId: string;
  selectedModelReady: boolean;
  defaultModelReady: boolean;
}

export interface StartRunInput {
  prompt: string;
  mode: "chat" | "work";
  projectId?: string;
  searchEnabled: boolean;
  researchEnabled: boolean;
}

export interface DesktopClient {
  getSnapshot(): Promise<DesktopSnapshot>;
  subscribe(listener: () => void): () => void;
  startRun(input: StartRunInput): Promise<{ runId: string; threadId: string }>;
  createProject(input: CreateProjectInput): Promise<ClientResult<ProjectSummary>>;
  importArtifact(projectId: string): Promise<ClientResult<LibraryItem>>;
  openArtifact(artifactId: string, contentVersion: number): Promise<ClientResult<ArtifactOpenResult>>;
  removeArtifact(
    artifactId: string,
    expectedRevision: number,
    expectedContentVersion: number,
  ): Promise<ArtifactRemovalResult>;
  getAccountSetup(): Promise<AccountSetupState>;
  getDesktopPreferences(): Promise<DesktopPreferences>;
  updateDesktopPreferences(input: { expectedRevision: number; keepRunningInNotificationArea: boolean }): Promise<DesktopPreferences>;
  getChatModelCatalog(): Promise<ChatModelCatalog>;
  selectChatModel(input: { expectedRevision: number; modelId: string }): Promise<ChatModelPreference>;
  beginGrokBuildAuth(): Promise<ClientResult<GrokAuthChallenge>>;
  completeGrokBuildAuth(): Promise<ClientResult<AccountSetupState>>;
  beginSuperGrokDeviceEnrollment(): Promise<SuperGrokEnrollmentStatus>;
  getSuperGrokEnrollmentStatus(): Promise<SuperGrokEnrollmentStatus>;
  cancelSuperGrokEnrollment(): Promise<SuperGrokEnrollmentStatus>;
  disconnectSuperGrok(): Promise<SuperGrokEnrollmentStatus>;
  enrollXaiApiKey(): Promise<ClientResult<AccountSetupState>>;
  deleteXaiApiKey(): Promise<ClientResult<AccountSetupState>>;
  getConversation(threadId: string): Promise<ClientResult<ConversationDetail>>;
  openExternalUrl(url: string): Promise<ClientResult<void>>;
  searchWorkspace(input: { projectId?: string; query: string; offset?: number; limit?: number }): Promise<WorkspaceSearchResults>;
  subscribeConversation(threadId: string, listener: (conversation: ConversationDetail) => void): () => void;
  sendConversationMessage(threadId: string, content: string, attachments: ConversationAttachment[]): Promise<ClientResult<{ messageId: string; turnId: string }>>;
  cancelConversationTurn(input: { turnId: string; expectedRevision: number }): Promise<ClientResult<ConversationTurnDetail>>;
  retryConversationTurn(input: { sourceTurnId: string; expectedRevision: number }): Promise<ClientResult<ConversationTurnDetail>>;
  editConversationMessage(threadId: string, messageId: string, content: string): Promise<ClientResult<ConversationDetail>>;
  regenerateConversationMessage(threadId: string, messageId: string): Promise<ClientResult<ConversationDetail>>;
  branchConversation(threadId: string, messageId: string): Promise<ClientResult<ConversationDetail>>;
  listMediaCreations(kind: "image" | "video"): Promise<ClientResult<MediaCreation[]>>;
  subscribeMediaCreations(kind: "image" | "video", listener: (creations: MediaCreation[]) => void): () => void;
  createMedia(input: { kind: "image" | "video"; prompt: string; aspectRatio: string; duration?: string }): Promise<ClientResult<MediaCreation>>;
  cancelMedia(creationId: string): Promise<ClientResult<MediaCreation>>;
  getVoiceSetup(): Promise<VoiceSetup>;
  startVoiceSession(inputDeviceId: string, outputDeviceId: string): Promise<ClientResult<VoiceSession>>;
  setVoiceSessionState(sessionId: string, state: "listening" | "interrupted" | "ended"): Promise<ClientResult<VoiceSession>>;
  saveAutomation(draft: AutomationDraft): Promise<ClientResult<AutomationSummary>>;
  getManagedIntegration(integrationId: "wisp"): Promise<ClientResult<ManagedIntegrationDetail>>;
  changeManagedIntegration(integrationId: "wisp", action: "install" | "update" | "rollback"): Promise<ClientResult<ManagedIntegrationDetail>>;
}
