use grok_domain::{
    ApprovalError, AutomationScheduleError, AutomationSchedulerError, ChatModelPreferenceError,
    ConversationTurnError, DesktopPreferencesError, EffectTransitionError, IdError,
    PrivilegedOperationError, TransitionError, WorkspaceError,
};
use thiserror::Error;

use crate::StoreError;

/// Stable error categories returned by application use cases.
#[derive(Debug, Error)]
pub enum ApplicationError {
    /// Input failed domain validation.
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// The requested entity was not found.
    #[error("entity not found")]
    NotFound,
    /// A concurrent request changed the entity first.
    #[error("revision conflict")]
    Conflict,
    /// The action violates the current lifecycle state.
    #[error("invalid state: {0}")]
    InvalidState(String),
    /// An infrastructure dependency is temporarily unavailable.
    #[error("dependency unavailable: {0}")]
    Unavailable(String),
    /// A trusted local component or platform boundary failed identity validation.
    #[error("integrity failure: {0}")]
    Integrity(String),
    /// A trusted provider rejected the supplied credential.
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    /// An unexpected persistence failure occurred.
    #[error("storage failure: {0}")]
    Storage(String),
    /// A caller-supplied execution deadline elapsed before a safe result existed.
    #[error("deadline exceeded")]
    DeadlineExceeded,
    /// The person cancelled an interactive native operation.
    #[error("operation cancelled")]
    Cancelled,
}

impl From<IdError> for ApplicationError {
    fn from(error: IdError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<TransitionError> for ApplicationError {
    fn from(error: TransitionError) -> Self {
        Self::InvalidState(error.to_string())
    }
}

impl From<ApprovalError> for ApplicationError {
    fn from(error: ApprovalError) -> Self {
        Self::InvalidState(error.to_string())
    }
}

impl From<EffectTransitionError> for ApplicationError {
    fn from(error: EffectTransitionError) -> Self {
        Self::InvalidState(error.to_string())
    }
}

impl From<PrivilegedOperationError> for ApplicationError {
    fn from(error: PrivilegedOperationError) -> Self {
        match error {
            PrivilegedOperationError::TargetKindMismatch { .. }
            | PrivilegedOperationError::InvalidTarget
            | PrivilegedOperationError::SelfSupersession
            | PrivilegedOperationError::InvalidLinks => Self::InvalidInput(error.to_string()),
            PrivilegedOperationError::InvalidPersistedState => Self::Integrity(error.to_string()),
            PrivilegedOperationError::InvalidTransition { .. }
            | PrivilegedOperationError::RetryPolicyViolation { .. }
            | PrivilegedOperationError::ClockRegression { .. }
            | PrivilegedOperationError::AuthorityExpired { .. }
            | PrivilegedOperationError::RevisionExhausted
            | PrivilegedOperationError::AttemptCountExhausted => {
                Self::InvalidState(error.to_string())
            }
        }
    }
}

impl From<WorkspaceError> for ApplicationError {
    fn from(error: WorkspaceError) -> Self {
        match error {
            WorkspaceError::InvalidField { .. } => Self::InvalidInput(error.to_string()),
            WorkspaceError::InvalidLifecycle { .. }
            | WorkspaceError::ClockRegression
            | WorkspaceError::RevisionExhausted => Self::InvalidState(error.to_string()),
        }
    }
}

impl From<AutomationScheduleError> for ApplicationError {
    fn from(error: AutomationScheduleError) -> Self {
        Self::InvalidInput(error.to_string())
    }
}

impl From<AutomationSchedulerError> for ApplicationError {
    fn from(error: AutomationSchedulerError) -> Self {
        match error {
            AutomationSchedulerError::Schedule(error) => Self::InvalidInput(error.to_string()),
            AutomationSchedulerError::InvalidExecutionText
            | AutomationSchedulerError::InvalidDecision
            | AutomationSchedulerError::InvalidLease => Self::InvalidInput(error.to_string()),
            AutomationSchedulerError::InvalidPersistedState => Self::Integrity(error.to_string()),
            AutomationSchedulerError::ClockRegression
            | AutomationSchedulerError::InvalidOccurrenceTransition { .. }
            | AutomationSchedulerError::RevisionExhausted
            | AutomationSchedulerError::StaleFence
            | AutomationSchedulerError::LeaseExpired
            | AutomationSchedulerError::LeaseStillHeld
            | AutomationSchedulerError::ClaimAttemptsExhausted => {
                Self::InvalidState(error.to_string())
            }
        }
    }
}

impl From<ConversationTurnError> for ApplicationError {
    fn from(error: ConversationTurnError) -> Self {
        match error {
            ConversationTurnError::InvalidField(_) => Self::InvalidInput(error.to_string()),
            ConversationTurnError::InvalidTransition { .. }
            | ConversationTurnError::ClockRegression
            | ConversationTurnError::RevisionExhausted => Self::InvalidState(error.to_string()),
        }
    }
}

impl From<DesktopPreferencesError> for ApplicationError {
    fn from(error: DesktopPreferencesError) -> Self {
        Self::InvalidState(error.to_string())
    }
}

impl From<ChatModelPreferenceError> for ApplicationError {
    fn from(error: ChatModelPreferenceError) -> Self {
        match error {
            ChatModelPreferenceError::InvalidModelId => Self::InvalidInput(error.to_string()),
            ChatModelPreferenceError::ClockRegression
            | ChatModelPreferenceError::RevisionExhausted => Self::InvalidState(error.to_string()),
        }
    }
}

impl From<StoreError> for ApplicationError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::NotFound => Self::NotFound,
            StoreError::Conflict => Self::Conflict,
            StoreError::Unavailable(message) => Self::Unavailable(message),
            StoreError::Internal(message) => Self::Storage(message),
        }
    }
}
