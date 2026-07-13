//! Pure product rules for Grok Desktop.

mod approval;
mod automation;
mod capability;
mod conversation;
mod effect;
mod host_execution;
mod id;
mod preferences;
mod privileged_operation;
mod run;
mod workspace;

pub use approval::{
    Approval, ApprovalDecision, ApprovalError, ApprovalRisk, ApprovalScope, ApprovalStatus,
    RequestedAction,
};
pub use automation::{
    AUTOMATION_SCHEDULE_CALCULATOR_VERSION, AutomationCadence, AutomationExecutionSnapshot,
    AutomationLocalDateTime, AutomationOccurrence, AutomationOccurrenceClaim,
    AutomationOccurrenceSlot, AutomationOccurrenceState, AutomationSchedule,
    AutomationScheduleCursor, AutomationScheduleDecision, AutomationScheduleError,
    AutomationScheduleEvaluation, AutomationScheduleFingerprint, AutomationSchedulerError,
    AutomationSchedulerLease, AutomationSchedulerLeaseToken,
    MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS, MAX_AUTOMATION_SCHEDULE_BYTES,
    MAX_AUTOMATION_SCHEDULE_DECISIONS, MAX_AUTOMATION_SCHEDULE_WINDOW_DAYS,
    MAX_AUTOMATION_SCHEDULER_LEASE_MS,
};
pub use capability::{
    AuthMethod, Capability, CapabilityAvailability, CapabilityRequirement, CapabilityStatus,
    CapabilitySurface,
};
pub use conversation::{
    ChatRail, ConversationCitation, ConversationFailure, ConversationFailureKind, ConversationTurn,
    ConversationTurnError, ConversationTurnEvent, ConversationTurnEventError,
    ConversationTurnEventKind, ConversationTurnEventLog, ConversationTurnLineage,
    ConversationTurnOrigin, ConversationTurnState, ConversationUsage,
    MAX_CONVERSATION_CITATION_TOTAL_BYTES, MAX_CONVERSATION_TEXT_CHUNK_BYTES,
    MAX_CONVERSATION_TEXT_EVENTS, MAX_CONVERSATION_USAGE_VALUE,
};
pub use effect::{EffectKind, EffectState, EffectTransitionError, Idempotency, SideEffect};
pub use host_execution::{
    HOST_ACKNOWLEDGMENT_PHRASE, HOST_ACKNOWLEDGMENT_VERSION, HostExecutionPolicy,
    HostExecutionPolicyError, HostToolClasses, MAX_HOST_EXECUTION_ROOTS,
};
pub use id::{
    ApprovalId, ArtifactId, AutomationId, AutomationOccurrenceId, AutomationSchedulerOwnerId,
    ConversationTurnId, EffectId, IdError, MessageId, PrivilegedOperationId, ProjectId, RunId,
    ThreadId,
};
pub use preferences::{
    ChatModelPreference, ChatModelPreferenceError, DEFAULT_XAI_CHAT_MODEL_ID, DesktopPreferences,
    DesktopPreferencesError,
};
pub use privileged_operation::{
    AuthorityGrantId, PayloadDigest, PrivilegedAuthority, PrivilegedIdempotency,
    PrivilegedIdempotencyKey, PrivilegedOperation, PrivilegedOperationError,
    PrivilegedOperationIntent, PrivilegedOperationKind, PrivilegedOperationLinks,
    PrivilegedOperationReview, PrivilegedOperationState, PrivilegedOperationTarget,
    PrivilegedOperationValueError, PrivilegedResourceId, PrivilegedRetryClass, RequestDigest,
};
pub use run::{
    Run, RunEvent, RunEventKind, RunKind, RunState, TransitionError, WorkExecutionBackend,
};
pub use workspace::{
    Artifact, ArtifactContentSummary, ArtifactState, ArtifactVersion, Automation,
    AutomationHistoryEntry, AutomationHistoryStatus, AutomationState, ConversationForkKind,
    ConversationMessageDerivation, ConversationMessageDerivationKind, ConversationThreadLineage,
    ConversationThreadOrigin, MAX_ARTIFACT_CONTENT_VERSION, MAX_MESSAGE_BYTES, Message,
    MessageRole, MessageState, MissedRunPolicy, OverlapPolicy, Project, ProjectState, Thread,
    ThreadState, WorkspaceError, validate_imported_file_name,
};

/// Milliseconds since the Unix epoch.
pub type UnixMillis = u64;
