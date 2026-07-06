use std::sync::Arc;

use async_trait::async_trait;
use grok_domain::{
    AUTOMATION_SCHEDULE_CALCULATOR_VERSION, Automation, AutomationExecutionSnapshot, AutomationId,
    AutomationOccurrence, AutomationOccurrenceId, AutomationOccurrenceState,
    AutomationScheduleCursor, AutomationScheduleDecision, AutomationSchedulerLease,
    AutomationSchedulerLeaseToken, AutomationSchedulerOwnerId, AutomationState,
    MAX_AUTOMATION_SCHEDULE_DECISIONS, MissedRunPolicy, UnixMillis,
};

use crate::{
    ApplicationError, Clock, IdGenerator, MutationCommand, StoreError,
    mutations::mutation_command_bytes,
};

/// Maximum definition records inspected by one journal tick.
pub const MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS: usize = 100;
/// Maximum expired claims recovered in one transaction.
pub const MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH: usize = 100;
/// Lease lifetime used by the journal-only scheduler kernel.
pub const AUTOMATION_SCHEDULER_LEASE_TTL_MS: u64 = 30_000;
/// Maximum proposed occurrence decisions committed with one cursor advancement.
pub const MAX_AUTOMATION_SCHEDULER_EVALUATION_OCCURRENCES: usize = 3;

const AUTOMATION_SCHEDULER_EVALUATION_WINDOW_MS: u64 = 180 * 86_400_000;

/// Result of atomically acquiring or renewing the singleton scheduler lease.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomationSchedulerLeaseAcquisition {
    /// The caller owns the returned fenced generation.
    Acquired {
        /// Current durable lease.
        lease: AutomationSchedulerLease,
        /// True only for an unexpired same-owner renewal.
        continuous: bool,
        /// Earliest due timestamp that can still be considered part of this
        /// continuous in-process scheduling interval.
        continuity_started_at: UnixMillis,
    },
    /// Another unexpired owner retains the lease.
    Busy {
        /// Current durable owner and expiry.
        lease: AutomationSchedulerLease,
    },
    /// Wall time moved behind a durable lease timestamp. No ownership or
    /// schedule state was changed.
    ClockRegressed {
        /// Durable wall-clock floor which must be reached before retrying.
        durable_floor: UnixMillis,
    },
}

/// Enabled definition plus its optional durable calculator cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationScheduleCandidate {
    /// Canonical current definition.
    pub automation: Automation,
    /// Cursor for the same definition revision, if initialized.
    pub cursor: Option<AutomationScheduleCursor>,
}

/// One atomic cursor advancement and its bounded occurrence decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationScheduleEvaluationCommit {
    /// Exact fenced scheduler owner.
    pub lease: AutomationSchedulerLeaseToken,
    /// Definition revision rechecked by the store.
    pub expected_automation_revision: u64,
    /// Cursor revision observed by the calculator, or `None` for initialization.
    pub expected_cursor_revision: Option<u64>,
    /// New canonical cursor snapshot.
    pub cursor: AutomationScheduleCursor,
    /// Proposed decisions in chronological order. The store atomically applies
    /// overlap policy and may return different terminal/queued states.
    pub occurrences: Vec<AutomationOccurrence>,
    /// Wall time used for every decision in this commit.
    pub observed_at: UnixMillis,
    /// Exact internal idempotency evidence.
    pub command: MutationCommand,
}

/// Exact committed evaluation projection returned on first write or replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationScheduleEvaluationResult {
    /// Committed cursor.
    pub cursor: AutomationScheduleCursor,
    /// Actual occurrence states after atomic overlap adjudication.
    pub occurrences: Vec<AutomationOccurrence>,
}

/// Durable evidence for one bounded occurrence claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationOccurrenceClaimAttempt {
    /// Owning occurrence.
    pub occurrence_id: AutomationOccurrenceId,
    /// One-based contiguous attempt number.
    pub sequence: u32,
    /// Exact scheduler owner and fencing generation.
    pub owner_id: AutomationSchedulerOwnerId,
    /// Exact positive fence.
    pub fence: u64,
    /// Claim acquisition time.
    pub claimed_at: UnixMillis,
    /// Exclusive claim expiry.
    pub expires_at: UnixMillis,
    /// Completion time, absent while the claim is live.
    pub completed_at: Option<UnixMillis>,
    /// Stable terminal evidence, absent while the claim is live.
    pub completion: Option<AutomationOccurrenceClaimCompletion>,
    /// Exact claim command fingerprint.
    pub request_fingerprint: [u8; 32],
}

/// One-way completion classification for immutable claim-attempt evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationOccurrenceClaimCompletion {
    /// An unlinked claim expired and safely returned to pending.
    ExpiredUnlinked,
    /// A durable run was linked before the volatile claim ended.
    RunLinked,
    /// Bounded attempts were exhausted without a run and require review.
    AttemptsExhausted,
    /// The occurrence terminalized for another exact reason.
    Terminalized,
}

/// Input for an atomic occurrence claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimAutomationOccurrence {
    /// Global scheduler owner and fence.
    pub lease: AutomationSchedulerLeaseToken,
    /// Exact pending occurrence.
    pub occurrence_id: AutomationOccurrenceId,
    /// Optimistic occurrence revision.
    pub expected_revision: u64,
    /// Claim acquisition time.
    pub claimed_at: UnixMillis,
    /// Exclusive claim expiry.
    pub expires_at: UnixMillis,
    /// Internal exact idempotency evidence.
    pub command: MutationCommand,
}

/// Bounded expired-claim recovery result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AutomationSchedulerRecoverySummary {
    /// Unlinked claims safely returned to pending.
    pub released_unlinked: usize,
    /// Linked occurrences moved to explicit review without replay.
    pub interrupted_linked: usize,
    /// Attempt-exhausted occurrences moved to explicit review.
    pub attempts_exhausted: usize,
    /// More expired rows remain after this bounded pass.
    pub truncated: bool,
}

/// Result class for one bounded journal tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutomationSchedulerTickStatus {
    /// The caller owned the lease and evaluated the returned bounded page.
    Completed,
    /// Another live daemon owner retained the lease; nothing was evaluated.
    LeaseBusy,
    /// Wall time was behind durable state; no scheduler mutation was made.
    ClockRegressed,
}

/// Bounded journal tick summary. It never represents executed work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationSchedulerTickSummary {
    /// Stable tick result class.
    pub status: AutomationSchedulerTickStatus,
    /// Whether this tick continued an unexpired same-owner lease.
    pub lease_continuous: bool,
    /// Definitions inspected in this page.
    pub definitions_evaluated: usize,
    /// Cursors initialized for previously unseen enabled definitions.
    pub cursors_initialized: usize,
    /// Occurrence decisions durably materialized.
    pub occurrences_materialized: usize,
    /// Next stable definition cursor when another page remains.
    pub next_definition_cursor: Option<AutomationId>,
    /// Durable wall-clock floor for a regressed clock.
    pub durable_clock_floor: Option<UnixMillis>,
}

impl AutomationSchedulerTickSummary {
    const fn inactive(status: AutomationSchedulerTickStatus) -> Self {
        Self {
            status,
            lease_continuous: false,
            definitions_evaluated: 0,
            cursors_initialized: 0,
            occurrences_materialized: 0,
            next_definition_cursor: None,
            durable_clock_floor: None,
        }
    }
}

/// Read-only journal health. This does not imply execution readiness.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AutomationSchedulerJournalStatus {
    /// Current lease, if one has ever been acquired and remains persisted.
    pub lease: Option<AutomationSchedulerLease>,
    /// Definitions with initialized cursors.
    pub cursor_count: u64,
    /// Immediately eligible occurrences.
    pub pending_count: u64,
    /// Single retained overlaps.
    pub queued_overlap_count: u64,
    /// Volatile unlinked claims.
    pub claimed_count: u64,
    /// Run-linked occurrences which must never be redispatched on lease expiry.
    pub run_linked_count: u64,
    /// Terminal occurrences requiring explicit review.
    pub needs_review_count: u64,
}

/// Capability-focused durable boundary for the automation scheduler journal.
///
/// No method executes a Run, provider call, tool, or privileged operation.
#[async_trait]
pub trait AutomationSchedulerStore: Send + Sync {
    /// Atomically acquires, renews, or reports the current singleton lease.
    async fn acquire_automation_scheduler_lease(
        &self,
        owner_id: &AutomationSchedulerOwnerId,
        now: UnixMillis,
        ttl_ms: u64,
    ) -> Result<AutomationSchedulerLeaseAcquisition, StoreError>;

    /// Lists enabled definitions in stable identifier order with their cursors.
    async fn list_automation_schedule_candidates(
        &self,
        after: Option<&AutomationId>,
        limit: usize,
    ) -> Result<Vec<AutomationScheduleCandidate>, StoreError>;

    /// Commits one exact evaluation atomically with overlap adjudication.
    async fn commit_automation_schedule_evaluation(
        &self,
        evaluation: AutomationScheduleEvaluationCommit,
    ) -> Result<AutomationScheduleEvaluationResult, StoreError>;

    /// Loads one exact occurrence.
    async fn get_automation_occurrence(
        &self,
        id: &AutomationOccurrenceId,
    ) -> Result<AutomationOccurrence, StoreError>;

    /// Lists occurrences in stable logical schedule order.
    async fn list_automation_occurrences(
        &self,
        automation_id: &AutomationId,
        after: Option<&AutomationOccurrenceId>,
        limit: usize,
    ) -> Result<Vec<AutomationOccurrence>, StoreError>;

    /// Atomically claims one pending occurrence and appends immutable attempt evidence.
    async fn claim_automation_occurrence(
        &self,
        claim: ClaimAutomationOccurrence,
    ) -> Result<AutomationOccurrence, StoreError>;

    /// Recovers expired claims without dispatching any work.
    async fn recover_automation_occurrence_claims(
        &self,
        lease: &AutomationSchedulerLeaseToken,
        now: UnixMillis,
        limit: usize,
    ) -> Result<AutomationSchedulerRecoverySummary, StoreError>;

    /// Returns bounded aggregate journal health without execution material.
    async fn automation_scheduler_journal_status(
        &self,
    ) -> Result<AutomationSchedulerJournalStatus, StoreError>;
}

/// Journal-only scheduler calculator. It has deliberately no execution dependency.
pub struct AutomationSchedulerService {
    store: Arc<dyn AutomationSchedulerStore>,
    clock: Arc<dyn Clock>,
    ids: Arc<dyn IdGenerator>,
}

impl AutomationSchedulerService {
    /// Creates a scheduler journal service with no Run or provider authority.
    #[must_use]
    pub fn new(
        store: Arc<dyn AutomationSchedulerStore>,
        clock: Arc<dyn Clock>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { store, clock, ids }
    }

    /// Borrows the wall clock for bounded scheduler operations.
    #[must_use]
    pub fn now(&self) -> UnixMillis {
        self.clock.now()
    }

    /// Generates an internal occurrence identity. The durable store still
    /// enforces logical-slot uniqueness independently of this value.
    pub(crate) fn occurrence_id(&self) -> Result<AutomationOccurrenceId, grok_domain::IdError> {
        AutomationOccurrenceId::new(self.ids.generate("automation-occurrence"))
    }

    /// Returns the read-only journal projection. Execution remains disabled.
    ///
    /// # Errors
    ///
    /// Returns [`StoreError`] when the durable journal cannot be read.
    pub async fn journal_status(&self) -> Result<AutomationSchedulerJournalStatus, StoreError> {
        self.store.automation_scheduler_journal_status().await
    }

    /// Evaluates one bounded page of internally enabled definitions and writes
    /// only scheduler journal state.
    ///
    /// This method has no execution dependency and cannot create a Run, call a
    /// provider, request approval, or invoke a tool.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] when persisted scheduler state is corrupt,
    /// storage is unavailable, or deterministic calculation cannot advance.
    pub async fn tick(
        &self,
        owner_id: &AutomationSchedulerOwnerId,
        after: Option<&AutomationId>,
        limit: usize,
    ) -> Result<AutomationSchedulerTickSummary, ApplicationError> {
        if !(1..=MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS).contains(&limit) {
            return Err(ApplicationError::InvalidInput(
                "automation scheduler tick limit is outside the supported bounds".into(),
            ));
        }
        let observed_at = self.clock.now();
        let acquisition = self
            .store
            .acquire_automation_scheduler_lease(
                owner_id,
                observed_at,
                AUTOMATION_SCHEDULER_LEASE_TTL_MS,
            )
            .await?;
        let (lease, continuous, continuity_started_at) = match acquisition {
            AutomationSchedulerLeaseAcquisition::Acquired {
                lease,
                continuous,
                continuity_started_at,
            } => (lease, continuous, continuity_started_at),
            AutomationSchedulerLeaseAcquisition::Busy { .. } => {
                return Ok(AutomationSchedulerTickSummary::inactive(
                    AutomationSchedulerTickStatus::LeaseBusy,
                ));
            }
            AutomationSchedulerLeaseAcquisition::ClockRegressed { durable_floor } => {
                let mut summary = AutomationSchedulerTickSummary::inactive(
                    AutomationSchedulerTickStatus::ClockRegressed,
                );
                summary.durable_clock_floor = Some(durable_floor);
                return Ok(summary);
            }
        };
        let mut candidates = self
            .store
            .list_automation_schedule_candidates(after, limit.saturating_add(1))
            .await?;
        let has_more = candidates.len() > limit;
        candidates.truncate(limit);
        let next_definition_cursor = has_more
            .then(|| {
                candidates
                    .last()
                    .map(|candidate| candidate.automation.id.clone())
            })
            .flatten();
        let mut summary = AutomationSchedulerTickSummary {
            status: AutomationSchedulerTickStatus::Completed,
            lease_continuous: continuous,
            definitions_evaluated: 0,
            cursors_initialized: 0,
            occurrences_materialized: 0,
            next_definition_cursor,
            durable_clock_floor: None,
        };
        let lease_token = lease.token();
        for candidate in candidates {
            let result = self
                .evaluate_candidate(&lease_token, continuity_started_at, observed_at, candidate)
                .await?;
            summary.definitions_evaluated = summary.definitions_evaluated.saturating_add(1);
            summary.cursors_initialized = summary
                .cursors_initialized
                .saturating_add(usize::from(result.initialized));
            summary.occurrences_materialized = summary
                .occurrences_materialized
                .saturating_add(result.occurrences);
        }
        Ok(summary)
    }

    /// Recovers one bounded batch of expired claims without executing work.
    ///
    /// # Errors
    ///
    /// Returns [`ApplicationError`] for invalid bounds, lease contention, clock
    /// regression, or persistence failure.
    pub async fn recover_expired_claims(
        &self,
        owner_id: &AutomationSchedulerOwnerId,
        limit: usize,
    ) -> Result<AutomationSchedulerRecoverySummary, ApplicationError> {
        if !(1..=MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH).contains(&limit) {
            return Err(ApplicationError::InvalidInput(
                "automation scheduler recovery limit is outside the supported bounds".into(),
            ));
        }
        let now = self.clock.now();
        let acquisition = self
            .store
            .acquire_automation_scheduler_lease(owner_id, now, AUTOMATION_SCHEDULER_LEASE_TTL_MS)
            .await?;
        let AutomationSchedulerLeaseAcquisition::Acquired { lease, .. } = acquisition else {
            return match acquisition {
                AutomationSchedulerLeaseAcquisition::Busy { .. } => Err(
                    ApplicationError::Unavailable("automation scheduler lease is busy".into()),
                ),
                AutomationSchedulerLeaseAcquisition::ClockRegressed { .. } => Err(
                    ApplicationError::InvalidState("automation scheduler clock regressed".into()),
                ),
                AutomationSchedulerLeaseAcquisition::Acquired { .. } => unreachable!(),
            };
        };
        Ok(self
            .store
            .recover_automation_occurrence_claims(&lease.token(), now, limit)
            .await?)
    }

    #[allow(clippy::too_many_lines)]
    async fn evaluate_candidate(
        &self,
        lease: &AutomationSchedulerLeaseToken,
        continuity_started_at: UnixMillis,
        observed_at: UnixMillis,
        candidate: AutomationScheduleCandidate,
    ) -> Result<CandidateEvaluationSummary, ApplicationError> {
        let automation = candidate.automation;
        if automation.state != AutomationState::Enabled {
            return Err(ApplicationError::Integrity(
                "scheduler store returned a non-enabled automation".into(),
            ));
        }
        let snapshot = AutomationExecutionSnapshot::new(
            automation.revision,
            automation.project_id.clone(),
            automation.title.clone(),
            automation.prompt.clone(),
            automation.schedule.clone(),
            automation.timezone.clone(),
            automation.missed_run_policy,
            automation.overlap_policy,
        )?;
        let Some(mut cursor) = candidate.cursor else {
            if observed_at < automation.updated_at {
                return Err(ApplicationError::InvalidState(
                    "automation scheduler clock predates definition activation".into(),
                ));
            }
            let next = snapshot
                .schedule
                .next_decision_after(&snapshot.timezone, automation.updated_at)?;
            let cursor = AutomationScheduleCursor::new(
                automation.id.clone(),
                &snapshot,
                automation.updated_at,
                Some(next),
                observed_at,
            )?;
            let command = evaluation_command(
                &self.ids.generate("automation-evaluation"),
                lease,
                automation.revision,
                None,
                &cursor,
                &[],
            )?;
            self.store
                .commit_automation_schedule_evaluation(AutomationScheduleEvaluationCommit {
                    lease: lease.clone(),
                    expected_automation_revision: automation.revision,
                    expected_cursor_revision: None,
                    cursor,
                    occurrences: Vec::new(),
                    observed_at,
                    command,
                })
                .await?;
            return Ok(CandidateEvaluationSummary {
                initialized: true,
                occurrences: 0,
            });
        };
        if cursor.automation_id != automation.id
            || cursor.definition_revision != automation.revision
            || cursor.schedule_fingerprint != snapshot.schedule_fingerprint
            || cursor.calculator_version != AUTOMATION_SCHEDULE_CALCULATOR_VERSION
        {
            return Err(ApplicationError::Integrity(
                "automation scheduler cursor does not match its definition".into(),
            ));
        }
        let expected_cursor_revision = Some(cursor.revision);
        if observed_at < cursor.evaluated_through {
            return Err(ApplicationError::InvalidState(
                "automation scheduler clock predates its durable cursor".into(),
            ));
        }
        if observed_at == cursor.evaluated_through {
            return Ok(CandidateEvaluationSummary {
                initialized: false,
                occurrences: 0,
            });
        }
        let through = cursor
            .evaluated_through
            .checked_add(AUTOMATION_SCHEDULER_EVALUATION_WINDOW_MS)
            .map_or(observed_at, |bounded| bounded.min(observed_at));
        let calculation = snapshot.schedule.decisions_between(
            &snapshot.timezone,
            cursor.evaluated_through,
            through,
            MAX_AUTOMATION_SCHEDULE_DECISIONS,
        )?;
        if calculation.truncated {
            return Err(ApplicationError::Unavailable(
                "automation scheduler calculation exceeded its bounded window".into(),
            ));
        }
        let mut occurrences = self.materialize_decisions(
            &automation,
            &snapshot,
            calculation.decisions,
            continuity_started_at,
            observed_at,
        )?;
        if occurrences.len() > MAX_AUTOMATION_SCHEDULER_EVALUATION_OCCURRENCES {
            return Err(ApplicationError::Unavailable(
                "automation scheduler evaluation produced too many occurrence decisions".into(),
            ));
        }
        occurrences.sort_by_key(|occurrence| occurrence.nominal_local);
        let next = snapshot
            .schedule
            .next_decision_after(&snapshot.timezone, through)?;
        cursor.advance(through, Some(next), observed_at)?;
        let command = evaluation_command(
            &self.ids.generate("automation-evaluation"),
            lease,
            automation.revision,
            expected_cursor_revision,
            &cursor,
            &occurrences,
        )?;
        let committed = self
            .store
            .commit_automation_schedule_evaluation(AutomationScheduleEvaluationCommit {
                lease: lease.clone(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision,
                cursor,
                occurrences,
                observed_at,
                command,
            })
            .await?;
        Ok(CandidateEvaluationSummary {
            initialized: false,
            occurrences: committed.occurrences.len(),
        })
    }

    fn materialize_decisions(
        &self,
        automation: &Automation,
        snapshot: &AutomationExecutionSnapshot,
        decisions: Vec<AutomationScheduleDecision>,
        continuity_started_at: UnixMillis,
        observed_at: UnixMillis,
    ) -> Result<Vec<AutomationOccurrence>, ApplicationError> {
        let mut gaps = Vec::new();
        let mut missed = Vec::new();
        let mut current = Vec::new();
        for decision in decisions {
            match decision {
                AutomationScheduleDecision::SkippedNonexistentLocalTime { .. } => {
                    gaps.push(decision);
                }
                AutomationScheduleDecision::Due { scheduled_for, .. }
                    if scheduled_for < continuity_started_at =>
                {
                    missed.push(decision);
                }
                AutomationScheduleDecision::Due { .. } => current.push(decision),
            }
        }
        let mut occurrences = Vec::with_capacity(3);
        for gap in gaps {
            occurrences.push(AutomationOccurrence::skipped_invalid_local_time(
                self.occurrence_id()?,
                automation.id.clone(),
                snapshot.clone(),
                gap,
                observed_at,
            )?);
        }
        if let Some(latest) = missed.last().copied() {
            let count = u32::try_from(missed.len()).map_err(|_| {
                ApplicationError::InvalidState("automation missed-run count is exhausted".into())
            })?;
            let mut occurrence = AutomationOccurrence::pending(
                self.occurrence_id()?,
                automation.id.clone(),
                snapshot.clone(),
                latest,
                count,
                observed_at,
            )?;
            if snapshot.missed_run_policy == MissedRunPolicy::Skip {
                occurrence.skip_missed(observed_at)?;
            }
            occurrences.push(occurrence);
        }
        for decision in current {
            occurrences.push(AutomationOccurrence::pending(
                self.occurrence_id()?,
                automation.id.clone(),
                snapshot.clone(),
                decision,
                1,
                observed_at,
            )?);
        }
        Ok(occurrences)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CandidateEvaluationSummary {
    initialized: bool,
    occurrences: usize,
}

fn evaluation_command(
    key: &str,
    lease: &AutomationSchedulerLeaseToken,
    automation_revision: u64,
    expected_cursor_revision: Option<u64>,
    cursor: &AutomationScheduleCursor,
    occurrences: &[AutomationOccurrence],
) -> Result<MutationCommand, ApplicationError> {
    let mut parts = vec![
        lease.owner_id.as_str().as_bytes().to_vec(),
        lease.fence.to_be_bytes().to_vec(),
        cursor.automation_id.as_str().as_bytes().to_vec(),
        automation_revision.to_be_bytes().to_vec(),
    ];
    push_optional_u64(&mut parts, expected_cursor_revision);
    parts.push(cursor.definition_revision.to_be_bytes().to_vec());
    parts.push(cursor.schedule_fingerprint.as_bytes().to_vec());
    parts.push(cursor.calculator_version.to_be_bytes().to_vec());
    parts.push(cursor.evaluated_through.to_be_bytes().to_vec());
    push_schedule_decision(&mut parts, cursor.next_decision);
    parts.push(cursor.revision.to_be_bytes().to_vec());
    parts.push(cursor.created_at.to_be_bytes().to_vec());
    parts.push(cursor.updated_at.to_be_bytes().to_vec());
    parts.push(
        u64::try_from(occurrences.len())
            .unwrap_or(u64::MAX)
            .to_be_bytes()
            .to_vec(),
    );
    for occurrence in occurrences {
        parts.push(occurrence.id.as_str().as_bytes().to_vec());
        parts.push(occurrence.automation_id.as_str().as_bytes().to_vec());
        parts.push(occurrence.snapshot.project_id.as_str().as_bytes().to_vec());
        parts.push(
            occurrence
                .snapshot
                .definition_revision
                .to_be_bytes()
                .to_vec(),
        );
        parts.push(occurrence.snapshot.title.as_bytes().to_vec());
        parts.push(occurrence.snapshot.prompt.as_bytes().to_vec());
        parts.push(occurrence.snapshot.canonical_schedule.as_bytes().to_vec());
        parts.push(occurrence.snapshot.timezone.as_bytes().to_vec());
        parts.push(vec![missed_run_policy_key(
            occurrence.snapshot.missed_run_policy,
        )]);
        parts.push(vec![overlap_policy_key(occurrence.snapshot.overlap_policy)]);
        parts.push(occurrence.snapshot.schedule_fingerprint.as_bytes().to_vec());
        parts.push(
            occurrence
                .snapshot
                .calculator_version
                .to_be_bytes()
                .to_vec(),
        );
        push_nominal_local(&mut parts, occurrence.nominal_local);
        push_optional_u64(&mut parts, occurrence.scheduled_for);
        parts.push(occurrence.occurrence_count.to_be_bytes().to_vec());
        parts.push(vec![occurrence_state_key(occurrence.state)]);
        parts.push(occurrence.claim_attempt_count.to_be_bytes().to_vec());
        parts.push(occurrence.revision.to_be_bytes().to_vec());
        parts.push(occurrence.created_at.to_be_bytes().to_vec());
        parts.push(occurrence.updated_at.to_be_bytes().to_vec());
    }
    let borrowed = parts.iter().map(Vec::as_slice).collect::<Vec<_>>();
    mutation_command_bytes("automation_scheduler_evaluate_v1", key, &borrowed)
}

fn push_schedule_decision(parts: &mut Vec<Vec<u8>>, decision: Option<AutomationScheduleDecision>) {
    match decision {
        None => parts.push(vec![0]),
        Some(AutomationScheduleDecision::Due {
            nominal_local,
            scheduled_for,
        }) => {
            parts.push(vec![1]);
            push_nominal_local(parts, nominal_local);
            parts.push(scheduled_for.to_be_bytes().to_vec());
        }
        Some(AutomationScheduleDecision::SkippedNonexistentLocalTime { nominal_local }) => {
            parts.push(vec![2]);
            push_nominal_local(parts, nominal_local);
        }
    }
}

fn push_nominal_local(parts: &mut Vec<Vec<u8>>, value: grok_domain::AutomationLocalDateTime) {
    parts.push(value.year.to_be_bytes().to_vec());
    parts.push(vec![value.month, value.day, value.hour, value.minute]);
}

fn push_optional_u64(parts: &mut Vec<Vec<u8>>, value: Option<u64>) {
    match value {
        Some(value) => {
            parts.push(vec![1]);
            parts.push(value.to_be_bytes().to_vec());
        }
        None => parts.push(vec![0]),
    }
}

const fn missed_run_policy_key(value: MissedRunPolicy) -> u8 {
    match value {
        MissedRunPolicy::RunOnce => 1,
        MissedRunPolicy::Skip => 2,
    }
}

const fn overlap_policy_key(value: grok_domain::OverlapPolicy) -> u8 {
    match value {
        grok_domain::OverlapPolicy::QueueOne => 1,
        grok_domain::OverlapPolicy::Skip => 2,
    }
}

const fn occurrence_state_key(value: AutomationOccurrenceState) -> u8 {
    match value {
        AutomationOccurrenceState::Pending => 0,
        AutomationOccurrenceState::QueuedOverlap => 1,
        AutomationOccurrenceState::Claimed => 2,
        AutomationOccurrenceState::RunLinked => 3,
        AutomationOccurrenceState::Succeeded => 4,
        AutomationOccurrenceState::Failed => 5,
        AutomationOccurrenceState::SkippedMissed => 6,
        AutomationOccurrenceState::SkippedOverlap => 7,
        AutomationOccurrenceState::SkippedInvalidLocalTime => 8,
        AutomationOccurrenceState::InterruptedNeedsReview => 9,
        AutomationOccurrenceState::Cancelled => 10,
    }
}

/// Returns whether a state occupies the one active occurrence slot.
#[must_use]
pub const fn automation_occurrence_is_active(state: AutomationOccurrenceState) -> bool {
    matches!(
        state,
        AutomationOccurrenceState::Pending
            | AutomationOccurrenceState::Claimed
            | AutomationOccurrenceState::RunLinked
    )
}
