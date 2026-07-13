//! Framework-independent product use cases and outbound ports.

mod agent_runtime;
mod approvals;
mod artifacts;
mod automation_scheduler;
mod capabilities;
mod chat_models;
mod chat_rail;
mod conversation;
mod credential_enrollment;
mod credentials;
mod effects;
mod error;
mod grok_build_auth;
mod isolation;
mod isolation_runtime;
mod managed_integrations;
mod models;
mod mutations;
mod ports;
mod preferences;
mod privileged_gateway;
mod privileged_operations;
mod runs;
mod supergrok_oauth;
mod usage_summary;
mod vault;
mod workspace;

pub use agent_runtime::{
    AgentAuthMethod, AgentEvent, AgentEventStream, AgentPermissionDecision, AgentPermissionOption,
    AgentPermissionOptionKind, AgentPermissionRequest, AgentPrompt, AgentRuntime,
    AgentRuntimeCapabilities, AgentRuntimeError, AgentRuntimeErrorKind, AgentRuntimeProbe,
    AgentSession, AgentSessionRequest, AgentToolCall, AgentToolCallStatus, HostToolsMcpServer,
};
pub use approvals::{ApprovalService, RequestApproval};
pub use artifacts::{
    ARTIFACT_IMPORT_IO_TIMEOUT_MS, ARTIFACT_OPEN_TIMEOUT_MS, ARTIFACT_REMOVAL_IO_TIMEOUT_MS,
    ArtifactContentPublication, ArtifactContentPurge, ArtifactContentReadyResult,
    ArtifactContentRetention, ArtifactContentStatus, ArtifactContentStore,
    ArtifactImportFailureCode, ArtifactImportPlan, ArtifactImportRecoverySummary,
    ArtifactImportReservation, ArtifactImportState, ArtifactOpenError, ArtifactOpenFailureCode,
    ArtifactOpenPlan, ArtifactOpenReceipt, ArtifactOpenReceiptStatus, ArtifactOpenRecoverySummary,
    ArtifactOpenReservation, ArtifactOpenState, ArtifactOpener, ArtifactQuotaUsage,
    ArtifactRemovalPlan, ArtifactRemovalRecoverySummary, ArtifactRemovalReservation,
    ArtifactRemovalResolution, ArtifactRemovalState, ArtifactRetentionFailureCode,
    ArtifactRetentionRecord, ArtifactRetentionState, ArtifactService, ArtifactStore,
    ImportArtifact, MAX_ARTIFACT_FILE_BYTES, MAX_ARTIFACT_RECOVERY_BATCH,
    MAX_GLOBAL_ARTIFACT_BYTES, MAX_PROJECT_ARTIFACT_BYTES, MAX_PROJECT_ARTIFACT_COUNT,
    OpenArtifact, PreparedArtifactContent, RemoveArtifact, SelectedSourcePath,
};
pub use automation_scheduler::{
    AUTOMATION_SCHEDULER_LEASE_TTL_MS, AutomationOccurrenceClaimAttempt,
    AutomationOccurrenceClaimCompletion, AutomationOccurrenceDispatch,
    AutomationOccurrenceDispatchResult, AutomationOccurrenceRunCompletion,
    AutomationScheduleCandidate, AutomationScheduleEvaluationCommit,
    AutomationScheduleEvaluationResult, AutomationSchedulerJournalStatus,
    AutomationSchedulerLeaseAcquisition, AutomationSchedulerRecoverySummary,
    AutomationSchedulerService, AutomationSchedulerStore, AutomationSchedulerTickStatus,
    AutomationSchedulerTickSummary, ClaimAutomationOccurrence,
    MAX_AUTOMATION_SCHEDULER_EVALUATION_OCCURRENCES, MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH,
    MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS, ScheduledGuestDispatchError,
    ScheduledGuestDispatcher, ScheduledGuestOutcome, ScheduledGuestRequest,
    automation_occurrence_is_active,
};
pub use capabilities::{CapabilityFacts, CapabilityResolver};
pub use chat_models::{ChatModelCatalog, ChatModelCatalogEntry, ChatModelService, SelectChatModel};
pub use chat_rail::ChatRailSelection;
pub use conversation::{
    AcknowledgeConversationForkDelivery, BranchConversationThread, CancelConversationTurnCommit,
    ConversationCancellationSignal, ConversationForkCommandResolution, ConversationForkDelivery,
    ConversationForkDeliveryState, ConversationForkMetadata, ConversationForkPlan,
    ConversationForkReservation, ConversationForkSnapshot, ConversationForkTurnPlan,
    ConversationInheritedAssistantOutcome, ConversationRecoverySummary, ConversationService,
    ConversationThreadCredentialBinding, ConversationThreadModelBinding, ConversationTurnDispatch,
    ConversationTurnEventPage, ConversationTurnReservation, ConversationTurnReservationSource,
    ConversationTurnSnapshot, ConversationTurnStore, EditAndBranchConversationTurn,
    ExecuteConversationTurn, MAX_CONVERSATION_CONTEXT_BYTES, MAX_CONVERSATION_CONTEXT_MESSAGES,
    MAX_CONVERSATION_EVENT_BATCH, MAX_CONVERSATION_FORK_DELIVERY_ALIASES,
    MAX_CONVERSATION_FORK_DIRECT_CHILDREN, MAX_CONVERSATION_FORK_FAMILY_THREADS,
    MAX_CONVERSATION_FORK_INHERITED_OUTCOMES, MAX_CONVERSATION_FORK_METADATA_BYTES,
    MAX_CONVERSATION_RECOVERY_BATCH, ProviderStartCommit, RegenerateConversationTurn,
    RetryConversationTurn, StartConversationTurn, StartedConversationFork, StartedConversationTurn,
    TerminalTurnCommit, conversation_fork_metadata_estimated_bytes,
    conversation_fork_metadata_is_within_bounds,
};
pub use credential_enrollment::{
    CredentialEnrollment, CredentialEnrollmentError, CredentialEnrollmentRequest,
    CredentialEnrollmentService,
};
pub use credentials::{
    AccountState, CredentialMutationReservation, CredentialMutationStore, CredentialService,
    XaiApiKeyValidation, XaiApiKeyValidationError, XaiApiKeyValidator,
};
pub use effects::{PrepareEffect, SideEffectService};
pub use error::ApplicationError;
pub use grok_build_auth::{GrokBuildAuthService, GrokBuildAuthStatus};
pub use grok_domain::DEFAULT_XAI_CHAT_MODEL_ID;
pub use isolation::{
    IsolationBackend, IsolationBrokerCapabilities, IsolationBrokerOperation,
    IsolationContractVersion, IsolationProbe, IsolationProbeError, IsolationWorkspaceMode,
};
pub use isolation_runtime::{IsolationRuntime, IsolationRuntimeFacts};
pub use managed_integrations::{
    ApplyManagedIntegrationLifecycle, MAX_MANAGED_INTEGRATION_RECOVERY_BATCH,
    ManagedIntegrationLifecycle, ManagedIntegrationLifecycleCommit,
    ManagedIntegrationLifecycleStore, ManagedIntegrationMutation, ManagedIntegrationPhase,
    ManagedIntegrationRecoveryEntry,
};
pub use models::{
    Citation, ContentPart, ConversationEvent, ConversationMessage, ConversationModel,
    ConversationModelFactory, ConversationRequest, ConversationRole, ConversationStream,
    GeneratedAsset, ImageRequest, MediaGenerator, ModelDescriptor, ModelError, ModelErrorKind,
    ModelFailureCertainty, PRODUCT_CHAT_SEARCH_SYSTEM_PROMPT_V3, PRODUCT_CHAT_SYSTEM_PROMPT_V2,
    ServerTool, Usage,
};
pub use ports::{
    ChatModelPreferenceStore, Clock, DatabaseKey, DesktopPreferencesStore,
    ExecutionMutationOutcome, ExecutionStore, HostExecutionPolicyStore, IdGenerator,
    KeyProviderError, MutationCommand, NewRunEvent, SecureKeyProvider, StoreError,
    WorkspaceSearchHit, WorkspaceSearchKind, WorkspaceStore,
};
pub use preferences::{DesktopPreferencesService, UpdateDesktopPreferences};
pub use privileged_gateway::{
    PrivilegedGateway, PrivilegedGatewayError, PrivilegedGatewayResult,
    PrivilegedGuestControlTransport,
};
pub use privileged_operations::{
    BeginPrivilegedDispatch, MAX_PRIVILEGED_ATTEMPT_DURATION_MS,
    MAX_PRIVILEGED_OPERATION_PAYLOAD_BYTES, MAX_PRIVILEGED_RECOVERY_BATCH,
    MIN_PRIVILEGED_OPERATION_PAYLOAD_BYTES, PreparePrivilegedOperation, PrivilegedDispatchAttempt,
    PrivilegedOperationService, PrivilegedOperationStore, PrivilegedPreparation,
    PrivilegedRecoveryCandidate, PrivilegedRecoverySummary,
};
pub use runs::{CreateRun, RunService};
pub use supergrok_oauth::{
    DeviceAuthorization, OAuthCancellation, OAuthFailure, OAuthTokenGrant,
    SUPERGROK_OAUTH_VAULT_NAME, SuperGrokCredential, SuperGrokEnrollmentService,
    SuperGrokEnrollmentStatus, SuperGrokOAuth,
};
pub use usage_summary::{
    GetUsageSummary, UsageScope, UsageSummary, UsageSummaryService, UsageWindow, window_lower_bound,
};
pub use vault::{SecretName, SecretValue, SecretVault, VaultError};
pub use workspace::{
    CreateAutomation, CreateMessage, CreateProject, CreateThread, Page, UpdateAutomation,
    UpdateMessage, UpdateProject, UpdateThread, WorkspaceService,
};
