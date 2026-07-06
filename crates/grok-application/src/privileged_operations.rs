use std::sync::Arc;

use async_trait::async_trait;
use grok_domain::{
    PayloadDigest, PrivilegedOperation, PrivilegedOperationId, PrivilegedOperationIntent,
    PrivilegedOperationState, UnixMillis,
};
use sha2::{Digest, Sha256};

use crate::{ApplicationError, Clock, IdGenerator, StoreError};

/// Schema-7 payload lower bound. Empty and one-byte envelopes are never valid.
pub const MIN_PRIVILEGED_OPERATION_PAYLOAD_BYTES: usize = 2;
/// Schema-7 payload upper bound at every application/storage boundary.
pub const MAX_PRIVILEGED_OPERATION_PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
/// Broker attempts may never outlive this interval.
pub const MAX_PRIVILEGED_ATTEMPT_DURATION_MS: u64 = 30_000;
/// Maximum interrupted attempts inspected in one startup pass.
pub const MAX_PRIVILEGED_RECOVERY_BATCH: usize = 256;

/// Immutable intent and exact bounded bytes to commit before privileged I/O.
pub struct PreparePrivilegedOperation {
    /// Closed daemon-owned semantic intent.
    pub intent: PrivilegedOperationIntent,
    /// Exact request envelope retained for a later typed gateway.
    pub payload: Vec<u8>,
}

/// Result of atomic intent preparation or a verified durable replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegedPreparation {
    /// Original durable aggregate for both first execution and replay.
    pub operation: PrivilegedOperation,
    /// True only when this call committed the intent and payload.
    pub created: bool,
}

/// Exact attempt metadata committed atomically with `dispatching`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegedDispatchAttempt {
    /// New attempt sequence, equal to the aggregate attempt count.
    pub sequence: u32,
    /// Fresh transport identity; never the stable journal identity.
    pub transport_operation_id: String,
    /// SHA-256 of the exact wire request for this attempt.
    pub wire_digest: [u8; 32],
    /// Current broker boot identity.
    pub broker_boot_id: [u8; 16],
    /// Current guest boot identity.
    pub guest_boot_id: [u8; 16],
    /// Durable timestamp immediately before dispatch.
    pub started_at: UnixMillis,
    /// Absolute bounded transport deadline.
    pub deadline_unix_ms: UnixMillis,
}

/// Parameters for reserving one future typed gateway dispatch.
pub struct BeginPrivilegedDispatch {
    /// Existing prepared or retry-pending journal identity.
    pub operation_id: PrivilegedOperationId,
    /// Optimistic aggregate revision observed by the caller.
    pub expected_revision: u64,
    /// Globally unique attempt transport identity.
    pub transport_operation_id: String,
    /// Digest of the exact request a future typed gateway will receive.
    pub wire_digest: [u8; 32],
    /// Current qualified broker boot identity.
    pub broker_boot_id: [u8; 16],
    /// Current authenticated guest boot identity.
    pub guest_boot_id: [u8; 16],
    /// Positive transport lifetime, bounded to 30 seconds.
    pub timeout_ms: u64,
}

/// Dispatching row and its correlated last attempt for startup recovery.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivilegedRecoveryCandidate {
    /// Validated aggregate in `dispatching`.
    pub operation: PrivilegedOperation,
    /// Correlated last-attempt sequence.
    pub attempt_sequence: u32,
    /// Correlated last-attempt start time.
    pub attempt_started_at: UnixMillis,
}

/// Bounded startup recovery result without exposing journal details over IPC.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrivilegedRecoverySummary {
    /// Retry-safe operations moved to `retry_pending`.
    pub retry_pending: usize,
    /// Non-idempotent operations moved to explicit human review.
    pub interrupted_needs_review: usize,
    /// True when the bounded pass observed additional dispatching rows.
    pub truncated: bool,
}

impl PrivilegedRecoverySummary {
    /// Total number of atomically recovered dispatches.
    #[must_use]
    pub const fn recovered(self) -> usize {
        self.retry_pending + self.interrupted_needs_review
    }
}

/// Capability-focused persistence boundary for the privileged journal.
///
/// Compound methods deliberately encode the transactions required by ADR 0003;
/// there is no generic privileged method or opaque execution gateway here.
#[async_trait]
pub trait PrivilegedOperationStore: Send + Sync {
    /// Resolves an existing authority/key tombstone and conflicts on changed intent.
    async fn resolve_preparation(
        &self,
        intent: &PrivilegedOperationIntent,
    ) -> Result<Option<PrivilegedOperation>, StoreError>;

    /// Atomically inserts intent and exact payload, or replays an exact digest match.
    async fn prepare_with_payload(
        &self,
        operation: PrivilegedOperation,
        payload: Vec<u8>,
    ) -> Result<PrivilegedPreparation, StoreError>;

    /// Loads one validated aggregate without loading or exposing its payload.
    async fn get_privileged_operation(
        &self,
        id: &PrivilegedOperationId,
    ) -> Result<PrivilegedOperation, StoreError>;

    /// Atomically moves the aggregate to dispatching and appends its attempt.
    async fn begin_dispatch_with_attempt(
        &self,
        operation: PrivilegedOperation,
        expected_revision: u64,
        attempt: PrivilegedDispatchAttempt,
    ) -> Result<PrivilegedOperation, StoreError>;

    /// Lists a stable bounded set of validated interrupted candidates.
    async fn list_dispatching_for_recovery(
        &self,
        limit: usize,
    ) -> Result<Vec<PrivilegedRecoveryCandidate>, StoreError>;

    /// Atomically records unknown attempt certainty and its safe lifecycle edge.
    async fn recover_interrupted_attempt(
        &self,
        operation: PrivilegedOperation,
        expected_revision: u64,
        attempt_sequence: u32,
        completed_at: UnixMillis,
    ) -> Result<PrivilegedOperation, StoreError>;
}

/// Internal journal coordinator. It persists state but grants no Work authority.
pub struct PrivilegedOperationService {
    store: Arc<dyn PrivilegedOperationStore>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl PrivilegedOperationService {
    /// Creates an internal privileged-operation journal coordinator.
    #[must_use]
    pub fn new(
        store: Arc<dyn PrivilegedOperationStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { store, clock, ids }
    }

    /// Commits immutable intent and exact payload before any future gateway I/O.
    ///
    /// # Errors
    ///
    /// Returns an application error for invalid bounds/digests, invalid domain
    /// intent, idempotency conflicts, or storage failures.
    pub async fn prepare(
        &self,
        input: PreparePrivilegedOperation,
    ) -> Result<PrivilegedPreparation, ApplicationError> {
        validate_payload(&input.payload, input.intent.payload_digest)?;
        if let Some(operation) = self.store.resolve_preparation(&input.intent).await? {
            return Ok(PrivilegedPreparation {
                operation,
                created: false,
            });
        }
        let operation = PrivilegedOperation::prepare(
            PrivilegedOperationId::new(self.ids.generate("privileged-operation"))?,
            input.intent,
            self.clock.now(),
        )?;
        self.store
            .prepare_with_payload(operation, input.payload)
            .await
            .map_err(Into::into)
    }

    /// Atomically persists `dispatching` and the exact attempt before gateway I/O.
    ///
    /// This method still performs no I/O and does not enable Work.
    ///
    /// # Errors
    ///
    /// Returns an application error for invalid attempt metadata, stale state,
    /// expired authority, or storage failures.
    pub async fn begin_dispatch(
        &self,
        input: BeginPrivilegedDispatch,
    ) -> Result<PrivilegedOperation, ApplicationError> {
        validate_transport_id(&input.transport_operation_id)?;
        if input.broker_boot_id == [0; 16] || input.guest_boot_id == [0; 16] {
            return Err(ApplicationError::InvalidInput(
                "privileged attempt boot identities must be nonzero".into(),
            ));
        }
        if input.timeout_ms == 0 || input.timeout_ms > MAX_PRIVILEGED_ATTEMPT_DURATION_MS {
            return Err(ApplicationError::InvalidInput(
                "privileged attempt duration must be between 1 and 30000 milliseconds".into(),
            ));
        }
        let mut operation = self
            .store
            .get_privileged_operation(&input.operation_id)
            .await?;
        if input.transport_operation_id == operation.id.as_str() {
            return Err(ApplicationError::InvalidInput(
                "transport identity must differ from the durable journal identity".into(),
            ));
        }
        if operation.revision != input.expected_revision {
            return Err(ApplicationError::Conflict);
        }
        let started_at = self.clock.now();
        let deadline_unix_ms = started_at
            .checked_add(input.timeout_ms)
            .ok_or_else(|| ApplicationError::InvalidInput("attempt deadline overflow".into()))?;
        operation.dispatch(started_at)?;
        let attempt = PrivilegedDispatchAttempt {
            sequence: operation.attempt_count,
            transport_operation_id: input.transport_operation_id,
            wire_digest: input.wire_digest,
            broker_boot_id: input.broker_boot_id,
            guest_boot_id: input.guest_boot_id,
            started_at,
            deadline_unix_ms,
        };
        self.store
            .begin_dispatch_with_attempt(operation, input.expected_revision, attempt)
            .await
            .map_err(Into::into)
    }

    /// Recovers at most `limit` interrupted attempts without replaying any I/O.
    ///
    /// Retry-safe operations become retry-pending. Non-idempotent operations
    /// become interrupted-needs-review. Re-running recovery is idempotent because
    /// only durable `dispatching` attempts are selected.
    ///
    /// # Errors
    ///
    /// Returns an application error when the bound is invalid, a durable row is
    /// corrupt, a concurrent mutation wins, or the atomic recovery commit fails.
    pub async fn recover_interrupted(
        &self,
        limit: usize,
    ) -> Result<PrivilegedRecoverySummary, ApplicationError> {
        if limit == 0 || limit > MAX_PRIVILEGED_RECOVERY_BATCH {
            return Err(ApplicationError::InvalidInput(format!(
                "privileged recovery limit must be between 1 and {MAX_PRIVILEGED_RECOVERY_BATCH}"
            )));
        }
        let query_limit = limit
            .checked_add(1)
            .ok_or_else(|| ApplicationError::InvalidInput("recovery limit overflow".into()))?;
        let candidates = self
            .store
            .list_dispatching_for_recovery(query_limit)
            .await?;
        let mut summary = PrivilegedRecoverySummary {
            truncated: candidates.len() > limit,
            ..PrivilegedRecoverySummary::default()
        };
        for candidate in candidates.into_iter().take(limit) {
            validate_recovery_candidate(&candidate)?;
            let expected_revision = candidate.operation.revision;
            let completed_at = self
                .clock
                .now()
                .max(candidate.operation.updated_at)
                .max(candidate.attempt_started_at);
            let mut recovered = candidate.operation;
            recovered.interrupt(completed_at)?;
            let recovered = self
                .store
                .recover_interrupted_attempt(
                    recovered,
                    expected_revision,
                    candidate.attempt_sequence,
                    completed_at,
                )
                .await?;
            match recovered.state {
                PrivilegedOperationState::RetryPending => summary.retry_pending += 1,
                PrivilegedOperationState::InterruptedNeedsReview => {
                    summary.interrupted_needs_review += 1;
                }
                _ => {
                    return Err(ApplicationError::Integrity(
                        "privileged recovery committed an invalid state".into(),
                    ));
                }
            }
        }
        Ok(summary)
    }
}

fn validate_payload(payload: &[u8], expected: PayloadDigest) -> Result<(), ApplicationError> {
    if !(MIN_PRIVILEGED_OPERATION_PAYLOAD_BYTES..=MAX_PRIVILEGED_OPERATION_PAYLOAD_BYTES)
        .contains(&payload.len())
    {
        return Err(ApplicationError::InvalidInput(
            "privileged operation payload is outside durable bounds".into(),
        ));
    }
    let actual: [u8; 32] = Sha256::digest(payload).into();
    if actual.as_slice() != expected.as_bytes() {
        return Err(ApplicationError::InvalidInput(
            "privileged operation payload digest does not match exact bytes".into(),
        ));
    }
    Ok(())
}

fn validate_transport_id(value: &str) -> Result<(), ApplicationError> {
    if !(16..=128).contains(&value.len())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
    {
        return Err(ApplicationError::InvalidInput(
            "transport operation id must be 16-128 allowlisted ASCII bytes".into(),
        ));
    }
    Ok(())
}

fn validate_recovery_candidate(
    candidate: &PrivilegedRecoveryCandidate,
) -> Result<(), ApplicationError> {
    if candidate.operation.state != PrivilegedOperationState::Dispatching
        || candidate.attempt_sequence == 0
        || candidate.attempt_sequence != candidate.operation.attempt_count
        || candidate.attempt_started_at != candidate.operation.updated_at
    {
        return Err(ApplicationError::Integrity(
            "privileged recovery candidate is internally inconsistent".into(),
        ));
    }
    Ok(())
}
