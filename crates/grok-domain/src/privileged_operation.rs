use std::fmt::{self, Display};

use thiserror::Error;

use crate::{ApprovalId, EffectId, PrivilegedOperationId, RunId, UnixMillis};

const MIN_KEY_BYTES: usize = 16;
const MAX_KEY_BYTES: usize = 128;

/// A stable identifier for the exact authority grant evaluated for an operation.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AuthorityGrantId(String);

impl AuthorityGrantId {
    /// Creates a grant identifier suitable for durable storage and local protocols.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationValueError`] unless the identifier is between
    /// 16 and 128 bytes and contains only the allowlisted ASCII characters.
    pub fn new(value: impl Into<String>) -> Result<Self, PrivilegedOperationValueError> {
        let value = value.into();
        validate_safe_ascii(&value, "authority_grant_id", MIN_KEY_BYTES)?;
        Ok(Self(value))
    }

    /// Returns the validated external representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the identifier into its validated external representation.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Display for AuthorityGrantId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A caller-supplied deduplication key scoped to privileged operations.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrivilegedIdempotencyKey(String);

impl PrivilegedIdempotencyKey {
    /// Creates a bounded key accepted by the privileged service protocol.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationValueError`] unless the key is between 16
    /// and 128 bytes and contains only the allowlisted ASCII characters.
    pub fn new(value: impl Into<String>) -> Result<Self, PrivilegedOperationValueError> {
        let value = value.into();
        validate_safe_ascii(&value, "idempotency_key", MIN_KEY_BYTES)?;
        Ok(Self(value))
    }

    /// Returns the validated external representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the key into its validated external representation.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Display for PrivilegedIdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A bounded non-secret resource identifier retained for review and audit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PrivilegedResourceId(String);

impl PrivilegedResourceId {
    /// Creates a resource identifier without relying on payload parsing.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationValueError`] unless the identifier is between
    /// 1 and 128 bytes and contains only the allowlisted ASCII characters.
    pub fn new(value: impl Into<String>) -> Result<Self, PrivilegedOperationValueError> {
        let value = value.into();
        validate_safe_ascii(&value, "resource_id", 1)?;
        Ok(Self(value))
    }

    /// Returns the validated external representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Consumes the identifier into its validated external representation.
    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl Display for PrivilegedResourceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

macro_rules! digest_type {
    ($name:ident, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            /// Wraps an already-computed SHA-256 digest.
            #[must_use]
            pub const fn new(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            /// Borrows the fixed-width digest for storage or protocol binding.
            #[must_use]
            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            /// Copies the fixed-width digest.
            #[must_use]
            pub const fn to_bytes(self) -> [u8; 32] {
                self.0
            }
        }
    };
}

digest_type!(
    RequestDigest,
    "SHA-256 of the canonical semantic privileged-operation request."
);
digest_type!(
    PayloadDigest,
    "SHA-256 of the exact bounded payload persisted for dispatch."
);

/// Invalid bounded authority or idempotency metadata.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PrivilegedOperationValueError {
    /// Safe values enforce a field-specific minimum length.
    #[error("{field} must contain at least {minimum} bytes")]
    TooShort {
        /// Invalid field.
        field: &'static str,
        /// Inclusive lower bound.
        minimum: usize,
    },
    /// Keys are bounded at every process and storage boundary.
    #[error("{field} exceeds {maximum} bytes")]
    TooLong {
        /// Invalid field.
        field: &'static str,
        /// Inclusive upper bound.
        maximum: usize,
    },
    /// Keys use the same conservative alphabet as the privileged protocol.
    #[error("{field} contains a character outside [A-Za-z0-9._:-]")]
    InvalidCharacter {
        /// Invalid field.
        field: &'static str,
    },
}

fn validate_safe_ascii(
    value: &str,
    field: &'static str,
    minimum: usize,
) -> Result<(), PrivilegedOperationValueError> {
    if value.len() < minimum {
        return Err(PrivilegedOperationValueError::TooShort { field, minimum });
    }
    if value.len() > MAX_KEY_BYTES {
        return Err(PrivilegedOperationValueError::TooLong {
            field,
            maximum: MAX_KEY_BYTES,
        });
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(PrivilegedOperationValueError::InvalidCharacter { field });
    }
    Ok(())
}

/// Closed privileged operations supported by the durable journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrivilegedOperationKind {
    /// Read-only guest runner readiness query.
    RunnerHealth,
    /// Apply a declared tool catalog inside the isolated guest.
    CatalogApply,
    /// Start a managed integration process.
    IntegrationStart,
    /// Stop a managed integration process.
    IntegrationStop,
    /// Read a versioned computer-use observation.
    ComputerObserve,
    /// Perform an input action against a versioned observation.
    ComputerAct,
}

impl PrivilegedOperationKind {
    /// Returns the fixed retry policy for this operation kind.
    ///
    /// Catalog application remains non-idempotent until the complete guest and
    /// broker path has a qualified convergent replay contract.
    #[must_use]
    pub const fn retry_class(self) -> PrivilegedRetryClass {
        match self {
            Self::RunnerHealth | Self::ComputerObserve => PrivilegedRetryClass::RetrySafe,
            Self::CatalogApply
            | Self::IntegrationStart
            | Self::IntegrationStop
            | Self::ComputerAct => PrivilegedRetryClass::NonIdempotent,
        }
    }
}

/// Non-secret resource identity needed to review a privileged operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrivilegedOperationTarget {
    /// Isolated runner selected for a health query.
    Runner {
        /// Stable utility VM identifier.
        vm_id: PrivilegedResourceId,
    },
    /// Isolated runner receiving a declared catalog.
    Catalog {
        /// Stable utility VM identifier.
        vm_id: PrivilegedResourceId,
    },
    /// Integration selected for a start request.
    IntegrationStart {
        /// Stable utility VM identifier.
        vm_id: PrivilegedResourceId,
        /// Signed integration identifier.
        integration_id: PrivilegedResourceId,
    },
    /// Exact running integration instance selected for a stop request.
    IntegrationStop {
        /// Stable utility VM identifier.
        vm_id: PrivilegedResourceId,
        /// Signed integration identifier.
        integration_id: PrivilegedResourceId,
        /// Runtime instance identifier.
        instance_id: PrivilegedResourceId,
    },
    /// Integration selected for a versioned computer-use observation.
    ComputerObserve {
        /// Stable utility VM identifier.
        vm_id: PrivilegedResourceId,
        /// Signed integration identifier.
        integration_id: PrivilegedResourceId,
    },
    /// Exact application and observation selected for a computer-use action.
    ComputerAct {
        /// Stable utility VM identifier.
        vm_id: PrivilegedResourceId,
        /// Signed integration identifier.
        integration_id: PrivilegedResourceId,
        /// Runtime instance identifier.
        instance_id: PrivilegedResourceId,
        /// Stable application identifier from the observation.
        application_id: PrivilegedResourceId,
        /// Positive observation revision against which the action was approved.
        observation_revision: u64,
    },
}

impl PrivilegedOperationTarget {
    /// Returns the only operation kind compatible with this target shape.
    #[must_use]
    pub const fn operation_kind(&self) -> PrivilegedOperationKind {
        match self {
            Self::Runner { .. } => PrivilegedOperationKind::RunnerHealth,
            Self::Catalog { .. } => PrivilegedOperationKind::CatalogApply,
            Self::IntegrationStart { .. } => PrivilegedOperationKind::IntegrationStart,
            Self::IntegrationStop { .. } => PrivilegedOperationKind::IntegrationStop,
            Self::ComputerObserve { .. } => PrivilegedOperationKind::ComputerObserve,
            Self::ComputerAct { .. } => PrivilegedOperationKind::ComputerAct,
        }
    }

    const fn is_valid(&self) -> bool {
        !matches!(
            self,
            Self::ComputerAct {
                observation_revision: 0,
                ..
            }
        )
    }
}

/// Whether an interrupted operation may be dispatched again under the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrivilegedRetryClass {
    /// Repeating the operation cannot create an additional external side effect.
    RetrySafe,
    /// Repeating the operation may duplicate or contradict a visible side effect.
    NonIdempotent,
}

/// Exact authority snapshot bound to the operation request digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegedAuthority {
    /// Stable grant evaluated by the daemon policy layer.
    pub grant_id: AuthorityGrantId,
    /// Last millisecond in which a new dispatch may begin.
    pub expires_at: UnixMillis,
}

impl PrivilegedAuthority {
    /// Creates immutable authority metadata for an operation.
    #[must_use]
    pub const fn new(grant_id: AuthorityGrantId, expires_at: UnixMillis) -> Self {
        Self {
            grant_id,
            expires_at,
        }
    }
}

/// Deduplication metadata bound to the canonical semantic request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegedIdempotency {
    /// Key used to find an existing journal record.
    pub key: PrivilegedIdempotencyKey,
    /// Digest used to reject reuse of the key for different semantics.
    pub request_digest: RequestDigest,
}

impl PrivilegedIdempotency {
    /// Creates immutable idempotency metadata for an operation.
    #[must_use]
    pub const fn new(key: PrivilegedIdempotencyKey, request_digest: RequestDigest) -> Self {
        Self {
            key,
            request_digest,
        }
    }
}

/// Optional durable links into the owning execution and approval records.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrivilegedOperationLinks {
    /// Run that requested the operation, when it belongs to a Work run.
    pub run_id: Option<RunId>,
    /// General side-effect record associated with this privileged boundary.
    pub effect_id: Option<EffectId>,
    /// Exact approval authorizing the operation, when one is required.
    pub approval_id: Option<ApprovalId>,
    /// Earlier reviewed operation replaced by this explicit new request.
    pub supersedes_id: Option<PrivilegedOperationId>,
}

/// Immutable semantic intent committed before a privileged operation dispatches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegedOperationIntent {
    /// Closed semantic operation kind.
    pub kind: PrivilegedOperationKind,
    /// Bounded non-secret target retained for review and audit.
    pub target: PrivilegedOperationTarget,
    /// SHA-256 of the exact bounded payload retained by the journal.
    pub payload_digest: PayloadDigest,
    /// Exact authority evaluated before preparation.
    pub authority: PrivilegedAuthority,
    /// Caller key and canonical request digest used for durable replay checks.
    pub idempotency: PrivilegedIdempotency,
    /// Optional links to related durable records.
    pub links: PrivilegedOperationLinks,
}

impl PrivilegedOperationIntent {
    /// Groups the immutable values validated by [`PrivilegedOperation::prepare`].
    #[must_use]
    pub const fn new(
        kind: PrivilegedOperationKind,
        target: PrivilegedOperationTarget,
        payload_digest: PayloadDigest,
        authority: PrivilegedAuthority,
        idempotency: PrivilegedIdempotency,
        links: PrivilegedOperationLinks,
    ) -> Self {
        Self {
            kind,
            target,
            payload_digest,
            authority,
            idempotency,
            links,
        }
    }
}

/// Durable lifecycle of a privileged operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrivilegedOperationState {
    /// Intent and immutable payload are durable but undispatched.
    Prepared,
    /// An attempt has crossed or may have crossed the privileged boundary.
    Dispatching,
    /// A retry-safe attempt may be explicitly dispatched again.
    RetryPending,
    /// A known successful result is durable.
    Succeeded,
    /// A known terminal failure is durable.
    Failed,
    /// A non-idempotent result is uncertain and requires human review.
    InterruptedNeedsReview,
    /// The uncertain result received an explicit review disposition.
    Reviewed,
    /// The intent was cancelled before any dispatch.
    Cancelled,
}

impl PrivilegedOperationState {
    /// Returns whether this state accepts no further transition.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::Reviewed | Self::Cancelled
        )
    }

    const fn permits(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Prepared, Self::Dispatching | Self::Cancelled)
                | (
                    Self::Dispatching,
                    Self::RetryPending
                        | Self::Succeeded
                        | Self::Failed
                        | Self::InterruptedNeedsReview
                )
                | (Self::RetryPending, Self::Dispatching)
                | (Self::InterruptedNeedsReview, Self::Reviewed)
        )
    }
}

/// Human disposition for an uncertain non-idempotent result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PrivilegedOperationReview {
    /// Review established that the original action completed.
    ConfirmedSucceeded,
    /// Review established that the original action did not complete.
    ConfirmedFailed,
    /// The result remains unknown and the user chose not to take further action.
    Abandoned,
}

/// Invalid privileged-operation construction or lifecycle operation.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PrivilegedOperationError {
    /// The requested edge is absent from the closed state machine.
    #[error("invalid privileged operation transition from {from:?} to {to:?}")]
    InvalidTransition {
        /// Existing lifecycle state.
        from: PrivilegedOperationState,
        /// Requested lifecycle state.
        to: PrivilegedOperationState,
    },
    /// A retry was requested for an operation that may duplicate a side effect.
    #[error("privileged operation {kind:?} is not retry-safe")]
    RetryPolicyViolation {
        /// Operation whose fixed policy rejects retry.
        kind: PrivilegedOperationKind,
    },
    /// State transitions cannot move the journal clock backwards.
    #[error("transition timestamp {attempted} predates last update {current}")]
    ClockRegression {
        /// Current update timestamp.
        current: UnixMillis,
        /// Attempted update timestamp.
        attempted: UnixMillis,
    },
    /// No new dispatch may begin after the bound authority expires.
    #[error("authority grant expired at {expires_at}")]
    AuthorityExpired {
        /// Inclusive last valid dispatch timestamp.
        expires_at: UnixMillis,
    },
    /// A replacement operation cannot link to itself.
    #[error("privileged operation cannot supersede itself")]
    SelfSupersession,
    /// Run-owned effects and approvals require their owning run link.
    #[error("effect_id and approval_id require run_id")]
    InvalidLinks,
    /// The target shape must match the closed semantic operation kind.
    #[error("target for {target_kind:?} cannot be used with {operation_kind:?}")]
    TargetKindMismatch {
        /// Requested operation kind.
        operation_kind: PrivilegedOperationKind,
        /// Operation kind implied by the target shape.
        target_kind: PrivilegedOperationKind,
    },
    /// Computer actions must bind a positive observation revision.
    #[error("computer action target requires a positive observation revision")]
    InvalidTarget,
    /// The optimistic revision cannot advance further.
    #[error("privileged operation revision is exhausted")]
    RevisionExhausted,
    /// The bounded dispatch attempt counter cannot advance further.
    #[error("privileged operation attempt count is exhausted")]
    AttemptCountExhausted,
    /// Durable fields do not describe a state reachable through this aggregate.
    #[error("persisted privileged operation is internally inconsistent")]
    InvalidPersistedState,
}

/// Durable source of truth for one bounded privileged operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegedOperation {
    /// Stable journal identifier.
    pub id: PrivilegedOperationId,
    /// Closed semantic operation kind.
    pub kind: PrivilegedOperationKind,
    /// Bounded non-secret target retained for review and audit.
    pub target: PrivilegedOperationTarget,
    /// SHA-256 of the exact bounded payload retained by the journal.
    pub payload_digest: PayloadDigest,
    /// Exact authority evaluated before preparation.
    pub authority: PrivilegedAuthority,
    /// Caller key and canonical request digest used for durable replay checks.
    pub idempotency: PrivilegedIdempotency,
    /// Optional links to related durable records.
    pub links: PrivilegedOperationLinks,
    /// Current lifecycle state.
    pub state: PrivilegedOperationState,
    /// Human disposition after an uncertain non-idempotent attempt.
    pub review: Option<PrivilegedOperationReview>,
    /// Number of attempts whose dispatch was durably recorded.
    pub attempt_count: u32,
    /// Optimistic concurrency revision.
    pub revision: u64,
    /// Preparation timestamp.
    pub created_at: UnixMillis,
    /// Last successful lifecycle transition timestamp.
    pub updated_at: UnixMillis,
}

impl PrivilegedOperation {
    /// Persists immutable intent before any privileged dispatch.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationError`] if authority is already expired,
    /// the target does not match the operation kind, the operation claims to
    /// replace itself, or run-owned links lack a run.
    pub fn prepare(
        id: PrivilegedOperationId,
        intent: PrivilegedOperationIntent,
        now: UnixMillis,
    ) -> Result<Self, PrivilegedOperationError> {
        let PrivilegedOperationIntent {
            kind,
            target,
            payload_digest,
            authority,
            idempotency,
            links,
        } = intent;
        if authority.expires_at < now {
            return Err(PrivilegedOperationError::AuthorityExpired {
                expires_at: authority.expires_at,
            });
        }
        let target_kind = target.operation_kind();
        if target_kind != kind {
            return Err(PrivilegedOperationError::TargetKindMismatch {
                operation_kind: kind,
                target_kind,
            });
        }
        if !target.is_valid() {
            return Err(PrivilegedOperationError::InvalidTarget);
        }
        if links.supersedes_id.as_ref() == Some(&id) {
            return Err(PrivilegedOperationError::SelfSupersession);
        }
        if links.run_id.is_none() && (links.effect_id.is_some() || links.approval_id.is_some()) {
            return Err(PrivilegedOperationError::InvalidLinks);
        }
        Ok(Self {
            id,
            kind,
            target,
            payload_digest,
            authority,
            idempotency,
            links,
            state: PrivilegedOperationState::Prepared,
            review: None,
            attempt_count: 0,
            revision: 0,
            created_at: now,
            updated_at: now,
        })
    }

    /// Rehydrates a durable snapshot after checking every aggregate invariant.
    ///
    /// Adapters must use this constructor instead of trusting database fields.
    /// In particular, retry classification, lifecycle revision, attempt count,
    /// review disposition, target shape, links, and timestamps are checked as a
    /// single unit.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationError::InvalidPersistedState`] when the
    /// snapshot could not have been produced by the closed state machine.
    pub fn restore(snapshot: Self) -> Result<Self, PrivilegedOperationError> {
        if snapshot.validate_snapshot() {
            Ok(snapshot)
        } else {
            Err(PrivilegedOperationError::InvalidPersistedState)
        }
    }

    /// Returns the fixed retry policy for this operation.
    #[must_use]
    pub const fn retry_class(&self) -> PrivilegedRetryClass {
        self.kind.retry_class()
    }

    /// Records that a bounded attempt is about to cross the privileged boundary.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationError`] unless the operation is prepared or
    /// retry-pending, the timestamp and authority are valid, and counters can advance.
    pub fn dispatch(&mut self, now: UnixMillis) -> Result<(), PrivilegedOperationError> {
        self.validate_edge(PrivilegedOperationState::Dispatching, now)?;
        if now > self.authority.expires_at {
            return Err(PrivilegedOperationError::AuthorityExpired {
                expires_at: self.authority.expires_at,
            });
        }
        let next_attempt = self
            .attempt_count
            .checked_add(1)
            .ok_or(PrivilegedOperationError::AttemptCountExhausted)?;
        self.apply_transition(PrivilegedOperationState::Dispatching, now)?;
        self.attempt_count = next_attempt;
        Ok(())
    }

    /// Records a known successful external result.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationError`] unless an attempt is dispatching.
    pub fn succeed(&mut self, now: UnixMillis) -> Result<(), PrivilegedOperationError> {
        self.move_to(PrivilegedOperationState::Succeeded, now)
    }

    /// Records a known terminal external failure.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationError`] unless an attempt is dispatching.
    pub fn fail(&mut self, now: UnixMillis) -> Result<(), PrivilegedOperationError> {
        self.move_to(PrivilegedOperationState::Failed, now)
    }

    /// Records a known retryable result for a retry-safe operation.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationError`] unless an attempt is dispatching and
    /// the operation kind has a fixed retry-safe policy.
    pub fn schedule_retry(&mut self, now: UnixMillis) -> Result<(), PrivilegedOperationError> {
        self.validate_edge(PrivilegedOperationState::RetryPending, now)?;
        if self.retry_class() != PrivilegedRetryClass::RetrySafe {
            return Err(PrivilegedOperationError::RetryPolicyViolation { kind: self.kind });
        }
        self.apply_transition(PrivilegedOperationState::RetryPending, now)
    }

    /// Recovers a dispatch whose external outcome was not durably recorded.
    ///
    /// Retry-safe reads become retry-pending. Every non-idempotent operation,
    /// including catalog application, becomes interrupted-needs-review.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationError`] unless an attempt is dispatching.
    pub fn interrupt(&mut self, now: UnixMillis) -> Result<(), PrivilegedOperationError> {
        match self.retry_class() {
            PrivilegedRetryClass::RetrySafe => self.schedule_retry(now),
            PrivilegedRetryClass::NonIdempotent => {
                self.move_to(PrivilegedOperationState::InterruptedNeedsReview, now)
            }
        }
    }

    /// Records a human disposition for an uncertain non-idempotent result.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationError`] unless the operation requires review.
    pub fn review(
        &mut self,
        disposition: PrivilegedOperationReview,
        now: UnixMillis,
    ) -> Result<(), PrivilegedOperationError> {
        self.move_to(PrivilegedOperationState::Reviewed, now)?;
        self.review = Some(disposition);
        Ok(())
    }

    /// Cancels an intent that has never been dispatched.
    ///
    /// # Errors
    ///
    /// Returns [`PrivilegedOperationError`] unless the operation is prepared.
    pub fn cancel(&mut self, now: UnixMillis) -> Result<(), PrivilegedOperationError> {
        self.move_to(PrivilegedOperationState::Cancelled, now)
    }

    fn move_to(
        &mut self,
        next: PrivilegedOperationState,
        now: UnixMillis,
    ) -> Result<(), PrivilegedOperationError> {
        self.validate_edge(next, now)?;
        self.apply_transition(next, now)
    }

    fn validate_edge(
        &self,
        next: PrivilegedOperationState,
        now: UnixMillis,
    ) -> Result<(), PrivilegedOperationError> {
        if !self.state.permits(next) {
            return Err(PrivilegedOperationError::InvalidTransition {
                from: self.state,
                to: next,
            });
        }
        if now < self.updated_at {
            return Err(PrivilegedOperationError::ClockRegression {
                current: self.updated_at,
                attempted: now,
            });
        }
        if self.revision == u64::MAX {
            return Err(PrivilegedOperationError::RevisionExhausted);
        }
        Ok(())
    }

    fn apply_transition(
        &mut self,
        next: PrivilegedOperationState,
        now: UnixMillis,
    ) -> Result<(), PrivilegedOperationError> {
        self.revision = self
            .revision
            .checked_add(1)
            .ok_or(PrivilegedOperationError::RevisionExhausted)?;
        self.state = next;
        self.updated_at = now;
        Ok(())
    }

    fn validate_snapshot(&self) -> bool {
        if self.target.operation_kind() != self.kind
            || !self.target.is_valid()
            || self.links.supersedes_id.as_ref() == Some(&self.id)
            || (self.links.run_id.is_none()
                && (self.links.effect_id.is_some() || self.links.approval_id.is_some()))
            || self.authority.expires_at < self.created_at
            || self.updated_at < self.created_at
            || (self.state == PrivilegedOperationState::Dispatching
                && self.updated_at > self.authority.expires_at)
        {
            return false;
        }

        let expected_revision = match self.state {
            PrivilegedOperationState::Prepared => {
                if self.attempt_count != 0 || self.updated_at != self.created_at {
                    return false;
                }
                Some(0)
            }
            PrivilegedOperationState::Cancelled => (self.attempt_count == 0).then_some(1),
            PrivilegedOperationState::Dispatching => self
                .attempt_count
                .checked_mul(2)
                .and_then(|value| value.checked_sub(1))
                .map(u64::from),
            PrivilegedOperationState::RetryPending => {
                if self.retry_class() != PrivilegedRetryClass::RetrySafe || self.attempt_count == 0
                {
                    return false;
                }
                self.attempt_count.checked_mul(2).map(u64::from)
            }
            PrivilegedOperationState::Succeeded | PrivilegedOperationState::Failed => {
                if self.attempt_count == 0 {
                    return false;
                }
                self.attempt_count.checked_mul(2).map(u64::from)
            }
            PrivilegedOperationState::InterruptedNeedsReview => (self.retry_class()
                == PrivilegedRetryClass::NonIdempotent
                && self.attempt_count == 1)
                .then_some(2),
            PrivilegedOperationState::Reviewed => (self.retry_class()
                == PrivilegedRetryClass::NonIdempotent
                && self.attempt_count == 1)
                .then_some(3),
        };

        let review_is_valid = matches!(
            (self.state, self.review),
            (PrivilegedOperationState::Reviewed, Some(_))
                | (
                    PrivilegedOperationState::Prepared
                        | PrivilegedOperationState::Dispatching
                        | PrivilegedOperationState::RetryPending
                        | PrivilegedOperationState::Succeeded
                        | PrivilegedOperationState::Failed
                        | PrivilegedOperationState::InterruptedNeedsReview
                        | PrivilegedOperationState::Cancelled,
                    None
                )
        );
        expected_revision == Some(self.revision) && review_is_valid
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CREATED_AT: UnixMillis = 100;
    const EXPIRES_AT: UnixMillis = 1_000;

    fn resource(value: &str) -> PrivilegedResourceId {
        PrivilegedResourceId::new(value).expect("resource id")
    }

    fn target(kind: PrivilegedOperationKind) -> PrivilegedOperationTarget {
        match kind {
            PrivilegedOperationKind::RunnerHealth => PrivilegedOperationTarget::Runner {
                vm_id: resource("work-vm"),
            },
            PrivilegedOperationKind::CatalogApply => PrivilegedOperationTarget::Catalog {
                vm_id: resource("work-vm"),
            },
            PrivilegedOperationKind::IntegrationStart => {
                PrivilegedOperationTarget::IntegrationStart {
                    vm_id: resource("work-vm"),
                    integration_id: resource("wisp"),
                }
            }
            PrivilegedOperationKind::IntegrationStop => {
                PrivilegedOperationTarget::IntegrationStop {
                    vm_id: resource("work-vm"),
                    integration_id: resource("wisp"),
                    instance_id: resource("instance-1"),
                }
            }
            PrivilegedOperationKind::ComputerObserve => {
                PrivilegedOperationTarget::ComputerObserve {
                    vm_id: resource("work-vm"),
                    integration_id: resource("wisp"),
                }
            }
            PrivilegedOperationKind::ComputerAct => PrivilegedOperationTarget::ComputerAct {
                vm_id: resource("work-vm"),
                integration_id: resource("wisp"),
                instance_id: resource("instance-1"),
                application_id: resource("application-1"),
                observation_revision: 1,
            },
        }
    }

    fn intent_with_target(
        kind: PrivilegedOperationKind,
        target: PrivilegedOperationTarget,
        links: PrivilegedOperationLinks,
    ) -> PrivilegedOperationIntent {
        PrivilegedOperationIntent::new(
            kind,
            target,
            PayloadDigest::new([2; 32]),
            PrivilegedAuthority::new(
                AuthorityGrantId::new("authority-grant-0001").expect("grant id"),
                EXPIRES_AT,
            ),
            PrivilegedIdempotency::new(
                PrivilegedIdempotencyKey::new("idempotency-key-0001").expect("key"),
                RequestDigest::new([1; 32]),
            ),
            links,
        )
    }

    fn intent(kind: PrivilegedOperationKind) -> PrivilegedOperationIntent {
        intent_with_target(kind, target(kind), PrivilegedOperationLinks::default())
    }

    fn operation(kind: PrivilegedOperationKind) -> PrivilegedOperation {
        PrivilegedOperation::prepare(
            PrivilegedOperationId::new("privileged-operation-0001").expect("operation id"),
            intent(kind),
            CREATED_AT,
        )
        .expect("operation")
    }

    #[test]
    fn safe_keys_use_the_privileged_protocol_alphabet_and_bounds() {
        assert!(matches!(
            PrivilegedIdempotencyKey::new("a".repeat(15)),
            Err(PrivilegedOperationValueError::TooShort { .. })
        ));
        assert!(matches!(
            PrivilegedIdempotencyKey::new("a".repeat(129)),
            Err(PrivilegedOperationValueError::TooLong { .. })
        ));
        for invalid in [
            "idempotency/key-0001",
            "idempotency key-0001",
            "idempotency-key-\u{00e9}",
            "idempotency-key-\n",
        ] {
            assert!(matches!(
                PrivilegedIdempotencyKey::new(invalid),
                Err(PrivilegedOperationValueError::InvalidCharacter { .. })
            ));
        }

        let valid = "Abcdef012345._:-";
        assert_eq!(
            AuthorityGrantId::new(valid).expect("grant id").as_str(),
            valid
        );
        assert_eq!(
            PrivilegedIdempotencyKey::new(valid)
                .expect("idempotency key")
                .as_str(),
            valid
        );
        assert!(matches!(
            PrivilegedResourceId::new(""),
            Err(PrivilegedOperationValueError::TooShort { minimum: 1, .. })
        ));
        assert!(matches!(
            PrivilegedResourceId::new("unsafe/resource"),
            Err(PrivilegedOperationValueError::InvalidCharacter { .. })
        ));
        assert_eq!(
            PrivilegedResourceId::new("work-vm")
                .expect("resource id")
                .as_str(),
            "work-vm"
        );
    }

    #[test]
    fn digest_types_offer_fixed_width_borrow_and_copy() {
        let request = RequestDigest::new([7; 32]);
        let payload = PayloadDigest::new([9; 32]);
        assert_eq!(request.as_bytes(), &[7; 32]);
        assert_eq!(request.to_bytes(), [7; 32]);
        assert_eq!(payload.as_bytes(), &[9; 32]);
        assert_eq!(payload.to_bytes(), [9; 32]);
    }

    #[test]
    fn retry_classification_is_closed_and_catalog_apply_is_conservative() {
        let cases = [
            (
                PrivilegedOperationKind::RunnerHealth,
                PrivilegedRetryClass::RetrySafe,
            ),
            (
                PrivilegedOperationKind::CatalogApply,
                PrivilegedRetryClass::NonIdempotent,
            ),
            (
                PrivilegedOperationKind::IntegrationStart,
                PrivilegedRetryClass::NonIdempotent,
            ),
            (
                PrivilegedOperationKind::IntegrationStop,
                PrivilegedRetryClass::NonIdempotent,
            ),
            (
                PrivilegedOperationKind::ComputerObserve,
                PrivilegedRetryClass::RetrySafe,
            ),
            (
                PrivilegedOperationKind::ComputerAct,
                PrivilegedRetryClass::NonIdempotent,
            ),
        ];
        for (kind, expected) in cases {
            assert_eq!(kind.retry_class(), expected);
        }
    }

    #[test]
    fn target_shapes_map_exhaustively_to_their_closed_operation_kinds() {
        for kind in [
            PrivilegedOperationKind::RunnerHealth,
            PrivilegedOperationKind::CatalogApply,
            PrivilegedOperationKind::IntegrationStart,
            PrivilegedOperationKind::IntegrationStop,
            PrivilegedOperationKind::ComputerObserve,
            PrivilegedOperationKind::ComputerAct,
        ] {
            assert_eq!(target(kind).operation_kind(), kind);
        }
    }

    #[test]
    fn prepare_retains_immutable_metadata_and_typed_links() {
        let id = PrivilegedOperationId::new("privileged-operation-0002").expect("operation id");
        let links = PrivilegedOperationLinks {
            run_id: Some(RunId::new("run-1").expect("run id")),
            effect_id: Some(EffectId::new("effect-1").expect("effect id")),
            approval_id: Some(ApprovalId::new("approval-1").expect("approval id")),
            supersedes_id: Some(
                PrivilegedOperationId::new("privileged-operation-0001").expect("superseded id"),
            ),
        };
        let target = target(PrivilegedOperationKind::ComputerAct);
        let operation = PrivilegedOperation::prepare(
            id.clone(),
            PrivilegedOperationIntent::new(
                PrivilegedOperationKind::ComputerAct,
                target.clone(),
                PayloadDigest::new([4; 32]),
                PrivilegedAuthority::new(
                    AuthorityGrantId::new("authority-grant-0002").expect("grant id"),
                    EXPIRES_AT,
                ),
                PrivilegedIdempotency::new(
                    PrivilegedIdempotencyKey::new("idempotency-key-0002").expect("key"),
                    RequestDigest::new([3; 32]),
                ),
                links.clone(),
            ),
            CREATED_AT,
        )
        .expect("operation");

        assert_eq!(operation.id, id);
        assert_eq!(operation.target, target);
        assert_eq!(operation.links, links);
        assert_eq!(operation.state, PrivilegedOperationState::Prepared);
        assert_eq!(operation.attempt_count, 0);
        assert_eq!(operation.revision, 0);
        assert_eq!(operation.created_at, CREATED_AT);
        assert_eq!(operation.updated_at, CREATED_AT);
        assert_eq!(operation.review, None);
    }

    #[test]
    fn state_transition_matrix_is_exhaustive() {
        use PrivilegedOperationState::{
            Cancelled, Dispatching, Failed, InterruptedNeedsReview, Prepared, RetryPending,
            Reviewed, Succeeded,
        };

        let states = [
            Prepared,
            Dispatching,
            RetryPending,
            Succeeded,
            Failed,
            InterruptedNeedsReview,
            Reviewed,
            Cancelled,
        ];
        for from in states {
            for to in states {
                let expected = matches!(
                    (from, to),
                    (Prepared, Dispatching | Cancelled)
                        | (
                            Dispatching,
                            RetryPending | Succeeded | Failed | InterruptedNeedsReview
                        )
                        | (RetryPending, Dispatching)
                        | (InterruptedNeedsReview, Reviewed)
                );
                assert_eq!(
                    from.permits(to),
                    expected,
                    "unexpected edge {from:?} -> {to:?}"
                );
            }
        }
    }

    #[test]
    fn retry_safe_interruption_can_dispatch_a_new_attempt() {
        let mut operation = operation(PrivilegedOperationKind::RunnerHealth);
        operation.dispatch(101).expect("first dispatch");
        operation.interrupt(102).expect("recover interruption");
        assert_eq!(operation.state, PrivilegedOperationState::RetryPending);
        operation.dispatch(103).expect("second dispatch");
        operation.succeed(104).expect("success");

        assert_eq!(operation.state, PrivilegedOperationState::Succeeded);
        assert_eq!(operation.attempt_count, 2);
        assert_eq!(operation.revision, 4);
        assert_eq!(operation.updated_at, 104);
    }

    #[test]
    fn non_idempotent_interruption_requires_review_and_never_redispatches() {
        for kind in [
            PrivilegedOperationKind::CatalogApply,
            PrivilegedOperationKind::IntegrationStart,
            PrivilegedOperationKind::IntegrationStop,
            PrivilegedOperationKind::ComputerAct,
        ] {
            let mut operation = operation(kind);
            operation.dispatch(101).expect("dispatch");
            operation.interrupt(102).expect("interrupt");
            assert_eq!(
                operation.state,
                PrivilegedOperationState::InterruptedNeedsReview
            );
            assert!(matches!(
                operation.dispatch(103),
                Err(PrivilegedOperationError::InvalidTransition { .. })
            ));
            operation
                .review(PrivilegedOperationReview::Abandoned, 104)
                .expect("review");
            assert_eq!(operation.state, PrivilegedOperationState::Reviewed);
            assert_eq!(operation.review, Some(PrivilegedOperationReview::Abandoned));
            assert!(operation.state.is_terminal());
            assert!(matches!(
                operation.dispatch(105),
                Err(PrivilegedOperationError::InvalidTransition { .. })
            ));
        }
    }

    #[test]
    fn retry_pending_rejects_every_non_idempotent_kind_without_mutation() {
        for kind in [
            PrivilegedOperationKind::CatalogApply,
            PrivilegedOperationKind::IntegrationStart,
            PrivilegedOperationKind::IntegrationStop,
            PrivilegedOperationKind::ComputerAct,
        ] {
            let mut operation = operation(kind);
            operation.dispatch(101).expect("dispatch");
            let snapshot = operation.clone();
            assert_eq!(
                operation.schedule_retry(102),
                Err(PrivilegedOperationError::RetryPolicyViolation { kind })
            );
            assert_eq!(operation, snapshot);
        }
    }

    #[test]
    fn terminal_and_reviewed_states_never_dispatch() {
        for state in [
            PrivilegedOperationState::Succeeded,
            PrivilegedOperationState::Failed,
            PrivilegedOperationState::Reviewed,
            PrivilegedOperationState::Cancelled,
        ] {
            let mut operation = operation(PrivilegedOperationKind::RunnerHealth);
            operation.state = state;
            assert!(state.is_terminal());
            assert_eq!(
                operation.dispatch(101),
                Err(PrivilegedOperationError::InvalidTransition {
                    from: state,
                    to: PrivilegedOperationState::Dispatching,
                })
            );
        }
    }

    #[test]
    fn known_failure_and_undispatched_cancellation_are_terminal() {
        let mut failed = operation(PrivilegedOperationKind::ComputerObserve);
        failed.dispatch(101).expect("dispatch");
        failed.fail(102).expect("fail");
        assert_eq!(failed.state, PrivilegedOperationState::Failed);
        assert!(failed.state.is_terminal());

        let mut cancelled = operation(PrivilegedOperationKind::ComputerAct);
        cancelled.cancel(101).expect("cancel");
        assert_eq!(cancelled.state, PrivilegedOperationState::Cancelled);
        assert_eq!(cancelled.attempt_count, 0);
        assert!(cancelled.state.is_terminal());
    }

    #[test]
    fn clock_regression_is_distinct_and_never_mutates_the_operation() {
        let mut operation = operation(PrivilegedOperationKind::RunnerHealth);
        operation.dispatch(110).expect("dispatch");
        let snapshot = operation.clone();
        assert_eq!(
            operation.succeed(109),
            Err(PrivilegedOperationError::ClockRegression {
                current: 110,
                attempted: 109,
            })
        );
        assert_eq!(operation, snapshot);
    }

    #[test]
    fn expired_authority_cannot_start_or_resume_dispatch() {
        let mut operation = operation(PrivilegedOperationKind::RunnerHealth);
        let snapshot = operation.clone();
        assert_eq!(
            operation.dispatch(EXPIRES_AT + 1),
            Err(PrivilegedOperationError::AuthorityExpired {
                expires_at: EXPIRES_AT,
            })
        );
        assert_eq!(operation, snapshot);

        operation.dispatch(101).expect("dispatch");
        operation.schedule_retry(102).expect("retry pending");
        let snapshot = operation.clone();
        assert_eq!(
            operation.dispatch(EXPIRES_AT + 1),
            Err(PrivilegedOperationError::AuthorityExpired {
                expires_at: EXPIRES_AT,
            })
        );
        assert_eq!(operation, snapshot);
    }

    #[test]
    fn stale_or_exhausted_transitions_do_not_partially_mutate() {
        let mut revision_exhausted = operation(PrivilegedOperationKind::RunnerHealth);
        revision_exhausted.revision = u64::MAX;
        let snapshot = revision_exhausted.clone();
        assert_eq!(
            revision_exhausted.dispatch(101),
            Err(PrivilegedOperationError::RevisionExhausted)
        );
        assert_eq!(revision_exhausted, snapshot);

        let mut attempts_exhausted = operation(PrivilegedOperationKind::RunnerHealth);
        attempts_exhausted.attempt_count = u32::MAX;
        let snapshot = attempts_exhausted.clone();
        assert_eq!(
            attempts_exhausted.dispatch(101),
            Err(PrivilegedOperationError::AttemptCountExhausted)
        );
        assert_eq!(attempts_exhausted, snapshot);
    }

    #[test]
    fn expired_initial_authority_and_self_supersession_are_rejected() {
        let id = PrivilegedOperationId::new("privileged-operation-0001").expect("operation id");
        let mut expired_intent = intent(PrivilegedOperationKind::RunnerHealth);
        expired_intent.authority.expires_at = CREATED_AT - 1;
        assert!(matches!(
            PrivilegedOperation::prepare(id.clone(), expired_intent, CREATED_AT),
            Err(PrivilegedOperationError::AuthorityExpired { .. })
        ));

        let mut self_superseding_intent = intent(PrivilegedOperationKind::RunnerHealth);
        self_superseding_intent.links.supersedes_id = Some(id.clone());
        assert!(matches!(
            PrivilegedOperation::prepare(id, self_superseding_intent, CREATED_AT),
            Err(PrivilegedOperationError::SelfSupersession)
        ));
    }

    #[test]
    fn run_owned_links_require_their_owning_run() {
        for links in [
            PrivilegedOperationLinks {
                effect_id: Some(EffectId::new("effect-1").expect("effect id")),
                ..PrivilegedOperationLinks::default()
            },
            PrivilegedOperationLinks {
                approval_id: Some(ApprovalId::new("approval-1").expect("approval id")),
                ..PrivilegedOperationLinks::default()
            },
        ] {
            assert!(matches!(
                PrivilegedOperation::prepare(
                    PrivilegedOperationId::new("privileged-operation-0001").expect("operation id"),
                    intent_with_target(
                        PrivilegedOperationKind::ComputerAct,
                        target(PrivilegedOperationKind::ComputerAct),
                        links,
                    ),
                    CREATED_AT,
                ),
                Err(PrivilegedOperationError::InvalidLinks)
            ));
        }

        let system_scoped_supersession = PrivilegedOperation::prepare(
            PrivilegedOperationId::new("privileged-operation-0002").expect("operation id"),
            intent_with_target(
                PrivilegedOperationKind::RunnerHealth,
                target(PrivilegedOperationKind::RunnerHealth),
                PrivilegedOperationLinks {
                    supersedes_id: Some(
                        PrivilegedOperationId::new("privileged-operation-0001")
                            .expect("superseded id"),
                    ),
                    ..PrivilegedOperationLinks::default()
                },
            ),
            CREATED_AT,
        );
        assert!(system_scoped_supersession.is_ok());
    }

    #[test]
    fn prepare_rejects_kind_target_mismatch_and_invalid_observation_revision() {
        let prepare = |kind, target| {
            PrivilegedOperation::prepare(
                PrivilegedOperationId::new("privileged-operation-0001").expect("operation id"),
                intent_with_target(kind, target, PrivilegedOperationLinks::default()),
                CREATED_AT,
            )
        };

        assert!(matches!(
            prepare(
                PrivilegedOperationKind::RunnerHealth,
                target(PrivilegedOperationKind::CatalogApply),
            ),
            Err(PrivilegedOperationError::TargetKindMismatch {
                operation_kind: PrivilegedOperationKind::RunnerHealth,
                target_kind: PrivilegedOperationKind::CatalogApply,
            })
        ));

        assert_eq!(
            prepare(
                PrivilegedOperationKind::ComputerAct,
                PrivilegedOperationTarget::ComputerAct {
                    vm_id: resource("work-vm"),
                    integration_id: resource("wisp"),
                    instance_id: resource("instance-1"),
                    application_id: resource("application-1"),
                    observation_revision: 0,
                },
            ),
            Err(PrivilegedOperationError::InvalidTarget)
        );
    }

    #[test]
    fn restore_accepts_only_snapshots_reachable_through_the_state_machine() {
        let prepared = operation(PrivilegedOperationKind::RunnerHealth);
        assert_eq!(
            PrivilegedOperation::restore(prepared.clone()).expect("restore prepared"),
            prepared
        );

        let mut retry_pending = operation(PrivilegedOperationKind::ComputerObserve);
        retry_pending.dispatch(101).expect("dispatch");
        retry_pending.schedule_retry(102).expect("retry pending");
        retry_pending.dispatch(103).expect("second dispatch");
        assert_eq!(
            PrivilegedOperation::restore(retry_pending.clone()).expect("restore dispatching"),
            retry_pending
        );

        let mut reviewed = operation(PrivilegedOperationKind::IntegrationStart);
        reviewed.dispatch(101).expect("dispatch");
        reviewed.interrupt(102).expect("interrupt");
        reviewed
            .review(PrivilegedOperationReview::Abandoned, 103)
            .expect("review");
        assert_eq!(
            PrivilegedOperation::restore(reviewed.clone()).expect("restore reviewed"),
            reviewed
        );

        let mut dispatching_at_expiry = operation(PrivilegedOperationKind::RunnerHealth);
        dispatching_at_expiry
            .dispatch(EXPIRES_AT)
            .expect("dispatch at inclusive authority boundary");
        assert_eq!(
            PrivilegedOperation::restore(dispatching_at_expiry.clone())
                .expect("restore dispatch at authority boundary"),
            dispatching_at_expiry
        );

        let mut recovered_after_expiry = dispatching_at_expiry;
        recovered_after_expiry
            .interrupt(EXPIRES_AT + 1)
            .expect("recovery may complete after authority expires");
        assert_eq!(
            PrivilegedOperation::restore(recovered_after_expiry.clone())
                .expect("restore recovery after authority expiry"),
            recovered_after_expiry
        );
    }

    #[test]
    fn restore_rejects_corrupt_revision_review_and_retry_metadata() {
        let mut wrong_revision = operation(PrivilegedOperationKind::RunnerHealth);
        wrong_revision.dispatch(101).expect("dispatch");
        wrong_revision.revision = 7;
        assert_eq!(
            PrivilegedOperation::restore(wrong_revision),
            Err(PrivilegedOperationError::InvalidPersistedState)
        );

        let mut forged_review = operation(PrivilegedOperationKind::IntegrationStart);
        forged_review.review = Some(PrivilegedOperationReview::ConfirmedSucceeded);
        assert_eq!(
            PrivilegedOperation::restore(forged_review),
            Err(PrivilegedOperationError::InvalidPersistedState)
        );

        let mut forbidden_retry = operation(PrivilegedOperationKind::ComputerAct);
        forbidden_retry.dispatch(101).expect("dispatch");
        forbidden_retry.state = PrivilegedOperationState::RetryPending;
        forbidden_retry.revision = 2;
        assert_eq!(
            PrivilegedOperation::restore(forbidden_retry),
            Err(PrivilegedOperationError::InvalidPersistedState)
        );

        for state in [
            PrivilegedOperationState::RetryPending,
            PrivilegedOperationState::Succeeded,
            PrivilegedOperationState::Failed,
        ] {
            let mut zero_attempt = operation(PrivilegedOperationKind::RunnerHealth);
            zero_attempt.state = state;
            assert_eq!(
                PrivilegedOperation::restore(zero_attempt),
                Err(PrivilegedOperationError::InvalidPersistedState)
            );
        }

        let mut dispatch_after_expiry = operation(PrivilegedOperationKind::RunnerHealth);
        dispatch_after_expiry
            .dispatch(EXPIRES_AT)
            .expect("seed otherwise valid dispatching state");
        dispatch_after_expiry.updated_at = EXPIRES_AT + 1;
        assert_eq!(
            PrivilegedOperation::restore(dispatch_after_expiry),
            Err(PrivilegedOperationError::InvalidPersistedState)
        );
    }
}
