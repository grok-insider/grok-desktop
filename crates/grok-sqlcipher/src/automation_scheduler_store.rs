use std::collections::HashSet;

use async_trait::async_trait;
use grok_application::{
    AutomationOccurrenceDispatch, AutomationOccurrenceDispatchResult,
    AutomationOccurrenceRunCompletion, AutomationScheduleCandidate,
    AutomationScheduleEvaluationCommit, AutomationScheduleEvaluationResult,
    AutomationSchedulerJournalStatus, AutomationSchedulerLeaseAcquisition,
    AutomationSchedulerRecoverySummary, AutomationSchedulerStore, ClaimAutomationOccurrence,
    MAX_AUTOMATION_SCHEDULER_EVALUATION_OCCURRENCES, MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH,
    MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS, MutationCommand, StoreError,
};
use grok_domain::{
    Automation, AutomationExecutionSnapshot, AutomationHistoryStatus, AutomationId,
    AutomationLocalDateTime, AutomationOccurrence, AutomationOccurrenceClaim,
    AutomationOccurrenceId, AutomationOccurrenceState, AutomationScheduleCursor,
    AutomationScheduleDecision, AutomationScheduleFingerprint, AutomationSchedulerLease,
    AutomationSchedulerLeaseToken, AutomationSchedulerOwnerId, AutomationState,
    ConversationThreadOrigin, MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS,
    MAX_AUTOMATION_SCHEDULE_DECISIONS, MAX_AUTOMATION_SCHEDULER_LEASE_MS, Message, MessageRole,
    MessageState, MissedRunPolicy, OverlapPolicy, ProjectId, ProjectState, RunEventKind, RunId,
    RunState, Thread, UnixMillis,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use crate::{SqlCipherStore, mapping};

const AUTOMATION_COLUMNS: &str = "id,project_id,title,prompt,schedule,timezone,missed_run_policy,overlap_policy,state,revision,created_at,updated_at";
const AUTOMATION_JOIN_COLUMNS: &str = "automation.id,automation.project_id,automation.title,automation.prompt,automation.schedule,automation.timezone,automation.missed_run_policy,automation.overlap_policy,automation.state,automation.revision,automation.created_at,automation.updated_at";
const CURSOR_COLUMNS: &str = "automation_id,definition_revision,schedule_fingerprint,calculator_version,evaluated_through,\
     next_kind,next_year,next_month,next_day,next_hour,next_minute,next_scheduled_for,\
     revision,created_at,updated_at";
const OCCURRENCE_COLUMNS: &str = "id,automation_id,definition_revision,snapshot_project_id,snapshot_title,snapshot_prompt,\
     canonical_schedule,timezone,missed_run_policy,overlap_policy,schedule_fingerprint,\
     calculator_version,nominal_year,nominal_month,nominal_day,nominal_hour,nominal_minute,\
     scheduled_for,occurrence_count,state,claim_owner_id,claim_fence,claimed_at,claim_expires_at,\
     run_id,claim_attempt_count,revision,created_at,updated_at";
const THREAD_COLUMNS: &str = "id,project_id,title,state,revision,created_at,updated_at,\
    (SELECT parent_thread_id FROM conversation_thread_forks WHERE child_thread_id=threads.id),\
    (SELECT source_turn_id FROM conversation_thread_forks WHERE child_thread_id=threads.id),\
    (SELECT source_message_id FROM conversation_thread_forks WHERE child_thread_id=threads.id),\
    (SELECT kind FROM conversation_thread_forks WHERE child_thread_id=threads.id),\
    (SELECT root_thread_id FROM conversation_thread_forks WHERE child_thread_id=threads.id),\
    (SELECT fork_depth FROM conversation_thread_forks WHERE child_thread_id=threads.id)";
const MESSAGE_COLUMNS: &str = "id,thread_id,sequence,role,content,state,revision,created_at,updated_at,\
    (SELECT kind FROM conversation_message_derivations WHERE child_message_id=messages.id),\
    (SELECT source_message_id FROM conversation_message_derivations WHERE child_message_id=messages.id),\
    (SELECT source_turn_id FROM conversation_message_derivations WHERE child_message_id=messages.id),\
    (SELECT source_context_sequence FROM conversation_message_derivations WHERE child_message_id=messages.id)";
const RUN_COLUMNS: &str = "id,project_id,thread_id,state,revision,created_at,updated_at";

#[async_trait]
impl AutomationSchedulerStore for SqlCipherStore {
    async fn acquire_automation_scheduler_lease(
        &self,
        owner_id: &AutomationSchedulerOwnerId,
        now: UnixMillis,
        ttl_ms: u64,
    ) -> Result<AutomationSchedulerLeaseAcquisition, StoreError> {
        let owner_id = owner_id.clone();
        self.with_store(move |connection| acquire_lease(connection, owner_id, now, ttl_ms))
            .await
    }

    async fn list_automation_schedule_candidates(
        &self,
        after: Option<&AutomationId>,
        limit: usize,
    ) -> Result<Vec<AutomationScheduleCandidate>, StoreError> {
        let after = after.map(ToString::to_string);
        self.with_store(move |connection| list_candidates(connection, after.as_deref(), limit))
            .await
    }

    async fn commit_automation_schedule_evaluation(
        &self,
        evaluation: AutomationScheduleEvaluationCommit,
    ) -> Result<AutomationScheduleEvaluationResult, StoreError> {
        self.with_store(move |connection| commit_evaluation(connection, evaluation))
            .await
    }

    async fn get_automation_occurrence(
        &self,
        id: &AutomationOccurrenceId,
    ) -> Result<AutomationOccurrence, StoreError> {
        let id = id.to_string();
        self.with_store(move |connection| query_occurrence(connection, &id))
            .await
    }

    async fn list_automation_occurrences(
        &self,
        automation_id: &AutomationId,
        after: Option<&AutomationOccurrenceId>,
        limit: usize,
    ) -> Result<Vec<AutomationOccurrence>, StoreError> {
        let automation_id = automation_id.to_string();
        let after = after.map(ToString::to_string);
        self.with_store(move |connection| {
            list_occurrences(connection, &automation_id, after.as_deref(), limit)
        })
        .await
    }

    async fn claim_automation_occurrence(
        &self,
        claim: ClaimAutomationOccurrence,
    ) -> Result<AutomationOccurrence, StoreError> {
        self.with_store(move |connection| claim_occurrence(connection, &claim))
            .await
    }

    async fn claim_and_bind_automation_occurrence(
        &self,
        dispatch: AutomationOccurrenceDispatch,
    ) -> Result<AutomationOccurrenceDispatchResult, StoreError> {
        self.with_store(move |connection| claim_and_bind_occurrence(connection, dispatch))
            .await
    }

    async fn list_resumable_automation_dispatches(
        &self,
        after: Option<&AutomationOccurrenceId>,
        limit: usize,
    ) -> Result<Vec<AutomationOccurrenceDispatchResult>, StoreError> {
        let after = after.map(ToString::to_string);
        self.with_store(move |connection| {
            list_resumable_dispatches(connection, after.as_deref(), limit)
        })
        .await
    }

    async fn begin_automation_occurrence_run(
        &self,
        occurrence_id: &AutomationOccurrenceId,
        expected_occurrence_revision: u64,
        run_id: &RunId,
        expected_run_revision: u64,
        now: UnixMillis,
    ) -> Result<AutomationOccurrenceDispatchResult, StoreError> {
        let occurrence_id = occurrence_id.clone();
        let run_id = run_id.clone();
        self.with_store(move |connection| {
            begin_occurrence_run(
                connection,
                &occurrence_id,
                expected_occurrence_revision,
                &run_id,
                expected_run_revision,
                now,
            )
        })
        .await
    }

    async fn complete_automation_occurrence_run(
        &self,
        occurrence_id: &AutomationOccurrenceId,
        expected_revision: u64,
        run_id: &RunId,
        completion: AutomationOccurrenceRunCompletion,
        now: UnixMillis,
    ) -> Result<AutomationOccurrence, StoreError> {
        let occurrence_id = occurrence_id.clone();
        let run_id = run_id.clone();
        self.with_store(move |connection| {
            complete_occurrence_run(
                connection,
                &occurrence_id,
                expected_revision,
                &run_id,
                completion,
                now,
            )
        })
        .await
    }

    async fn recover_automation_occurrence_claims(
        &self,
        lease: &AutomationSchedulerLeaseToken,
        now: UnixMillis,
        limit: usize,
    ) -> Result<AutomationSchedulerRecoverySummary, StoreError> {
        let lease = lease.clone();
        self.with_store(move |connection| recover_claims(connection, &lease, now, limit))
            .await
    }

    async fn automation_scheduler_journal_status(
        &self,
    ) -> Result<AutomationSchedulerJournalStatus, StoreError> {
        self.with_store(journal_status).await
    }

    async fn link_automation_occurrence_run(
        &self,
        lease: &AutomationSchedulerLeaseToken,
        occurrence_id: &AutomationOccurrenceId,
        expected_revision: u64,
        run_id: RunId,
        now: UnixMillis,
    ) -> Result<AutomationOccurrence, StoreError> {
        let lease = lease.clone();
        let occurrence_id = occurrence_id.clone();
        self.with_store(move |connection| {
            link_occurrence_run(
                connection,
                &lease,
                &occurrence_id,
                expected_revision,
                run_id,
                now,
            )
        })
        .await
    }
}

fn acquire_lease(
    connection: &mut Connection,
    owner_id: AutomationSchedulerOwnerId,
    now: UnixMillis,
    ttl_ms: u64,
) -> Result<AutomationSchedulerLeaseAcquisition, StoreError> {
    if ttl_ms == 0 || ttl_ms > MAX_AUTOMATION_SCHEDULER_LEASE_MS {
        return Err(StoreError::Conflict);
    }
    let transaction = begin(connection)?;
    let durable_floor = scheduler_durable_clock_floor(&transaction)?;
    if let Some(floor) = durable_floor
        && now < floor
    {
        return Ok(AutomationSchedulerLeaseAcquisition::ClockRegressed {
            durable_floor: floor,
        });
    }
    let current = query_lease(&transaction)?;
    let (lease, continuous, continuity_started_at) = match current {
        None => (
            AutomationSchedulerLease::acquire(owner_id, 1, now, ttl_ms)
                .map_err(|_| StoreError::Conflict)?,
            false,
            now,
        ),
        Some(lease) if now < lease.renewed_at => {
            return Ok(AutomationSchedulerLeaseAcquisition::ClockRegressed {
                durable_floor: lease.renewed_at,
            });
        }
        Some(mut lease) if lease.owner_id == owner_id && now < lease.expires_at => {
            let continuity_started_at = lease.renewed_at;
            let token = lease.token();
            lease
                .renew(&token, now, ttl_ms)
                .map_err(|_| StoreError::Conflict)?;
            (lease, true, continuity_started_at)
        }
        Some(lease) if now < lease.expires_at => {
            return Ok(AutomationSchedulerLeaseAcquisition::Busy { lease });
        }
        Some(mut lease) => {
            let fence = lease.fence.checked_add(1).ok_or(StoreError::Conflict)?;
            lease
                .take_over(owner_id, fence, now, ttl_ms)
                .map_err(|_| StoreError::Conflict)?;
            (lease, false, now)
        }
    };
    if current_is_absent(&transaction)? {
        transaction
            .execute(
                "INSERT INTO automation_scheduler_lease(
                     singleton,owner_id,fence,acquired_at,renewed_at,expires_at
                 ) VALUES (1,?1,?2,?3,?4,?5)",
                params![
                    lease.owner_id.as_str(),
                    number(lease.fence)?,
                    number(lease.acquired_at)?,
                    number(lease.renewed_at)?,
                    number(lease.expires_at)?,
                ],
            )
            .map_err(map_sqlite)?;
    } else {
        let changed = transaction
            .execute(
                "UPDATE automation_scheduler_lease
                 SET owner_id=?1,fence=?2,acquired_at=?3,renewed_at=?4,expires_at=?5
                 WHERE singleton=1",
                params![
                    lease.owner_id.as_str(),
                    number(lease.fence)?,
                    number(lease.acquired_at)?,
                    number(lease.renewed_at)?,
                    number(lease.expires_at)?,
                ],
            )
            .map_err(map_sqlite)?;
        ensure_one(changed)?;
    }
    transaction.commit().map_err(map_sqlite)?;
    Ok(AutomationSchedulerLeaseAcquisition::Acquired {
        lease,
        continuous,
        continuity_started_at,
    })
}

fn current_is_absent(transaction: &Transaction<'_>) -> Result<bool, StoreError> {
    transaction
        .query_row(
            "SELECT NOT EXISTS(SELECT 1 FROM automation_scheduler_lease WHERE singleton=1)",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite)
}

fn scheduler_durable_clock_floor(
    connection: &Connection,
) -> Result<Option<UnixMillis>, StoreError> {
    let floor = connection
        .query_row(
            "SELECT MAX(value) FROM (
                 SELECT renewed_at AS value FROM automation_scheduler_lease
                 UNION ALL SELECT evaluated_through FROM automation_schedule_cursors
                 UNION ALL SELECT updated_at FROM automation_schedule_cursors
                 UNION ALL SELECT updated_at FROM automation_occurrences
                 UNION ALL SELECT claimed_at FROM automation_occurrence_claim_attempts
                 UNION ALL SELECT completed_at FROM automation_occurrence_claim_attempts
                 WHERE completed_at IS NOT NULL
             )",
            [],
            |row| row.get::<_, Option<i64>>(0),
        )
        .map_err(map_sqlite)?;
    floor
        .map(u64::try_from)
        .transpose()
        .map_err(|_| StoreError::Internal("negative scheduler clock floor".into()))
}

fn list_candidates(
    connection: &Connection,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<AutomationScheduleCandidate>, StoreError> {
    if limit == 0 || limit > MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS.saturating_add(1) {
        return Err(StoreError::Conflict);
    }
    let mut statement = connection
        .prepare(&format!(
            "SELECT {AUTOMATION_JOIN_COLUMNS},
                    cursor.automation_id,cursor.definition_revision,cursor.schedule_fingerprint,
                    cursor.calculator_version,cursor.evaluated_through,cursor.next_kind,
                    cursor.next_year,cursor.next_month,cursor.next_day,cursor.next_hour,
                    cursor.next_minute,cursor.next_scheduled_for,cursor.revision,
                    cursor.created_at,cursor.updated_at
             FROM automations automation
             JOIN projects project ON project.id=automation.project_id
             LEFT JOIN automation_schedule_cursors cursor ON cursor.automation_id=automation.id
             WHERE automation.state=?1 AND project.state=?2
               AND (?3 IS NULL OR automation.id>?3)
             ORDER BY automation.id LIMIT ?4"
        ))
        .map_err(map_sqlite)?;
    let rows = statement
        .query_map(
            params![
                mapping::automation_state_to_i64(AutomationState::Enabled),
                mapping::project_state_to_i64(ProjectState::Active),
                after,
                sql_limit(limit),
            ],
            |row| {
                let automation = Automation::restore(mapping::automation_from_row(row)?)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?;
                let cursor_id = row.get::<_, Option<String>>(12)?;
                let cursor = cursor_id.map(|_| cursor_from_row(row, 12)).transpose()?;
                if cursor
                    .as_ref()
                    .is_some_and(|cursor| validate_cursor_binding(cursor, &automation).is_err())
                {
                    return Err(rusqlite::Error::InvalidQuery);
                }
                Ok(AutomationScheduleCandidate { automation, cursor })
            },
        )
        .map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

#[allow(clippy::too_many_lines)]
fn commit_evaluation(
    connection: &mut Connection,
    evaluation: AutomationScheduleEvaluationCommit,
) -> Result<AutomationScheduleEvaluationResult, StoreError> {
    validate_command(&evaluation.command, "automation_scheduler_evaluate_v1")?;
    if evaluation.occurrences.len() > MAX_AUTOMATION_SCHEDULER_EVALUATION_OCCURRENCES {
        return Err(StoreError::Conflict);
    }
    AutomationScheduleCursor::restore(evaluation.cursor.clone())
        .map_err(|_| StoreError::Conflict)?;
    let transaction = begin(connection)?;
    if let Some((fingerprint, automation_id)) = transaction
        .query_row(
            "SELECT request_fingerprint,automation_id
             FROM automation_schedule_evaluation_commands
             WHERE command_scope=?1 AND idempotency_key=?2",
            params![evaluation.command.scope, evaluation.command.key],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?
    {
        if fingerprint != evaluation.command.fingerprint
            || automation_id != evaluation.cursor.automation_id.as_str()
        {
            return Err(StoreError::Conflict);
        }
        return query_evaluation_result(
            &transaction,
            &evaluation.command.scope,
            &evaluation.command.key,
        );
    }

    ensure_live_lease(
        &transaction,
        &evaluation.lease,
        evaluation.observed_at,
        None,
    )?;
    let automation = query_automation(&transaction, evaluation.cursor.automation_id.as_str())?;
    if automation.state != AutomationState::Enabled
        || automation.revision != evaluation.expected_automation_revision
        || evaluation.cursor.definition_revision != automation.revision
    {
        return Err(StoreError::Conflict);
    }
    let project_state = transaction
        .query_row(
            "SELECT state FROM projects WHERE id=?1",
            [automation.project_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    if project_state != Some(mapping::project_state_to_i64(ProjectState::Active)) {
        return Err(StoreError::Conflict);
    }
    validate_occurrence_cardinality(&transaction, Some(automation.id.as_str()))?;
    let snapshot = AutomationExecutionSnapshot::new(
        automation.revision,
        automation.project_id.clone(),
        automation.title.clone(),
        automation.prompt.clone(),
        automation.schedule.clone(),
        automation.timezone.clone(),
        automation.missed_run_policy,
        automation.overlap_policy,
    )
    .map_err(|_| StoreError::Conflict)?;
    if evaluation.cursor.schedule_fingerprint != snapshot.schedule_fingerprint
        || evaluation.cursor.calculator_version != snapshot.calculator_version
        || evaluation.cursor.updated_at != evaluation.observed_at
        || evaluation.cursor.next_decision
            != snapshot
                .schedule
                .next_decision_after(&snapshot.timezone, evaluation.cursor.evaluated_through)
                .ok()
    {
        return Err(StoreError::Conflict);
    }

    let prior_cursor = query_cursor_optional(&transaction, automation.id.as_str())?;
    if prior_cursor
        .as_ref()
        .is_some_and(|cursor| validate_cursor_binding(cursor, &automation).is_err())
    {
        return Err(StoreError::Internal(
            "invalid persisted automation schedule cursor".into(),
        ));
    }
    if evaluation.expected_cursor_revision != prior_cursor.as_ref().map(|cursor| cursor.revision) {
        return Err(StoreError::Conflict);
    }
    match &prior_cursor {
        None => {
            if evaluation.cursor.revision != 0
                || evaluation.cursor.created_at != evaluation.observed_at
                || evaluation.cursor.evaluated_through != automation.updated_at
                || !evaluation.occurrences.is_empty()
            {
                return Err(StoreError::Conflict);
            }
        }
        Some(prior) => {
            if evaluation.cursor.revision
                != prior.revision.checked_add(1).ok_or(StoreError::Conflict)?
                || evaluation.cursor.created_at != prior.created_at
                || evaluation.cursor.evaluated_through < prior.evaluated_through
                || evaluation.cursor.updated_at < prior.updated_at
                || evaluation.cursor.definition_revision != prior.definition_revision
                || evaluation.cursor.schedule_fingerprint != prior.schedule_fingerprint
                || evaluation.cursor.calculator_version != prior.calculator_version
            {
                return Err(StoreError::Conflict);
            }
        }
    }
    validate_proposed_occurrences(
        &evaluation,
        &snapshot,
        prior_cursor.as_ref().map(|cursor| cursor.evaluated_through),
    )?;

    let next = DecisionColumns::from(evaluation.cursor.next_decision);
    transaction
        .execute(
            "INSERT INTO automation_schedule_evaluation_commands(
                 command_scope,idempotency_key,request_fingerprint,owner_id,fence,
                 automation_id,definition_revision,schedule_fingerprint,
                 expected_cursor_revision,result_cursor_revision,prior_evaluated_through,
                 evaluated_through,next_kind,next_year,next_month,next_day,next_hour,next_minute,
                 next_scheduled_for,result_updated_at,created_at
             ) VALUES (
                 ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,
                 ?19,?20,?21
             )",
            params![
                evaluation.command.scope,
                evaluation.command.key,
                evaluation.command.fingerprint.as_slice(),
                evaluation.lease.owner_id.as_str(),
                number(evaluation.lease.fence)?,
                automation.id.as_str(),
                number(evaluation.cursor.definition_revision)?,
                evaluation.cursor.schedule_fingerprint.as_bytes().as_slice(),
                optional_number(evaluation.expected_cursor_revision)?,
                number(evaluation.cursor.revision)?,
                prior_cursor
                    .as_ref()
                    .map(|cursor| cursor.evaluated_through)
                    .map(number)
                    .transpose()?,
                number(evaluation.cursor.evaluated_through)?,
                next.kind,
                next.year,
                next.month,
                next.day,
                next.hour,
                next.minute,
                optional_number(next.scheduled_for)?,
                number(evaluation.cursor.updated_at)?,
                number(evaluation.observed_at)?,
            ],
        )
        .map_err(map_sqlite)?;
    persist_cursor(&transaction, &evaluation.cursor, prior_cursor.is_some())?;

    let mut committed = Vec::with_capacity(evaluation.occurrences.len());
    for mut occurrence in evaluation.occurrences {
        adjudicate_overlap(&transaction, &mut occurrence, evaluation.observed_at)?;
        insert_occurrence(
            &transaction,
            &occurrence,
            &evaluation.command.scope,
            &evaluation.command.key,
        )?;
        append_skip_history(&transaction, &occurrence, evaluation.observed_at)?;
        committed.push(occurrence);
    }
    transaction.commit().map_err(map_sqlite)?;
    Ok(AutomationScheduleEvaluationResult {
        cursor: evaluation.cursor,
        occurrences: committed,
    })
}

fn validate_proposed_occurrences(
    evaluation: &AutomationScheduleEvaluationCommit,
    snapshot: &AutomationExecutionSnapshot,
    prior_evaluated_through: Option<UnixMillis>,
) -> Result<(), StoreError> {
    let allowed_slots = if let Some(prior_evaluated_through) = prior_evaluated_through {
        let calculation = snapshot
            .schedule
            .decisions_between(
                &snapshot.timezone,
                prior_evaluated_through,
                evaluation.cursor.evaluated_through,
                MAX_AUTOMATION_SCHEDULE_DECISIONS,
            )
            .map_err(|_| StoreError::Conflict)?;
        if calculation.truncated {
            return Err(StoreError::Conflict);
        }
        calculation
            .decisions
            .into_iter()
            .map(AutomationScheduleDecision::nominal_local)
            .collect::<HashSet<_>>()
    } else {
        HashSet::new()
    };
    let mut prior_slot = None;
    for occurrence in &evaluation.occurrences {
        AutomationOccurrence::restore(occurrence.clone()).map_err(|_| StoreError::Conflict)?;
        if occurrence.automation_id != evaluation.cursor.automation_id
            || occurrence.snapshot != *snapshot
            || occurrence.created_at != evaluation.observed_at
            || occurrence.updated_at != evaluation.observed_at
            || !matches!(
                occurrence.state,
                AutomationOccurrenceState::Pending
                    | AutomationOccurrenceState::SkippedMissed
                    | AutomationOccurrenceState::SkippedInvalidLocalTime
            )
            || occurrence.claim.is_some()
            || occurrence.run_id.is_some()
            || occurrence.claim_attempt_count != 0
            || usize::try_from(occurrence.occurrence_count)
                .map_or(true, |count| count > MAX_AUTOMATION_SCHEDULE_DECISIONS)
            || !allowed_slots.contains(&occurrence.nominal_local)
            || (occurrence.state == AutomationOccurrenceState::Pending && occurrence.revision != 0)
            || (occurrence.state == AutomationOccurrenceState::SkippedMissed
                && (occurrence.revision != 1
                    || snapshot.missed_run_policy != MissedRunPolicy::Skip))
            || (occurrence.state == AutomationOccurrenceState::SkippedInvalidLocalTime
                && occurrence.revision != 0)
            || (occurrence.occurrence_count > 1
                && snapshot.missed_run_policy == MissedRunPolicy::Skip
                && occurrence.state != AutomationOccurrenceState::SkippedMissed)
            || occurrence
                .scheduled_for
                .is_some_and(|scheduled_for| scheduled_for > evaluation.cursor.evaluated_through)
        {
            return Err(StoreError::Conflict);
        }
        let slot = occurrence.slot();
        let key = (
            slot.definition_revision,
            slot.nominal_local.year,
            slot.nominal_local.month,
            slot.nominal_local.day,
            slot.nominal_local.hour,
            slot.nominal_local.minute,
        );
        if prior_slot.is_some_and(|prior| prior >= key) {
            return Err(StoreError::Conflict);
        }
        prior_slot = Some(key);
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct DecisionColumns {
    kind: Option<i64>,
    year: Option<i64>,
    month: Option<i64>,
    day: Option<i64>,
    hour: Option<i64>,
    minute: Option<i64>,
    scheduled_for: Option<u64>,
}

impl From<Option<AutomationScheduleDecision>> for DecisionColumns {
    fn from(decision: Option<AutomationScheduleDecision>) -> Self {
        let Some(decision) = decision else {
            return Self {
                kind: None,
                year: None,
                month: None,
                day: None,
                hour: None,
                minute: None,
                scheduled_for: None,
            };
        };
        let nominal = decision.nominal_local();
        Self {
            kind: Some(match decision {
                AutomationScheduleDecision::Due { .. } => 0,
                AutomationScheduleDecision::SkippedNonexistentLocalTime { .. } => 1,
            }),
            year: Some(i64::from(nominal.year)),
            month: Some(i64::from(nominal.month)),
            day: Some(i64::from(nominal.day)),
            hour: Some(i64::from(nominal.hour)),
            minute: Some(i64::from(nominal.minute)),
            scheduled_for: decision.scheduled_for(),
        }
    }
}

fn persist_cursor(
    transaction: &Transaction<'_>,
    cursor: &AutomationScheduleCursor,
    exists: bool,
) -> Result<(), StoreError> {
    let next = DecisionColumns::from(cursor.next_decision);
    let changed = if exists {
        transaction.execute(
            "UPDATE automation_schedule_cursors
             SET evaluated_through=?1,next_kind=?2,next_year=?3,next_month=?4,next_day=?5,
                 next_hour=?6,next_minute=?7,next_scheduled_for=?8,revision=?9,updated_at=?10
             WHERE automation_id=?11",
            params![
                number(cursor.evaluated_through)?,
                next.kind,
                next.year,
                next.month,
                next.day,
                next.hour,
                next.minute,
                optional_number(next.scheduled_for)?,
                number(cursor.revision)?,
                number(cursor.updated_at)?,
                cursor.automation_id.as_str(),
            ],
        )
    } else {
        transaction.execute(
            "INSERT INTO automation_schedule_cursors(
                 automation_id,definition_revision,schedule_fingerprint,calculator_version,
                 evaluated_through,next_kind,next_year,next_month,next_day,next_hour,next_minute,
                 next_scheduled_for,revision,created_at,updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            params![
                cursor.automation_id.as_str(),
                number(cursor.definition_revision)?,
                cursor.schedule_fingerprint.as_bytes().as_slice(),
                i64::from(cursor.calculator_version),
                number(cursor.evaluated_through)?,
                next.kind,
                next.year,
                next.month,
                next.day,
                next.hour,
                next.minute,
                optional_number(next.scheduled_for)?,
                number(cursor.revision)?,
                number(cursor.created_at)?,
                number(cursor.updated_at)?,
            ],
        )
    }
    .map_err(map_sqlite)?;
    ensure_one(changed)
}

fn adjudicate_overlap(
    transaction: &Transaction<'_>,
    occurrence: &mut AutomationOccurrence,
    now: UnixMillis,
) -> Result<(), StoreError> {
    if occurrence.state != AutomationOccurrenceState::Pending {
        return Ok(());
    }
    let active: bool = transaction
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM automation_occurrences
                 WHERE automation_id=?1 AND state IN (0,2,3)
             )",
            [occurrence.automation_id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if !active {
        return Ok(());
    }
    match occurrence.snapshot.overlap_policy {
        OverlapPolicy::Skip => occurrence
            .skip_overlap(now)
            .map_err(|_| StoreError::Conflict),
        OverlapPolicy::QueueOne => {
            let queued: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM automation_occurrences
                         WHERE automation_id=?1 AND state=1
                     )",
                    [occurrence.automation_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if queued {
                occurrence
                    .skip_overlap(now)
                    .map_err(|_| StoreError::Conflict)
            } else {
                occurrence
                    .queue_overlap(now)
                    .map_err(|_| StoreError::Conflict)
            }
        }
    }
}

fn insert_occurrence(
    transaction: &Transaction<'_>,
    occurrence: &AutomationOccurrence,
    evaluation_scope: &str,
    evaluation_key: &str,
) -> Result<(), StoreError> {
    let claim = occurrence.claim.as_ref();
    transaction
        .execute(
            "INSERT INTO automation_occurrences(
                 id,automation_id,evaluation_scope,evaluation_key,definition_revision,
                 snapshot_project_id,snapshot_title,snapshot_prompt,canonical_schedule,timezone,
                 missed_run_policy,overlap_policy,schedule_fingerprint,calculator_version,
                 nominal_year,nominal_month,nominal_day,nominal_hour,nominal_minute,scheduled_for,
                 occurrence_count,initial_state,initial_revision,state,claim_owner_id,claim_fence,
                 claimed_at,claim_expires_at,run_id,claim_attempt_count,revision,created_at,updated_at
             ) VALUES (
                 ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,
                 ?18,?19,?20,?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33
             )",
            params![
                occurrence.id.as_str(),
                occurrence.automation_id.as_str(),
                evaluation_scope,
                evaluation_key,
                number(occurrence.snapshot.definition_revision)?,
                occurrence.snapshot.project_id.as_str(),
                occurrence.snapshot.title,
                occurrence.snapshot.prompt,
                occurrence.snapshot.canonical_schedule,
                occurrence.snapshot.timezone,
                mapping::missed_run_policy_to_i64(occurrence.snapshot.missed_run_policy),
                mapping::overlap_policy_to_i64(occurrence.snapshot.overlap_policy),
                occurrence.snapshot.schedule_fingerprint.as_bytes().as_slice(),
                i64::from(occurrence.snapshot.calculator_version),
                i64::from(occurrence.nominal_local.year),
                i64::from(occurrence.nominal_local.month),
                i64::from(occurrence.nominal_local.day),
                i64::from(occurrence.nominal_local.hour),
                i64::from(occurrence.nominal_local.minute),
                optional_number(occurrence.scheduled_for)?,
                i64::from(occurrence.occurrence_count),
                occurrence_state_to_i64(occurrence.state),
                number(occurrence.revision)?,
                occurrence_state_to_i64(occurrence.state),
                claim.map(|claim| claim.owner_id.as_str()),
                claim.map(|claim| claim.fence).map(number).transpose()?,
                claim.map(|claim| claim.claimed_at).map(number).transpose()?,
                claim.map(|claim| claim.expires_at).map(number).transpose()?,
                occurrence.run_id.as_ref().map(RunId::as_str),
                i64::from(occurrence.claim_attempt_count),
                number(occurrence.revision)?,
                number(occurrence.created_at)?,
                number(occurrence.updated_at)?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn append_skip_history(
    transaction: &Transaction<'_>,
    occurrence: &AutomationOccurrence,
    recorded_at: UnixMillis,
) -> Result<(), StoreError> {
    let (status, summary) = match occurrence.state {
        AutomationOccurrenceState::SkippedMissed => (
            AutomationHistoryStatus::SkippedMissed,
            "Skipped by missed-run policy.",
        ),
        AutomationOccurrenceState::SkippedOverlap => (
            AutomationHistoryStatus::SkippedOverlap,
            "Skipped by overlap policy.",
        ),
        _ => return Ok(()),
    };
    let scheduled_for = occurrence.scheduled_for.ok_or(StoreError::Conflict)?;
    let existing = transaction
        .query_row(
            "SELECT recorded_at,status,summary FROM automation_history
             WHERE automation_id=?1 AND scheduled_for=?2",
            params![occurrence.automation_id.as_str(), number(scheduled_for)?],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?;
    let expected = (
        number(recorded_at)?,
        mapping::automation_history_status_to_i64(status),
        summary.to_owned(),
    );
    if let Some(existing) = existing {
        return if existing == expected {
            Ok(())
        } else {
            Err(StoreError::Conflict)
        };
    }
    let sequence: i64 = transaction
        .query_row(
            "SELECT COALESCE(MAX(sequence),0)+1 FROM automation_history
             WHERE automation_id=?1",
            [occurrence.automation_id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    transaction
        .execute(
            "INSERT INTO automation_history(
                 automation_id,sequence,scheduled_for,recorded_at,status,summary
             ) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                occurrence.automation_id.as_str(),
                sequence,
                number(scheduled_for)?,
                number(recorded_at)?,
                mapping::automation_history_status_to_i64(status),
                summary,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn list_occurrences(
    connection: &Connection,
    automation_id: &str,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<AutomationOccurrence>, StoreError> {
    let exists: bool = connection
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM automations WHERE id=?1)",
            [automation_id],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if !exists {
        return Err(StoreError::NotFound);
    }
    validate_occurrence_cardinality(connection, Some(automation_id))?;
    let cursor = after
        .map(|id| {
            connection
                .query_row(
                    "SELECT definition_revision,nominal_year,nominal_month,nominal_day,
                            nominal_hour,nominal_minute,id
                     FROM automation_occurrences WHERE id=?1 AND automation_id=?2",
                    params![id, automation_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, i64>(4)?,
                            row.get::<_, i64>(5)?,
                            row.get::<_, String>(6)?,
                        ))
                    },
                )
                .optional()
                .map_err(map_sqlite)?
                .ok_or(StoreError::NotFound)
        })
        .transpose()?;
    if limit == 0 || limit > MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS {
        return Err(StoreError::Conflict);
    }
    let (revision, year, month, day, hour, minute, id) =
        cursor.map_or((None, None, None, None, None, None, None), |value| {
            (
                Some(value.0),
                Some(value.1),
                Some(value.2),
                Some(value.3),
                Some(value.4),
                Some(value.5),
                Some(value.6),
            )
        });
    let mut statement = connection
        .prepare(&format!(
            "SELECT {OCCURRENCE_COLUMNS} FROM automation_occurrences
             WHERE automation_id=?1 AND (
                 ?2 IS NULL OR
                 (definition_revision,nominal_year,nominal_month,nominal_day,
                  nominal_hour,nominal_minute,id) > (?2,?3,?4,?5,?6,?7,?8)
             )
             ORDER BY definition_revision,nominal_year,nominal_month,nominal_day,
                      nominal_hour,nominal_minute,id
             LIMIT ?9"
        ))
        .map_err(map_sqlite)?;
    let rows = statement
        .query_map(
            params![
                automation_id,
                revision,
                year,
                month,
                day,
                hour,
                minute,
                id,
                sql_limit(limit),
            ],
            |row| occurrence_from_row(row, 0),
        )
        .map_err(map_sqlite)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)
}

fn claim_occurrence(
    connection: &mut Connection,
    claim: &ClaimAutomationOccurrence,
) -> Result<AutomationOccurrence, StoreError> {
    validate_command(&claim.command, "automation_scheduler_claim_v1")?;
    if claim.expires_at <= claim.claimed_at
        || claim.expires_at - claim.claimed_at > MAX_AUTOMATION_SCHEDULER_LEASE_MS
    {
        return Err(StoreError::Conflict);
    }
    let transaction = begin(connection)?;
    if let Some((fingerprint, occurrence_id)) = transaction
        .query_row(
            "SELECT request_fingerprint,occurrence_id
             FROM automation_occurrence_claim_attempts
             WHERE command_scope=?1 AND idempotency_key=?2",
            params![claim.command.scope, claim.command.key],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?
    {
        if fingerprint != claim.command.fingerprint || occurrence_id != claim.occurrence_id.as_str()
        {
            return Err(StoreError::Conflict);
        }
        return query_claim_result(&transaction, &claim.command.scope, &claim.command.key);
    }
    ensure_live_lease(
        &transaction,
        &claim.lease,
        claim.claimed_at,
        Some(claim.expires_at),
    )?;
    let mut occurrence = query_occurrence(&transaction, claim.occurrence_id.as_str())?;
    if occurrence.revision != claim.expected_revision {
        return Err(StoreError::Conflict);
    }
    occurrence
        .claim(&claim.lease, claim.claimed_at, claim.expires_at)
        .map_err(|_| StoreError::Conflict)?;
    update_occurrence(&transaction, &occurrence)?;
    let changed = transaction
        .execute(
            "INSERT INTO automation_occurrence_claim_attempts(
                 occurrence_id,sequence,command_scope,idempotency_key,request_fingerprint,
                 owner_id,fence,claimed_at,expires_at,completed_at,outcome,
                 result_occurrence_revision
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL,NULL,?10)",
            params![
                occurrence.id.as_str(),
                i64::from(occurrence.claim_attempt_count),
                claim.command.scope,
                claim.command.key,
                claim.command.fingerprint.as_slice(),
                claim.lease.owner_id.as_str(),
                number(claim.lease.fence)?,
                number(claim.claimed_at)?,
                number(claim.expires_at)?,
                number(occurrence.revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    ensure_one(changed)?;
    transaction.commit().map_err(map_sqlite)?;
    Ok(occurrence)
}

#[allow(clippy::too_many_lines)]
fn claim_and_bind_occurrence(
    connection: &mut Connection,
    mut dispatch: AutomationOccurrenceDispatch,
) -> Result<AutomationOccurrenceDispatchResult, StoreError> {
    let claim = &dispatch.claim;
    validate_command(&claim.command, "automation_scheduler_claim_v1")?;
    if claim.expires_at <= claim.claimed_at
        || claim.expires_at - claim.claimed_at > MAX_AUTOMATION_SCHEDULER_LEASE_MS
    {
        return Err(StoreError::Conflict);
    }
    dispatch.prompt.sequence = 1;
    if Thread::restore(dispatch.thread.clone()).is_err()
        || !matches!(
            dispatch.thread.lineage.origin,
            ConversationThreadOrigin::Original
        )
        || Message::restore(dispatch.prompt.clone()).is_err()
        || !dispatch.prompt.derivation.is_original()
        || dispatch.prompt.role != MessageRole::User
        || dispatch.prompt.state != MessageState::Active
        || dispatch.prompt.thread_id != dispatch.thread.id
        || dispatch.run.project_id != dispatch.thread.project_id
        || dispatch.run.thread_id != dispatch.thread.id
        || dispatch.run.state != RunState::Queued
        || dispatch.run.revision != 0
        || dispatch.thread.created_at != claim.claimed_at
        || dispatch.thread.updated_at != claim.claimed_at
        || dispatch.prompt.created_at != claim.claimed_at
        || dispatch.prompt.updated_at != claim.claimed_at
        || dispatch.run.created_at != claim.claimed_at
        || dispatch.run.updated_at != claim.claimed_at
    {
        return Err(StoreError::Conflict);
    }

    let transaction = begin(connection)?;
    if let Some((fingerprint, occurrence_id, thread_id, prompt_id, run_id)) = transaction
        .query_row(
            "SELECT attempt.request_fingerprint,dispatch.occurrence_id,dispatch.thread_id,
                    dispatch.prompt_message_id,dispatch.run_id
             FROM automation_occurrence_dispatches dispatch
             JOIN automation_occurrence_claim_attempts attempt
               ON attempt.occurrence_id=dispatch.occurrence_id
              AND attempt.sequence=dispatch.claim_sequence
             WHERE attempt.command_scope=?1 AND attempt.idempotency_key=?2",
            params![claim.command.scope, claim.command.key],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?
    {
        if fingerprint != claim.command.fingerprint
            || occurrence_id != claim.occurrence_id.as_str()
            || thread_id != dispatch.thread.id.as_str()
            || prompt_id != dispatch.prompt.id.as_str()
            || run_id != dispatch.run.id.as_str()
        {
            return Err(StoreError::Conflict);
        }
        return query_dispatch_result(
            &transaction,
            &occurrence_id,
            &thread_id,
            &prompt_id,
            &run_id,
        );
    }
    // A claim command without a dispatch binding is either a legacy standalone
    // claim or corrupt partial state. It can never be upgraded into work.
    let command_exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM automation_occurrence_claim_attempts
             WHERE command_scope=?1 AND idempotency_key=?2)",
            params![claim.command.scope, claim.command.key],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if command_exists {
        return Err(StoreError::Conflict);
    }

    ensure_live_lease(
        &transaction,
        &claim.lease,
        claim.claimed_at,
        Some(claim.expires_at),
    )?;
    let mut occurrence = query_occurrence(&transaction, claim.occurrence_id.as_str())?;
    if occurrence.revision != claim.expected_revision
        || occurrence.state != AutomationOccurrenceState::Pending
        || dispatch.thread.project_id != occurrence.snapshot.project_id
        || dispatch.prompt.content != occurrence.snapshot.prompt
    {
        return Err(StoreError::Conflict);
    }
    let project_state = transaction
        .query_row(
            "SELECT state FROM projects WHERE id=?1",
            [occurrence.snapshot.project_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    if project_state != Some(mapping::project_state_to_i64(ProjectState::Active)) {
        return Err(StoreError::Conflict);
    }

    occurrence
        .claim(&claim.lease, claim.claimed_at, claim.expires_at)
        .map_err(|_| StoreError::Conflict)?;
    update_occurrence(&transaction, &occurrence)?;
    transaction
        .execute(
            "INSERT INTO automation_occurrence_claim_attempts(
                 occurrence_id,sequence,command_scope,idempotency_key,request_fingerprint,
                 owner_id,fence,claimed_at,expires_at,completed_at,outcome,result_occurrence_revision
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,NULL,NULL,?10)",
            params![
                occurrence.id.as_str(),
                i64::from(occurrence.claim_attempt_count),
                claim.command.scope,
                claim.command.key,
                claim.command.fingerprint.as_slice(),
                claim.lease.owner_id.as_str(),
                number(claim.lease.fence)?,
                number(claim.claimed_at)?,
                number(claim.expires_at)?,
                number(occurrence.revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    transaction
        .execute(
            "INSERT INTO threads(id,project_id,title,state,revision,created_at,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                dispatch.thread.id.as_str(),
                dispatch.thread.project_id.as_str(),
                dispatch.thread.title,
                mapping::thread_state_to_i64(dispatch.thread.state),
                number(dispatch.thread.revision)?,
                number(dispatch.thread.created_at)?,
                number(dispatch.thread.updated_at)?,
            ],
        )
        .map_err(map_sqlite)?;
    transaction
        .execute(
            "INSERT INTO messages(id,thread_id,sequence,role,content,state,revision,created_at,updated_at)
             VALUES (?1,?2,1,?3,?4,?5,?6,?7,?8)",
            params![
                dispatch.prompt.id.as_str(),
                dispatch.prompt.thread_id.as_str(),
                mapping::message_role_to_i64(dispatch.prompt.role),
                dispatch.prompt.content,
                mapping::message_state_to_i64(dispatch.prompt.state),
                number(dispatch.prompt.revision)?,
                number(dispatch.prompt.created_at)?,
                number(dispatch.prompt.updated_at)?,
            ],
        )
        .map_err(map_sqlite)?;
    crate::store::insert_run(&transaction, &dispatch.run)?;
    crate::store::append_events(
        &transaction,
        &dispatch.run.id,
        vec![grok_application::NewRunEvent {
            occurred_at: claim.claimed_at,
            kind: RunEventKind::Created,
        }],
    )?;
    occurrence
        .link_run(&claim.lease, dispatch.run.id.clone(), claim.claimed_at)
        .map_err(|_| StoreError::Conflict)?;
    update_occurrence(&transaction, &occurrence)?;
    complete_open_attempt(&transaction, &occurrence, claim.claimed_at, 1)?;
    transaction
        .execute(
            "INSERT INTO automation_occurrence_dispatches(
                 occurrence_id,claim_sequence,thread_id,prompt_message_id,run_id,
                 request_fingerprint,created_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                occurrence.id.as_str(),
                i64::from(occurrence.claim_attempt_count),
                dispatch.thread.id.as_str(),
                dispatch.prompt.id.as_str(),
                dispatch.run.id.as_str(),
                claim.command.fingerprint.as_slice(),
                number(claim.claimed_at)?,
            ],
        )
        .map_err(map_sqlite)?;
    transaction.commit().map_err(map_sqlite)?;
    Ok(AutomationOccurrenceDispatchResult {
        occurrence,
        thread: dispatch.thread,
        prompt: dispatch.prompt,
        run: dispatch.run,
    })
}

fn query_dispatch_result(
    connection: &Connection,
    occurrence_id: &str,
    thread_id: &str,
    prompt_id: &str,
    run_id: &str,
) -> Result<AutomationOccurrenceDispatchResult, StoreError> {
    Ok(AutomationOccurrenceDispatchResult {
        occurrence: query_occurrence(connection, occurrence_id)?,
        thread: connection
            .query_row(
                &format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id=?1"),
                [thread_id],
                mapping::thread_from_row,
            )
            .map_err(map_sqlite)?,
        prompt: connection
            .query_row(
                &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id=?1"),
                [prompt_id],
                mapping::message_from_row,
            )
            .map_err(map_sqlite)?,
        run: connection
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id=?1"),
                [run_id],
                mapping::run_from_row,
            )
            .map_err(map_sqlite)?,
    })
}

fn list_resumable_dispatches(
    connection: &Connection,
    after: Option<&str>,
    limit: usize,
) -> Result<Vec<AutomationOccurrenceDispatchResult>, StoreError> {
    if limit == 0 || limit > MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS {
        return Err(StoreError::Conflict);
    }
    let bindings = {
        let mut statement = connection
            .prepare(
                "SELECT dispatch.occurrence_id,dispatch.thread_id,
                        dispatch.prompt_message_id,dispatch.run_id
                 FROM automation_occurrence_dispatches dispatch
                 JOIN automation_occurrences occurrence ON occurrence.id=dispatch.occurrence_id
                 JOIN runs run ON run.id=dispatch.run_id
                 WHERE occurrence.state=3 AND run.state=0
                   AND (?1 IS NULL OR dispatch.occurrence_id>?1)
                 ORDER BY dispatch.occurrence_id LIMIT ?2",
            )
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map(params![after, sql_limit(limit)], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(map_sqlite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)?
    };
    bindings
        .into_iter()
        .map(|(occurrence, thread, prompt, run)| {
            query_dispatch_result(connection, &occurrence, &thread, &prompt, &run)
        })
        .collect()
}

fn begin_occurrence_run(
    connection: &mut Connection,
    occurrence_id: &AutomationOccurrenceId,
    expected_occurrence_revision: u64,
    run_id: &RunId,
    expected_run_revision: u64,
    now: UnixMillis,
) -> Result<AutomationOccurrenceDispatchResult, StoreError> {
    let transaction = begin(connection)?;
    let binding = transaction
        .query_row(
            "SELECT thread_id,prompt_message_id,run_id
             FROM automation_occurrence_dispatches WHERE occurrence_id=?1",
            [occurrence_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?
        .ok_or(StoreError::NotFound)?;
    if binding.2 != run_id.as_str() {
        return Err(StoreError::Conflict);
    }
    let occurrence = query_occurrence(&transaction, occurrence_id.as_str())?;
    let mut run = transaction
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id=?1"),
            [run_id.as_str()],
            mapping::run_from_row,
        )
        .map_err(map_sqlite)?;
    if occurrence.state != AutomationOccurrenceState::RunLinked
        || occurrence.revision != expected_occurrence_revision
        || occurrence.run_id.as_ref() != Some(run_id)
        || run.state != RunState::Queued
        || run.revision != expected_run_revision
    {
        return Err(StoreError::Conflict);
    }
    let mut events = Vec::with_capacity(2);
    let mut from = run.state;
    run.transition(RunState::Planning, now)
        .map_err(|_| StoreError::Conflict)?;
    events.push(grok_application::NewRunEvent {
        occurred_at: now,
        kind: RunEventKind::StateChanged {
            from,
            to: RunState::Planning,
        },
    });
    from = run.state;
    run.transition(RunState::Running, now)
        .map_err(|_| StoreError::Conflict)?;
    events.push(grok_application::NewRunEvent {
        occurred_at: now,
        kind: RunEventKind::StateChanged {
            from,
            to: RunState::Running,
        },
    });
    let changed = transaction
        .execute(
            "UPDATE runs SET state=?1,revision=?2,updated_at=?3
             WHERE id=?4 AND revision=?5",
            params![
                mapping::run_state_to_i64(run.state),
                number(run.revision)?,
                number(run.updated_at)?,
                run.id.as_str(),
                number(expected_run_revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    if changed != 1 || run.revision != expected_run_revision.saturating_add(2) {
        return Err(StoreError::Conflict);
    }
    crate::store::append_events(&transaction, &run.id, events)?;
    let result = query_dispatch_result(
        &transaction,
        occurrence_id.as_str(),
        &binding.0,
        &binding.1,
        &binding.2,
    )?;
    transaction.commit().map_err(map_sqlite)?;
    Ok(result)
}

fn complete_occurrence_run(
    connection: &mut Connection,
    occurrence_id: &AutomationOccurrenceId,
    expected_revision: u64,
    run_id: &RunId,
    completion: AutomationOccurrenceRunCompletion,
    now: UnixMillis,
) -> Result<AutomationOccurrence, StoreError> {
    let transaction = begin(connection)?;
    let bound: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM automation_occurrence_dispatches
             WHERE occurrence_id=?1 AND run_id=?2)",
            params![occurrence_id.as_str(), run_id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if !bound {
        return Err(StoreError::Conflict);
    }
    let mut run = transaction
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id=?1"),
            [run_id.as_str()],
            mapping::run_from_row,
        )
        .map_err(map_sqlite)?;
    let next_run_state = match completion {
        AutomationOccurrenceRunCompletion::Succeeded => RunState::Completed,
        AutomationOccurrenceRunCompletion::Failed => RunState::Failed,
        AutomationOccurrenceRunCompletion::InterruptedNeedsReview => {
            RunState::InterruptedNeedsReview
        }
        AutomationOccurrenceRunCompletion::Cancelled => RunState::Cancelled,
    };
    let mut occurrence = query_occurrence(&transaction, occurrence_id.as_str())?;
    if run.state != RunState::Running
        || occurrence.revision != expected_revision
        || occurrence.run_id.as_ref() != Some(run_id)
    {
        return Err(StoreError::Conflict);
    }
    run.transition(next_run_state, now)
        .map_err(|_| StoreError::Conflict)?;
    match completion {
        AutomationOccurrenceRunCompletion::Succeeded => occurrence.succeed(run_id, now),
        AutomationOccurrenceRunCompletion::Failed => occurrence.fail(run_id, now),
        AutomationOccurrenceRunCompletion::InterruptedNeedsReview => {
            occurrence.interrupt(run_id, now)
        }
        AutomationOccurrenceRunCompletion::Cancelled => occurrence.cancel(now),
    }
    .map_err(|_| StoreError::Conflict)?;
    crate::store::update_run(&transaction, &run, run.revision.saturating_sub(1))?;
    crate::store::append_events(
        &transaction,
        &run.id,
        vec![grok_application::NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::StateChanged {
                from: RunState::Running,
                to: next_run_state,
            },
        }],
    )?;
    update_occurrence(&transaction, &occurrence)?;
    promote_queued(&transaction, occurrence.automation_id.as_str(), now)?;
    transaction.commit().map_err(map_sqlite)?;
    Ok(occurrence)
}

#[allow(clippy::too_many_lines)]
fn recover_claims(
    connection: &mut Connection,
    lease: &AutomationSchedulerLeaseToken,
    now: UnixMillis,
    limit: usize,
) -> Result<AutomationSchedulerRecoverySummary, StoreError> {
    if !(1..=MAX_AUTOMATION_SCHEDULER_RECOVERY_BATCH).contains(&limit) {
        return Err(StoreError::Conflict);
    }
    let transaction = begin(connection)?;
    ensure_live_lease(&transaction, lease, now, None)?;
    validate_occurrence_cardinality(&transaction, None)?;
    let query_limit = limit.saturating_add(1);
    let ids = {
        let mut statement = transaction
            .prepare(
                "SELECT id FROM automation_occurrences
                 WHERE state IN (2,3) AND claim_expires_at<=?1
                 ORDER BY claim_expires_at,id LIMIT ?2",
            )
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map(params![number(now)?, sql_limit(query_limit)], |row| {
                row.get::<_, String>(0)
            })
            .map_err(map_sqlite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)?
    };
    let truncated = ids.len() > limit;
    let mut summary = AutomationSchedulerRecoverySummary {
        truncated,
        ..AutomationSchedulerRecoverySummary::default()
    };
    for id in ids.into_iter().take(limit) {
        let mut occurrence = query_occurrence(&transaction, &id)?;
        match occurrence.state {
            AutomationOccurrenceState::Claimed => {
                occurrence
                    .release_expired_claim(now)
                    .map_err(|_| StoreError::Conflict)?;
                update_occurrence(&transaction, &occurrence)?;
                if occurrence.claim_attempt_count >= MAX_AUTOMATION_OCCURRENCE_CLAIM_ATTEMPTS {
                    occurrence
                        .mark_claims_exhausted(now)
                        .map_err(|_| StoreError::Conflict)?;
                    update_occurrence(&transaction, &occurrence)?;
                    complete_open_attempt(&transaction, &occurrence, now, 2)?;
                    promote_queued(&transaction, occurrence.automation_id.as_str(), now)?;
                    summary.attempts_exhausted += 1;
                } else {
                    complete_open_attempt(&transaction, &occurrence, now, 0)?;
                    summary.released_unlinked += 1;
                }
            }
            AutomationOccurrenceState::RunLinked => {
                let run_id = occurrence.run_id.clone().ok_or_else(|| {
                    StoreError::Internal("run-linked automation occurrence lacks run id".into())
                })?;
                let resumable: bool = transaction
                    .query_row(
                        "SELECT EXISTS(
                            SELECT 1 FROM automation_occurrence_dispatches dispatch
                            JOIN runs run ON run.id=dispatch.run_id
                            WHERE dispatch.occurrence_id=?1
                              AND dispatch.run_id=?2 AND run.state=0
                        )",
                        params![occurrence.id.as_str(), run_id.as_str()],
                        |row| row.get(0),
                    )
                    .map_err(map_sqlite)?;
                if resumable {
                    summary.resumable_bound_queued =
                        summary.resumable_bound_queued.saturating_add(1);
                    continue;
                }
                let mut run = transaction
                    .query_row(
                        &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id=?1"),
                        [run_id.as_str()],
                        mapping::run_from_row,
                    )
                    .map_err(map_sqlite)?;
                match run.state {
                    RunState::Completed => occurrence.succeed(&run_id, now),
                    RunState::Failed => occurrence.fail(&run_id, now),
                    RunState::Cancelled => occurrence.cancel(now),
                    RunState::InterruptedNeedsReview => occurrence.interrupt(&run_id, now),
                    prior => {
                        let expected_revision = run.revision;
                        run.transition(RunState::InterruptedNeedsReview, now)
                            .map_err(|_| StoreError::Conflict)?;
                        crate::store::update_run(&transaction, &run, expected_revision)?;
                        crate::store::append_events(
                            &transaction,
                            &run.id,
                            vec![grok_application::NewRunEvent {
                                occurred_at: now,
                                kind: RunEventKind::StateChanged {
                                    from: prior,
                                    to: RunState::InterruptedNeedsReview,
                                },
                            }],
                        )?;
                        occurrence.interrupt(&run_id, now)
                    }
                }
                .map_err(|_| StoreError::Conflict)?;
                update_occurrence(&transaction, &occurrence)?;
                ensure_linked_attempt_evidence(&transaction, &occurrence, now)?;
                promote_queued(&transaction, occurrence.automation_id.as_str(), now)?;
                if occurrence.state == AutomationOccurrenceState::InterruptedNeedsReview {
                    summary.interrupted_linked += 1;
                }
            }
            _ => {
                return Err(StoreError::Internal(
                    "expired automation claim query returned invalid state".into(),
                ));
            }
        }
    }
    transaction.commit().map_err(map_sqlite)?;
    Ok(summary)
}

fn complete_open_attempt(
    transaction: &Transaction<'_>,
    occurrence: &AutomationOccurrence,
    now: UnixMillis,
    outcome: i64,
) -> Result<(), StoreError> {
    let changed = transaction
        .execute(
            "UPDATE automation_occurrence_claim_attempts
             SET completed_at=?1,outcome=?2
             WHERE occurrence_id=?3 AND sequence=?4 AND completed_at IS NULL",
            params![
                number(now)?,
                outcome,
                occurrence.id.as_str(),
                i64::from(occurrence.claim_attempt_count),
            ],
        )
        .map_err(map_sqlite)?;
    ensure_one(changed)
}

fn ensure_linked_attempt_evidence(
    transaction: &Transaction<'_>,
    occurrence: &AutomationOccurrence,
    now: UnixMillis,
) -> Result<(), StoreError> {
    let evidence = transaction
        .query_row(
            "SELECT completed_at,outcome FROM automation_occurrence_claim_attempts
             WHERE occurrence_id=?1 AND sequence=?2",
            params![
                occurrence.id.as_str(),
                i64::from(occurrence.claim_attempt_count),
            ],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, Option<i64>>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?;
    match evidence {
        Some((Some(completed_at), Some(1)))
            if u64::try_from(completed_at).is_ok_and(|completed_at| completed_at <= now) =>
        {
            Ok(())
        }
        _ => Err(StoreError::Internal(
            "run-linked automation occurrence lacks immutable claim evidence".into(),
        )),
    }
}

fn promote_queued(
    transaction: &Transaction<'_>,
    automation_id: &str,
    now: UnixMillis,
) -> Result<(), StoreError> {
    let queued_id = transaction
        .query_row(
            "SELECT id FROM automation_occurrences
             WHERE automation_id=?1 AND state=1",
            [automation_id],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    if let Some(id) = queued_id {
        let mut queued = query_occurrence(transaction, &id)?;
        queued
            .promote_queued(now)
            .map_err(|_| StoreError::Conflict)?;
        update_occurrence(transaction, &queued)?;
    }
    Ok(())
}

fn link_occurrence_run(
    connection: &mut Connection,
    lease: &AutomationSchedulerLeaseToken,
    occurrence_id: &AutomationOccurrenceId,
    expected_revision: u64,
    run_id: RunId,
    now: UnixMillis,
) -> Result<AutomationOccurrence, StoreError> {
    let transaction = begin(connection)?;
    ensure_live_lease(&transaction, lease, now, None)?;
    let mut occurrence = query_occurrence(&transaction, occurrence_id.as_str())?;
    if occurrence.revision != expected_revision {
        return Err(StoreError::Conflict);
    }
    // A durable run must already exist; the FK on automation_occurrences.run_id
    // rejects inventing identifiers that never entered the run journal.
    let run_exists: bool = transaction
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM runs WHERE id=?1)",
            [run_id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if !run_exists {
        return Err(StoreError::Conflict);
    }
    occurrence
        .link_run(lease, run_id, now)
        .map_err(|_| StoreError::Conflict)?;
    update_occurrence(&transaction, &occurrence)?;
    // Immutable claim evidence must record the link (outcome=1) while state is
    // RunLinked, matching automation_occurrence_claim_attempts completion rules.
    complete_open_attempt(&transaction, &occurrence, now, 1)?;
    transaction.commit().map_err(map_sqlite)?;
    Ok(occurrence)
}

fn journal_status(
    connection: &mut Connection,
) -> Result<AutomationSchedulerJournalStatus, StoreError> {
    validate_occurrence_cardinality(connection, None)?;
    let lease = query_lease(connection)?;
    let cursor_count = count(connection, "automation_schedule_cursors", None)?;
    let pending_count = count(connection, "automation_occurrences", Some(0))?;
    let queued_overlap_count = count(connection, "automation_occurrences", Some(1))?;
    let claimed_count = count(connection, "automation_occurrences", Some(2))?;
    let run_linked_count = count(connection, "automation_occurrences", Some(3))?;
    let needs_review_count = count(connection, "automation_occurrences", Some(9))?;
    Ok(AutomationSchedulerJournalStatus {
        lease,
        cursor_count,
        pending_count,
        queued_overlap_count,
        claimed_count,
        run_linked_count,
        needs_review_count,
    })
}

fn validate_occurrence_cardinality(
    connection: &Connection,
    automation_id: Option<&str>,
) -> Result<(), StoreError> {
    let invalid: bool = connection
        .query_row(
            "SELECT EXISTS(
                 SELECT automation_id FROM automation_occurrences
                 WHERE ?1 IS NULL OR automation_id=?1
                 GROUP BY automation_id
                 HAVING SUM(state IN (0,2,3)) > 1
                    OR SUM(state=1) > 1
                    OR (SUM(state IN (0,2,3)) = 0 AND SUM(state=1) != 0)
             )",
            [automation_id],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if invalid {
        return Err(StoreError::Internal(
            "invalid automation occurrence cardinality".into(),
        ));
    }
    Ok(())
}

fn count(connection: &Connection, table: &str, state: Option<i64>) -> Result<u64, StoreError> {
    let value: i64 = if let Some(state) = state {
        connection.query_row(
            &format!("SELECT count(*) FROM {table} WHERE state=?1"),
            [state],
            |row| row.get(0),
        )
    } else {
        connection.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
    }
    .map_err(map_sqlite)?;
    value
        .try_into()
        .map_err(|_| StoreError::Internal("negative scheduler journal count".into()))
}

fn update_occurrence(
    transaction: &Transaction<'_>,
    occurrence: &AutomationOccurrence,
) -> Result<(), StoreError> {
    let claim = occurrence.claim.as_ref();
    let changed = transaction
        .execute(
            "UPDATE automation_occurrences
             SET state=?1,claim_owner_id=?2,claim_fence=?3,claimed_at=?4,
                 claim_expires_at=?5,run_id=?6,claim_attempt_count=?7,revision=?8,updated_at=?9
             WHERE id=?10",
            params![
                occurrence_state_to_i64(occurrence.state),
                claim.map(|claim| claim.owner_id.as_str()),
                claim.map(|claim| claim.fence).map(number).transpose()?,
                claim
                    .map(|claim| claim.claimed_at)
                    .map(number)
                    .transpose()?,
                claim
                    .map(|claim| claim.expires_at)
                    .map(number)
                    .transpose()?,
                occurrence.run_id.as_ref().map(RunId::as_str),
                i64::from(occurrence.claim_attempt_count),
                number(occurrence.revision)?,
                number(occurrence.updated_at)?,
                occurrence.id.as_str(),
            ],
        )
        .map_err(map_sqlite)?;
    ensure_one(changed)
}

fn query_evaluation_result(
    connection: &Connection,
    scope: &str,
    key: &str,
) -> Result<AutomationScheduleEvaluationResult, StoreError> {
    let cursor = connection
        .query_row(
            "SELECT command.automation_id,command.definition_revision,
                    command.schedule_fingerprint,1,command.evaluated_through,
                    command.next_kind,command.next_year,command.next_month,command.next_day,
                    command.next_hour,command.next_minute,command.next_scheduled_for,
                    command.result_cursor_revision,cursor.created_at,command.result_updated_at
             FROM automation_schedule_evaluation_commands command
             JOIN automation_schedule_cursors cursor
               ON cursor.automation_id=command.automation_id
             WHERE command.command_scope=?1 AND command.idempotency_key=?2",
            params![scope, key],
            |row| cursor_from_row(row, 0),
        )
        .map_err(map_sqlite)?;
    let historical_columns = "id,automation_id,definition_revision,snapshot_project_id,snapshot_title,snapshot_prompt,\
         canonical_schedule,timezone,missed_run_policy,overlap_policy,schedule_fingerprint,\
         calculator_version,nominal_year,nominal_month,nominal_day,nominal_hour,nominal_minute,\
         scheduled_for,occurrence_count,initial_state,NULL,NULL,NULL,NULL,NULL,0,\
         initial_revision,created_at,created_at";
    let occurrences = {
        let mut statement = connection
            .prepare(&format!(
                "SELECT {historical_columns} FROM automation_occurrences
                 WHERE evaluation_scope=?1 AND evaluation_key=?2
                 ORDER BY nominal_year,nominal_month,nominal_day,nominal_hour,nominal_minute,id"
            ))
            .map_err(map_sqlite)?;
        let rows = statement
            .query_map(params![scope, key], |row| occurrence_from_row(row, 0))
            .map_err(map_sqlite)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(map_sqlite)?
    };
    Ok(AutomationScheduleEvaluationResult {
        cursor,
        occurrences,
    })
}

fn query_claim_result(
    connection: &Connection,
    scope: &str,
    key: &str,
) -> Result<AutomationOccurrence, StoreError> {
    connection
        .query_row(
            "SELECT occurrence.id,occurrence.automation_id,occurrence.definition_revision,
                    occurrence.snapshot_project_id,occurrence.snapshot_title,
                    occurrence.snapshot_prompt,occurrence.canonical_schedule,occurrence.timezone,
                    occurrence.missed_run_policy,occurrence.overlap_policy,
                    occurrence.schedule_fingerprint,occurrence.calculator_version,
                    occurrence.nominal_year,occurrence.nominal_month,occurrence.nominal_day,
                    occurrence.nominal_hour,occurrence.nominal_minute,occurrence.scheduled_for,
                    occurrence.occurrence_count,2,attempt.owner_id,attempt.fence,
                    attempt.claimed_at,attempt.expires_at,NULL,attempt.sequence,
                    attempt.result_occurrence_revision,occurrence.created_at,attempt.claimed_at
             FROM automation_occurrence_claim_attempts attempt
             JOIN automation_occurrences occurrence ON occurrence.id=attempt.occurrence_id
             WHERE attempt.command_scope=?1 AND attempt.idempotency_key=?2",
            params![scope, key],
            |row| occurrence_from_row(row, 0),
        )
        .map_err(map_sqlite)
}

fn query_automation(connection: &Connection, id: &str) -> Result<Automation, StoreError> {
    connection
        .query_row(
            &format!("SELECT {AUTOMATION_COLUMNS} FROM automations WHERE id=?1"),
            [id],
            mapping::automation_from_row,
        )
        .map_err(map_sqlite)
        .and_then(|automation| {
            Automation::restore(automation)
                .map_err(|_| StoreError::Internal("invalid persisted automation".into()))
        })
}

fn validate_cursor_binding(
    cursor: &AutomationScheduleCursor,
    automation: &Automation,
) -> Result<(), StoreError> {
    let snapshot = AutomationExecutionSnapshot::new(
        automation.revision,
        automation.project_id.clone(),
        automation.title.clone(),
        automation.prompt.clone(),
        automation.schedule.clone(),
        automation.timezone.clone(),
        automation.missed_run_policy,
        automation.overlap_policy,
    )
    .map_err(|_| StoreError::Internal("invalid persisted automation definition".into()))?;
    let expected_next = snapshot
        .schedule
        .next_decision_after(&snapshot.timezone, cursor.evaluated_through)
        .ok();
    if cursor.automation_id != automation.id
        || cursor.definition_revision != automation.revision
        || cursor.schedule_fingerprint != snapshot.schedule_fingerprint
        || cursor.calculator_version != snapshot.calculator_version
        || cursor.next_decision != expected_next
    {
        return Err(StoreError::Internal(
            "invalid persisted automation schedule cursor".into(),
        ));
    }
    Ok(())
}

fn query_cursor_optional(
    connection: &Connection,
    automation_id: &str,
) -> Result<Option<AutomationScheduleCursor>, StoreError> {
    connection
        .query_row(
            &format!(
                "SELECT {CURSOR_COLUMNS} FROM automation_schedule_cursors WHERE automation_id=?1"
            ),
            [automation_id],
            |row| cursor_from_row(row, 0),
        )
        .optional()
        .map_err(map_sqlite)
}

fn query_occurrence(connection: &Connection, id: &str) -> Result<AutomationOccurrence, StoreError> {
    connection
        .query_row(
            &format!("SELECT {OCCURRENCE_COLUMNS} FROM automation_occurrences WHERE id=?1"),
            [id],
            |row| occurrence_from_row(row, 0),
        )
        .map_err(map_sqlite)
}

fn query_lease(connection: &Connection) -> Result<Option<AutomationSchedulerLease>, StoreError> {
    connection
        .query_row(
            "SELECT owner_id,fence,acquired_at,renewed_at,expires_at
             FROM automation_scheduler_lease WHERE singleton=1",
            [],
            lease_from_row,
        )
        .optional()
        .map_err(map_sqlite)
}

fn ensure_live_lease(
    connection: &Connection,
    token: &AutomationSchedulerLeaseToken,
    now: UnixMillis,
    child_expires_at: Option<UnixMillis>,
) -> Result<(), StoreError> {
    let lease = query_lease(connection)?.ok_or(StoreError::Conflict)?;
    if lease.token() != *token
        || !lease.is_valid_at(now)
        || child_expires_at.is_some_and(|expires_at| expires_at > lease.expires_at)
    {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn cursor_from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<AutomationScheduleCursor> {
    let next_kind = row.get::<_, Option<i64>>(offset + 5)?;
    let next_decision = match next_kind {
        None => None,
        Some(kind) => {
            let nominal_local = AutomationLocalDateTime::new(
                row.get(offset + 6)?,
                row.get::<_, i64>(offset + 7)?
                    .try_into()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                row.get::<_, i64>(offset + 8)?
                    .try_into()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                row.get::<_, i64>(offset + 9)?
                    .try_into()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                row.get::<_, i64>(offset + 10)?
                    .try_into()
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
            )
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
            match kind {
                0 => Some(AutomationScheduleDecision::Due {
                    nominal_local,
                    scheduled_for: unsigned(row, offset + 11)?,
                }),
                1 if row.get::<_, Option<i64>>(offset + 11)?.is_none() => {
                    Some(AutomationScheduleDecision::SkippedNonexistentLocalTime { nominal_local })
                }
                _ => return Err(rusqlite::Error::InvalidQuery),
            }
        }
    };
    let cursor = AutomationScheduleCursor {
        automation_id: AutomationId::new(row.get::<_, String>(offset)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        definition_revision: unsigned(row, offset + 1)?,
        schedule_fingerprint: AutomationScheduleFingerprint::new(blob32(row, offset + 2)?),
        calculator_version: row
            .get::<_, i64>(offset + 3)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        evaluated_through: unsigned(row, offset + 4)?,
        next_decision,
        revision: unsigned(row, offset + 12)?,
        created_at: unsigned(row, offset + 13)?,
        updated_at: unsigned(row, offset + 14)?,
    };
    AutomationScheduleCursor::restore(cursor).map_err(|_| rusqlite::Error::InvalidQuery)
}

fn occurrence_from_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<AutomationOccurrence> {
    let definition_revision = unsigned(row, offset + 2)?;
    let canonical_schedule = row.get::<_, String>(offset + 6)?;
    let snapshot = AutomationExecutionSnapshot::new(
        definition_revision,
        ProjectId::new(row.get::<_, String>(offset + 3)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get(offset + 4)?,
        row.get(offset + 5)?,
        canonical_schedule,
        row.get(offset + 7)?,
        missed_run_policy_from_i64(row.get(offset + 8)?)?,
        overlap_policy_from_i64(row.get(offset + 9)?)?,
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)?;
    if snapshot.schedule_fingerprint.to_bytes() != blob32(row, offset + 10)?
        || i64::from(snapshot.calculator_version) != row.get::<_, i64>(offset + 11)?
    {
        return Err(rusqlite::Error::InvalidQuery);
    }
    let nominal_local = AutomationLocalDateTime::new(
        row.get(offset + 12)?,
        row.get::<_, i64>(offset + 13)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, i64>(offset + 14)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, i64>(offset + 15)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        row.get::<_, i64>(offset + 16)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    )
    .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let owner = row.get::<_, Option<String>>(offset + 20)?;
    let fence = optional_unsigned(row, offset + 21)?;
    let claimed_at = optional_unsigned(row, offset + 22)?;
    let expires_at = optional_unsigned(row, offset + 23)?;
    let claim = match (owner, fence, claimed_at, expires_at) {
        (None, None, None, None) => None,
        (Some(owner), Some(fence), Some(claimed_at), Some(expires_at)) => {
            Some(AutomationOccurrenceClaim {
                owner_id: AutomationSchedulerOwnerId::new(owner)
                    .map_err(|_| rusqlite::Error::InvalidQuery)?,
                fence,
                claimed_at,
                expires_at,
            })
        }
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let occurrence = AutomationOccurrence {
        id: AutomationOccurrenceId::new(row.get::<_, String>(offset)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        automation_id: AutomationId::new(row.get::<_, String>(offset + 1)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        snapshot,
        nominal_local,
        scheduled_for: optional_unsigned(row, offset + 17)?,
        occurrence_count: row
            .get::<_, i64>(offset + 18)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        state: occurrence_state_from_i64(row.get(offset + 19)?)?,
        claim,
        run_id: row
            .get::<_, Option<String>>(offset + 24)?
            .map(RunId::new)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        claim_attempt_count: row
            .get::<_, i64>(offset + 25)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        revision: unsigned(row, offset + 26)?,
        created_at: unsigned(row, offset + 27)?,
        updated_at: unsigned(row, offset + 28)?,
    };
    AutomationOccurrence::restore(occurrence).map_err(|_| rusqlite::Error::InvalidQuery)
}

fn lease_from_row(row: &Row<'_>) -> rusqlite::Result<AutomationSchedulerLease> {
    AutomationSchedulerLease::restore(AutomationSchedulerLease {
        owner_id: AutomationSchedulerOwnerId::new(row.get::<_, String>(0)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        fence: unsigned(row, 1)?,
        acquired_at: unsigned(row, 2)?,
        renewed_at: unsigned(row, 3)?,
        expires_at: unsigned(row, 4)?,
    })
    .map_err(|_| rusqlite::Error::InvalidQuery)
}

const fn occurrence_state_to_i64(state: AutomationOccurrenceState) -> i64 {
    match state {
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

fn occurrence_state_from_i64(value: i64) -> rusqlite::Result<AutomationOccurrenceState> {
    match value {
        0 => Ok(AutomationOccurrenceState::Pending),
        1 => Ok(AutomationOccurrenceState::QueuedOverlap),
        2 => Ok(AutomationOccurrenceState::Claimed),
        3 => Ok(AutomationOccurrenceState::RunLinked),
        4 => Ok(AutomationOccurrenceState::Succeeded),
        5 => Ok(AutomationOccurrenceState::Failed),
        6 => Ok(AutomationOccurrenceState::SkippedMissed),
        7 => Ok(AutomationOccurrenceState::SkippedOverlap),
        8 => Ok(AutomationOccurrenceState::SkippedInvalidLocalTime),
        9 => Ok(AutomationOccurrenceState::InterruptedNeedsReview),
        10 => Ok(AutomationOccurrenceState::Cancelled),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn missed_run_policy_from_i64(value: i64) -> rusqlite::Result<MissedRunPolicy> {
    match value {
        0 => Ok(MissedRunPolicy::RunOnce),
        1 => Ok(MissedRunPolicy::Skip),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn overlap_policy_from_i64(value: i64) -> rusqlite::Result<OverlapPolicy> {
    match value {
        0 => Ok(OverlapPolicy::QueueOne),
        1 => Ok(OverlapPolicy::Skip),
        _ => Err(rusqlite::Error::InvalidQuery),
    }
}

fn blob32(row: &Row<'_>, index: usize) -> rusqlite::Result<[u8; 32]> {
    row.get::<_, Vec<u8>>(index)?
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

fn unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    row.get::<_, i64>(index)?
        .try_into()
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

fn optional_unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(u64::try_from)
        .transpose()
        .map_err(|_| rusqlite::Error::InvalidQuery)
}

fn validate_command(command: &MutationCommand, scope: &str) -> Result<(), StoreError> {
    if command.scope != scope || command.key.is_empty() || command.key.len() > 128 {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn begin(connection: &mut Connection) -> Result<Transaction<'_>, StoreError> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite)
}

fn ensure_one(changed: usize) -> Result<(), StoreError> {
    if changed != 1 {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn number(value: u64) -> Result<i64, StoreError> {
    value
        .try_into()
        .map_err(|_| StoreError::Internal("numeric value out of range".into()))
}

fn optional_number(value: Option<u64>) -> Result<Option<i64>, StoreError> {
    value.map(number).transpose()
}

fn sql_limit(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn map_sqlite(error: rusqlite::Error) -> StoreError {
    match error {
        rusqlite::Error::QueryReturnedNoRows => StoreError::NotFound,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::ConstraintViolation =>
        {
            StoreError::Conflict
        }
        rusqlite::Error::SqliteFailure(code, _)
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            ) =>
        {
            StoreError::Unavailable("encrypted database is busy".into())
        }
        error => StoreError::Internal(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use grok_application::{
        AutomationScheduleEvaluationCommit, ClaimAutomationOccurrence, DatabaseKey, MutationCommand,
    };
    use grok_domain::{
        Automation, AutomationId, AutomationOccurrence, AutomationOccurrenceId, AutomationSchedule,
        AutomationScheduleCursor, AutomationSchedulerOwnerId, MissedRunPolicy, OverlapPolicy,
        ProjectId,
    };

    use super::*;
    use crate::schema;

    fn command(scope: &str, key: &str, byte: u8) -> MutationCommand {
        MutationCommand {
            scope: scope.into(),
            key: key.into(),
            fingerprint: [byte; 32],
        }
    }

    fn scheduler_fixture() -> (tempfile::TempDir, Connection, Automation) {
        let directory = tempfile::tempdir().expect("tempdir");
        let key = DatabaseKey::from_slice(&[61; 32]).expect("key");
        let connection = schema::open_encrypted(&directory.path().join("state.db"), &key)
            .expect("encrypted scheduler database");
        let automation = Automation::new(
            AutomationId::new("automation-1").expect("automation id"),
            ProjectId::new("project-1").expect("project id"),
            "Daily automation".into(),
            "Prepare the daily automation result.".into(),
            "v1;daily;00:00".into(),
            "UTC".into(),
            MissedRunPolicy::RunOnce,
            OverlapPolicy::QueueOne,
            true,
            0,
        )
        .expect("automation");
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('project-1','Project','',0,0,0,0);",
            )
            .expect("project");
        connection
            .execute(
                "INSERT INTO automations(
                     id,project_id,title,prompt,schedule,timezone,missed_run_policy,
                     overlap_policy,state,revision,created_at,updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    automation.id.as_str(),
                    automation.project_id.as_str(),
                    &automation.title,
                    &automation.prompt,
                    &automation.schedule,
                    &automation.timezone,
                    mapping::missed_run_policy_to_i64(automation.missed_run_policy),
                    mapping::overlap_policy_to_i64(automation.overlap_policy),
                    mapping::automation_state_to_i64(automation.state),
                    number(automation.revision).expect("revision"),
                    number(automation.created_at).expect("created"),
                    number(automation.updated_at).expect("updated"),
                ],
            )
            .expect("automation row");
        (directory, connection, automation)
    }

    fn materialize_pending(
        connection: &mut Connection,
        automation: &Automation,
    ) -> (AutomationSchedulerLease, AutomationOccurrence) {
        let schedule = AutomationSchedule::parse_canonical(&automation.schedule).expect("schedule");
        let decision = schedule
            .next_decision_after(&automation.timezone, automation.updated_at)
            .expect("decision");
        let due_at = decision.scheduled_for().expect("due");
        let owner = AutomationSchedulerOwnerId::new("dispatch-owner").expect("owner");
        let AutomationSchedulerLeaseAcquisition::Acquired { lease, .. } =
            acquire_lease(connection, owner, due_at, 60_000).expect("lease")
        else {
            panic!("expected lease")
        };
        let snapshot = AutomationExecutionSnapshot::new(
            automation.revision,
            automation.project_id.clone(),
            automation.title.clone(),
            automation.prompt.clone(),
            automation.schedule.clone(),
            automation.timezone.clone(),
            automation.missed_run_policy,
            automation.overlap_policy,
        )
        .expect("snapshot");
        let initialized = AutomationScheduleCursor::new(
            automation.id.clone(),
            &snapshot,
            automation.updated_at,
            Some(decision),
            due_at,
        )
        .expect("cursor");
        commit_evaluation(
            connection,
            AutomationScheduleEvaluationCommit {
                lease: lease.token(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: None,
                cursor: initialized.clone(),
                occurrences: vec![],
                observed_at: due_at,
                command: command("automation_scheduler_evaluate_v1", "dispatch-init", 41),
            },
        )
        .expect("initialize");
        let next = snapshot
            .schedule
            .next_decision_after(&snapshot.timezone, due_at)
            .expect("next");
        let mut advanced = initialized;
        advanced
            .advance(due_at, Some(next), due_at)
            .expect("advance");
        let occurrence = AutomationOccurrence::pending(
            AutomationOccurrenceId::new("dispatch-occurrence").expect("occurrence id"),
            automation.id.clone(),
            snapshot,
            decision,
            1,
            due_at,
        )
        .expect("occurrence");
        let result = commit_evaluation(
            connection,
            AutomationScheduleEvaluationCommit {
                lease: lease.token(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: Some(0),
                cursor: advanced,
                occurrences: vec![occurrence],
                observed_at: due_at,
                command: command("automation_scheduler_evaluate_v1", "dispatch-evaluate", 42),
            },
        )
        .expect("materialize");
        (
            lease,
            result.occurrences.into_iter().next().expect("pending"),
        )
    }

    #[allow(clippy::too_many_lines)]
    #[test]
    fn sql_dispatch_is_atomic_replayable_and_prompt_immutable() {
        let (_directory, mut connection, automation) = scheduler_fixture();
        let (lease, occurrence) = materialize_pending(&mut connection, &automation);
        let now = occurrence.updated_at;
        let thread = Thread::new(
            grok_domain::ThreadId::new("dispatch-thread").expect("thread"),
            automation.project_id.clone(),
            "Automation · Daily automation".into(),
            now,
        )
        .expect("thread");
        let prompt = Message::new(
            grok_domain::MessageId::new("dispatch-prompt").expect("prompt"),
            thread.id.clone(),
            MessageRole::User,
            automation.prompt.clone(),
            now,
        )
        .expect("prompt");
        let run = grok_domain::Run::queued(
            RunId::new("dispatch-run").expect("run"),
            automation.project_id,
            thread.id.clone(),
            now,
        );
        let dispatch = AutomationOccurrenceDispatch {
            claim: ClaimAutomationOccurrence {
                lease: lease.token(),
                occurrence_id: occurrence.id,
                expected_revision: occurrence.revision,
                claimed_at: now,
                expires_at: now + 30_000,
                command: command("automation_scheduler_claim_v1", "atomic-dispatch", 43),
            },
            thread,
            prompt,
            run,
        };
        connection
            .execute_batch(
                "CREATE TRIGGER inject_dispatch_commit_failure
                 BEFORE INSERT ON automation_occurrence_dispatches BEGIN
                     SELECT RAISE(ABORT, 'injected dispatch commit failure');
                 END;",
            )
            .expect("fault trigger");
        assert!(claim_and_bind_occurrence(&mut connection, dispatch.clone()).is_err());
        assert_eq!(
            query_occurrence(&connection, "dispatch-occurrence")
                .expect("rolled-back occurrence")
                .state,
            AutomationOccurrenceState::Pending
        );
        let partial_rows: u32 = connection
            .query_row(
                "SELECT
                    (SELECT count(*) FROM automation_occurrence_claim_attempts
                     WHERE occurrence_id='dispatch-occurrence') +
                    (SELECT count(*) FROM automation_occurrence_dispatches
                     WHERE occurrence_id='dispatch-occurrence') +
                    (SELECT count(*) FROM threads WHERE id='dispatch-thread') +
                    (SELECT count(*) FROM messages WHERE id='dispatch-prompt') +
                    (SELECT count(*) FROM runs WHERE id='dispatch-run')",
                [],
                |row| row.get(0),
            )
            .expect("partial row count");
        assert_eq!(
            partial_rows, 0,
            "the failed transaction must leave no partial work"
        );
        connection
            .execute_batch("DROP TRIGGER inject_dispatch_commit_failure;")
            .expect("remove fault trigger");
        let result =
            claim_and_bind_occurrence(&mut connection, dispatch.clone()).expect("dispatch");
        assert_eq!(
            result.occurrence.state,
            AutomationOccurrenceState::RunLinked
        );
        assert_eq!(result.prompt.sequence, 1);
        assert_eq!(
            claim_and_bind_occurrence(&mut connection, dispatch).expect("exact replay"),
            result
        );
        let recovery = recover_claims(&mut connection, &lease.token(), now + 30_000, 10)
            .expect("bound queued recovery");
        assert_eq!(recovery.resumable_bound_queued, 1);
        assert_eq!(recovery.interrupted_linked, 0);
        let resumable = list_resumable_dispatches(&connection, None, 10).expect("resumable");
        assert_eq!(resumable, vec![result.clone()]);
        connection
            .execute_batch(
                "CREATE TRIGGER inject_second_start_event_failure
                 BEFORE INSERT ON run_events WHEN new.run_id='dispatch-run' AND new.sequence=3
                 BEGIN SELECT RAISE(ABORT, 'injected start-event failure'); END;",
            )
            .expect("start fault trigger");
        assert!(
            begin_occurrence_run(
                &mut connection,
                &result.occurrence.id,
                result.occurrence.revision,
                &result.run.id,
                result.run.revision,
                now,
            )
            .is_err()
        );
        let rolled_back_run = connection
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id='dispatch-run'"),
                [],
                mapping::run_from_row,
            )
            .expect("rolled-back run");
        assert_eq!(rolled_back_run.state, RunState::Queued);
        assert_eq!(rolled_back_run.revision, 0);
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM run_events WHERE run_id='dispatch-run'",
                    [],
                    |row| row.get::<_, u32>(0),
                )
                .expect("rolled-back event count"),
            1
        );
        connection
            .execute_batch("DROP TRIGGER inject_second_start_event_failure;")
            .expect("remove start fault trigger");
        let running = begin_occurrence_run(
            &mut connection,
            &result.occurrence.id,
            result.occurrence.revision,
            &result.run.id,
            result.run.revision,
            now,
        )
        .expect("persist dispatch intent");
        assert_eq!(running.run.state, RunState::Running);
        assert_eq!(running.run.revision, 2);
        connection
            .execute_batch(
                "CREATE TRIGGER inject_occurrence_completion_failure
                 BEFORE UPDATE ON automation_occurrences
                 WHEN old.id='dispatch-occurrence' AND new.state=4
                 BEGIN SELECT RAISE(ABORT, 'injected occurrence completion failure'); END;",
            )
            .expect("completion fault trigger");
        assert!(
            complete_occurrence_run(
                &mut connection,
                &result.occurrence.id,
                result.occurrence.revision,
                &result.run.id,
                AutomationOccurrenceRunCompletion::Succeeded,
                now,
            )
            .is_err()
        );
        let completion_rollback_run = connection
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id='dispatch-run'"),
                [],
                mapping::run_from_row,
            )
            .expect("completion rollback run");
        assert_eq!(completion_rollback_run.state, RunState::Running);
        assert_eq!(
            query_occurrence(&connection, "dispatch-occurrence")
                .expect("completion rollback occurrence")
                .state,
            AutomationOccurrenceState::RunLinked
        );
        connection
            .execute_batch("DROP TRIGGER inject_occurrence_completion_failure;")
            .expect("remove completion fault trigger");
        let interrupted = recover_claims(&mut connection, &lease.token(), now + 30_000, 10)
            .expect("recover running dispatch");
        assert_eq!(interrupted.interrupted_linked, 1);
        assert_eq!(
            query_occurrence(&connection, result.occurrence.id.as_str())
                .expect("recovered occurrence")
                .state,
            AutomationOccurrenceState::InterruptedNeedsReview
        );
        let recovered_run = connection
            .query_row(
                &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id=?1"),
                [result.run.id.as_str()],
                mapping::run_from_row,
            )
            .expect("recovered run");
        assert_eq!(recovered_run.state, RunState::InterruptedNeedsReview);
        assert!(
            list_resumable_dispatches(&connection, None, 10)
                .expect("no replay")
                .is_empty()
        );
        assert!(
            connection
                .execute(
                    "UPDATE messages SET content='forged' WHERE id='dispatch-prompt'",
                    []
                )
                .is_err()
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM threads WHERE id='dispatch-thread'",
                    [],
                    |row| row.get::<_, u32>(0)
                )
                .expect("thread count"),
            1
        );
        assert_eq!(
            connection
                .query_row(
                    "SELECT count(*) FROM runs WHERE id='dispatch-run'",
                    [],
                    |row| row.get::<_, u32>(0)
                )
                .expect("run count"),
            1
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn sql_scheduler_journal_replays_exact_results_and_recovers_claims() {
        let (directory, mut connection, automation) = scheduler_fixture();
        let schedule = AutomationSchedule::parse_canonical(&automation.schedule).expect("schedule");
        let first = schedule
            .next_decision_after(&automation.timezone, automation.updated_at)
            .expect("first decision");
        let second = schedule
            .next_decision_after(
                &automation.timezone,
                first.scheduled_for().expect("first due timestamp"),
            )
            .expect("second decision");
        let third = schedule
            .next_decision_after(
                &automation.timezone,
                second.scheduled_for().expect("second due timestamp"),
            )
            .expect("third decision");
        let due_at = third.scheduled_for().expect("third due timestamp");
        let owner = AutomationSchedulerOwnerId::new("scheduler-owner").expect("owner");
        let acquired = acquire_lease(&mut connection, owner, due_at, 60_000).expect("lease");
        let AutomationSchedulerLeaseAcquisition::Acquired { lease, .. } = acquired else {
            panic!("expected acquired lease");
        };
        let snapshot = AutomationExecutionSnapshot::new(
            automation.revision,
            automation.project_id.clone(),
            automation.title.clone(),
            automation.prompt.clone(),
            automation.schedule.clone(),
            automation.timezone.clone(),
            automation.missed_run_policy,
            automation.overlap_policy,
        )
        .expect("snapshot");
        let mut cursor = AutomationScheduleCursor::new(
            automation.id.clone(),
            &snapshot,
            automation.updated_at,
            Some(first),
            due_at,
        )
        .expect("initial cursor");
        let initialize = AutomationScheduleEvaluationCommit {
            lease: lease.token(),
            expected_automation_revision: automation.revision,
            expected_cursor_revision: None,
            cursor: cursor.clone(),
            occurrences: Vec::new(),
            observed_at: due_at,
            command: command("automation_scheduler_evaluate_v1", "initialize", 1),
        };
        let initialized = commit_evaluation(&mut connection, initialize.clone()).expect("init");
        assert_eq!(
            commit_evaluation(&mut connection, initialize.clone()).expect("init replay"),
            initialized
        );

        let next = schedule
            .next_decision_after(&automation.timezone, due_at)
            .expect("next decision");
        cursor
            .advance(due_at, Some(next), due_at)
            .expect("advance cursor");
        let occurrences = [first, second, third]
            .into_iter()
            .enumerate()
            .map(|(index, decision)| {
                AutomationOccurrence::pending(
                    AutomationOccurrenceId::new(format!("occurrence-{}", index + 1))
                        .expect("occurrence id"),
                    automation.id.clone(),
                    snapshot.clone(),
                    decision,
                    1,
                    due_at,
                )
                .expect("pending occurrence")
            })
            .collect::<Vec<_>>();
        let evaluate = AutomationScheduleEvaluationCommit {
            lease: lease.token(),
            expected_automation_revision: automation.revision,
            expected_cursor_revision: Some(0),
            cursor,
            occurrences,
            observed_at: due_at,
            command: command("automation_scheduler_evaluate_v1", "evaluate", 2),
        };
        let evaluated = commit_evaluation(&mut connection, evaluate.clone()).expect("evaluate");
        assert_eq!(
            evaluated
                .occurrences
                .iter()
                .map(|occurrence| occurrence.state)
                .collect::<Vec<_>>(),
            vec![
                AutomationOccurrenceState::Pending,
                AutomationOccurrenceState::QueuedOverlap,
                AutomationOccurrenceState::SkippedOverlap,
            ]
        );
        let history: (i64, String) = connection
            .query_row(
                "SELECT status,summary FROM automation_history
                 WHERE automation_id='automation-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("atomic overlap history");
        assert_eq!(history, (3, "Skipped by overlap policy.".into()));
        let mut conflicting_replay = evaluate.clone();
        conflicting_replay.command.fingerprint = [99; 32];
        assert_eq!(
            commit_evaluation(&mut connection, conflicting_replay),
            Err(StoreError::Conflict)
        );
        let mut different_key = evaluate.clone();
        different_key.command.key = "evaluate-again".into();
        assert_eq!(
            commit_evaluation(&mut connection, different_key),
            Err(StoreError::Conflict)
        );

        let mut last_claim = None;
        let mut last_claimed = None;
        for attempt in 0_u8..16 {
            let current = query_occurrence(&connection, "occurrence-1").expect("pending");
            let claimed_at = due_at + 1 + u64::from(attempt) * 2;
            let claim = ClaimAutomationOccurrence {
                lease: lease.token(),
                occurrence_id: current.id,
                expected_revision: current.revision,
                claimed_at,
                expires_at: claimed_at + 1,
                command: command(
                    "automation_scheduler_claim_v1",
                    &format!("claim-{attempt}"),
                    attempt + 10,
                ),
            };
            let claimed = claim_occurrence(&mut connection, &claim).expect("claim");
            assert_eq!(claimed.state, AutomationOccurrenceState::Claimed);
            let recovered = recover_claims(&mut connection, &lease.token(), claim.expires_at, 100)
                .expect("recover");
            if attempt == 15 {
                assert_eq!(recovered.attempts_exhausted, 1);
                last_claim = Some(claim);
                last_claimed = Some(claimed);
            } else {
                assert_eq!(recovered.released_unlinked, 1);
            }
        }
        let first_current = query_occurrence(&connection, "occurrence-1").expect("first current");
        let promoted = query_occurrence(&connection, "occurrence-2").expect("promoted queued");
        assert_eq!(
            first_current.state,
            AutomationOccurrenceState::InterruptedNeedsReview
        );
        assert_eq!(promoted.state, AutomationOccurrenceState::Pending);
        assert_eq!(promoted.revision, 2);
        let attempt_count: u32 = connection
            .query_row(
                "SELECT count(*) FROM automation_occurrence_claim_attempts
                 WHERE occurrence_id='occurrence-1'",
                [],
                |row| row.get(0),
            )
            .expect("claim attempt evidence");
        assert_eq!(attempt_count, 16);
        drop(connection);

        let key = DatabaseKey::from_slice(&[61; 32]).expect("key");
        let mut reopened = schema::open_encrypted(&directory.path().join("state.db"), &key)
            .expect("reopen scheduler database");
        let restored = list_occurrences(&reopened, automation.id.as_str(), None, 100)
            .expect("restored occurrences");
        assert_eq!(restored.len(), 3);
        assert_eq!(
            list_occurrences(&reopened, automation.id.as_str(), None, 0),
            Err(StoreError::Conflict)
        );
        assert_eq!(
            list_occurrences(&reopened, automation.id.as_str(), None, 101),
            Err(StoreError::Conflict)
        );
        assert_eq!(
            claim_occurrence(&mut reopened, last_claim.as_ref().expect("last claim"))
                .expect("claim replay after restart"),
            last_claimed.expect("last claimed result")
        );
        assert_eq!(
            commit_evaluation(&mut reopened, evaluate).expect("evaluation replay after restart"),
            evaluated
        );
        assert_eq!(
            commit_evaluation(&mut reopened, initialize).expect("init replay after restart"),
            initialized
        );
        let status = journal_status(&mut reopened).expect("journal status");
        assert_eq!(status.cursor_count, 1);
        assert_eq!(status.pending_count, 1);
        assert_eq!(status.needs_review_count, 1);
    }

    #[test]
    fn sql_scheduler_records_actual_missed_policy_history_atomically() {
        let (_directory, mut connection, mut automation) = scheduler_fixture();
        connection
            .execute(
                "UPDATE automations SET missed_run_policy=1 WHERE id=?1",
                [automation.id.as_str()],
            )
            .expect("set missed-run policy");
        automation.missed_run_policy = MissedRunPolicy::Skip;
        let schedule = AutomationSchedule::parse_canonical(&automation.schedule).expect("schedule");
        let decision = schedule
            .next_decision_after(&automation.timezone, automation.updated_at)
            .expect("decision");
        let due_at = decision.scheduled_for().expect("due timestamp");
        let owner = AutomationSchedulerOwnerId::new("missed-owner").expect("owner");
        let acquired = acquire_lease(&mut connection, owner, due_at, 60_000).expect("lease");
        let AutomationSchedulerLeaseAcquisition::Acquired { lease, .. } = acquired else {
            panic!("expected acquired lease");
        };
        let snapshot = AutomationExecutionSnapshot::new(
            automation.revision,
            automation.project_id.clone(),
            automation.title.clone(),
            automation.prompt.clone(),
            automation.schedule.clone(),
            automation.timezone.clone(),
            automation.missed_run_policy,
            automation.overlap_policy,
        )
        .expect("snapshot");
        let mut cursor = AutomationScheduleCursor::new(
            automation.id.clone(),
            &snapshot,
            automation.updated_at,
            Some(decision),
            due_at,
        )
        .expect("initial cursor");
        commit_evaluation(
            &mut connection,
            AutomationScheduleEvaluationCommit {
                lease: lease.token(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: None,
                cursor: cursor.clone(),
                occurrences: Vec::new(),
                observed_at: due_at,
                command: command("automation_scheduler_evaluate_v1", "missed-init", 31),
            },
        )
        .expect("initialize");
        let next = schedule
            .next_decision_after(&automation.timezone, due_at)
            .expect("next decision");
        cursor.advance(due_at, Some(next), due_at).expect("advance");
        let mut occurrence = AutomationOccurrence::pending(
            AutomationOccurrenceId::new("missed-occurrence").expect("occurrence id"),
            automation.id.clone(),
            snapshot,
            decision,
            1,
            due_at,
        )
        .expect("occurrence");
        occurrence.skip_missed(due_at).expect("skip missed");
        let result = commit_evaluation(
            &mut connection,
            AutomationScheduleEvaluationCommit {
                lease: lease.token(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: Some(0),
                cursor,
                occurrences: vec![occurrence],
                observed_at: due_at,
                command: command("automation_scheduler_evaluate_v1", "missed-eval", 32),
            },
        )
        .expect("commit missed skip");
        assert_eq!(
            result.occurrences[0].state,
            AutomationOccurrenceState::SkippedMissed
        );
        let history: (i64, String) = connection
            .query_row(
                "SELECT status,summary FROM automation_history
                 WHERE automation_id='automation-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("missed history");
        assert_eq!(history, (2, "Skipped by missed-run policy.".into()));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn sql_scheduler_run_link_recovery_requires_evidence_and_promotes_queue() {
        let (_directory, mut connection, automation) = scheduler_fixture();
        let schedule = AutomationSchedule::parse_canonical(&automation.schedule).expect("schedule");
        let first = schedule
            .next_decision_after(&automation.timezone, automation.updated_at)
            .expect("first decision");
        let second = schedule
            .next_decision_after(
                &automation.timezone,
                first.scheduled_for().expect("first due"),
            )
            .expect("second decision");
        let observed_at = second.scheduled_for().expect("second due");
        let third = schedule
            .next_decision_after(&automation.timezone, observed_at)
            .expect("third decision");
        let owner = AutomationSchedulerOwnerId::new("run-link-owner").expect("owner");
        let acquired = acquire_lease(&mut connection, owner, observed_at, 60_000).expect("lease");
        let AutomationSchedulerLeaseAcquisition::Acquired { lease, .. } = acquired else {
            panic!("expected acquired lease");
        };
        let snapshot = AutomationExecutionSnapshot::new(
            automation.revision,
            automation.project_id.clone(),
            automation.title.clone(),
            automation.prompt.clone(),
            automation.schedule.clone(),
            automation.timezone.clone(),
            automation.missed_run_policy,
            automation.overlap_policy,
        )
        .expect("snapshot");
        let mut cursor = AutomationScheduleCursor::new(
            automation.id.clone(),
            &snapshot,
            automation.updated_at,
            Some(first),
            observed_at,
        )
        .expect("cursor");
        commit_evaluation(
            &mut connection,
            AutomationScheduleEvaluationCommit {
                lease: lease.token(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: None,
                cursor: cursor.clone(),
                occurrences: Vec::new(),
                observed_at,
                command: command("automation_scheduler_evaluate_v1", "run-link-init", 51),
            },
        )
        .expect("initialize");
        cursor
            .advance(observed_at, Some(third), observed_at)
            .expect("advance");
        let occurrences = [first, second]
            .into_iter()
            .enumerate()
            .map(|(index, decision)| {
                AutomationOccurrence::pending(
                    AutomationOccurrenceId::new(format!("run-link-occurrence-{}", index + 1))
                        .expect("occurrence id"),
                    automation.id.clone(),
                    snapshot.clone(),
                    decision,
                    1,
                    observed_at,
                )
                .expect("occurrence")
            })
            .collect();
        let evaluated = commit_evaluation(
            &mut connection,
            AutomationScheduleEvaluationCommit {
                lease: lease.token(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: Some(0),
                cursor,
                occurrences,
                observed_at,
                command: command("automation_scheduler_evaluate_v1", "run-link-eval", 52),
            },
        )
        .expect("evaluate");
        let claim = ClaimAutomationOccurrence {
            lease: lease.token(),
            occurrence_id: evaluated.occurrences[0].id.clone(),
            expected_revision: 0,
            claimed_at: observed_at + 1,
            expires_at: observed_at + 100,
            command: command("automation_scheduler_claim_v1", "run-link-claim", 53),
        };
        let claimed = claim_occurrence(&mut connection, &claim).expect("claim");
        let linked_at = observed_at + 2;
        connection
            .execute_batch(&format!(
                "INSERT INTO threads(
                     id,project_id,title,state,revision,created_at,updated_at
                 ) VALUES ('run-link-thread','project-1','Run link',0,0,{linked_at},{linked_at});
                 INSERT INTO runs(
                     id,project_id,thread_id,state,revision,created_at,updated_at
                 ) VALUES (
                     'run-link-run','project-1','run-link-thread',0,0,{linked_at},{linked_at}
                 );"
            ))
            .expect("linked run owner");
        connection
            .execute(
                "UPDATE automation_occurrences
                 SET state=3,run_id='run-link-run',revision=?1,updated_at=?2
                 WHERE id=?3",
                params![
                    number(claimed.revision + 1).expect("linked revision"),
                    number(linked_at).expect("linked time"),
                    claimed.id.as_str(),
                ],
            )
            .expect("link run");
        assert!(matches!(
            recover_claims(&mut connection, &lease.token(), claim.expires_at, 100),
            Err(StoreError::Internal(_))
        ));
        assert_eq!(
            query_occurrence(&connection, claimed.id.as_str())
                .expect("rolled-back linked occurrence")
                .state,
            AutomationOccurrenceState::RunLinked
        );
        assert_eq!(
            query_occurrence(&connection, "run-link-occurrence-2")
                .expect("rolled-back queue")
                .state,
            AutomationOccurrenceState::QueuedOverlap
        );
        connection
            .execute(
                "UPDATE automation_occurrence_claim_attempts
                 SET completed_at=?1,outcome=1
                 WHERE occurrence_id=?2 AND sequence=1",
                params![number(linked_at).expect("linked time"), claimed.id.as_str()],
            )
            .expect("complete run-linked claim evidence");

        let summary = recover_claims(&mut connection, &lease.token(), claim.expires_at, 100)
            .expect("recover linked claim");
        assert_eq!(summary.interrupted_linked, 1);
        let interrupted = query_occurrence(&connection, claimed.id.as_str()).expect("interrupted");
        let promoted =
            query_occurrence(&connection, "run-link-occurrence-2").expect("promoted occurrence");
        assert_eq!(
            interrupted.state,
            AutomationOccurrenceState::InterruptedNeedsReview
        );
        assert_eq!(promoted.state, AutomationOccurrenceState::Pending);
        let outcome: i64 = connection
            .query_row(
                "SELECT outcome FROM automation_occurrence_claim_attempts
                 WHERE occurrence_id=?1 AND sequence=1",
                [claimed.id.as_str()],
                |row| row.get(0),
            )
            .expect("linked evidence outcome");
        assert_eq!(outcome, 1);
    }

    #[test]
    fn sql_scheduler_rejects_an_occurrence_outside_the_exact_advanced_window() {
        let (_directory, mut connection, automation) = scheduler_fixture();
        let schedule = AutomationSchedule::parse_canonical(&automation.schedule).expect("schedule");
        let first = schedule
            .next_decision_after(&automation.timezone, automation.updated_at)
            .expect("first decision");
        let first_due = first.scheduled_for().expect("first due");
        let second = schedule
            .next_decision_after(&automation.timezone, first_due)
            .expect("second decision");
        let second_due = second.scheduled_for().expect("second due");
        let third = schedule
            .next_decision_after(&automation.timezone, second_due)
            .expect("third decision");
        let owner = AutomationSchedulerOwnerId::new("window-owner").expect("owner");
        let acquired = acquire_lease(&mut connection, owner, second_due, 60_000).expect("lease");
        let AutomationSchedulerLeaseAcquisition::Acquired { lease, .. } = acquired else {
            panic!("expected acquired lease");
        };
        let snapshot = AutomationExecutionSnapshot::new(
            automation.revision,
            automation.project_id.clone(),
            automation.title.clone(),
            automation.prompt.clone(),
            automation.schedule.clone(),
            automation.timezone.clone(),
            automation.missed_run_policy,
            automation.overlap_policy,
        )
        .expect("snapshot");
        let mut cursor = AutomationScheduleCursor::new(
            automation.id.clone(),
            &snapshot,
            automation.updated_at,
            Some(first),
            second_due,
        )
        .expect("initial cursor");
        commit_evaluation(
            &mut connection,
            AutomationScheduleEvaluationCommit {
                lease: lease.token(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: None,
                cursor: cursor.clone(),
                occurrences: Vec::new(),
                observed_at: second_due,
                command: command("automation_scheduler_evaluate_v1", "window-init", 41),
            },
        )
        .expect("initialize");
        cursor
            .advance(first_due, Some(second), second_due)
            .expect("first advance");
        commit_evaluation(
            &mut connection,
            AutomationScheduleEvaluationCommit {
                lease: lease.token(),
                expected_automation_revision: automation.revision,
                expected_cursor_revision: Some(0),
                cursor: cursor.clone(),
                occurrences: Vec::new(),
                observed_at: second_due,
                command: command("automation_scheduler_evaluate_v1", "window-first", 42),
            },
        )
        .expect("advance past first slot");
        cursor
            .advance(second_due, Some(third), second_due)
            .expect("second advance");
        let old_occurrence = AutomationOccurrence::pending(
            AutomationOccurrenceId::new("old-occurrence").expect("occurrence id"),
            automation.id,
            snapshot,
            first,
            1,
            second_due,
        )
        .expect("old occurrence");
        assert_eq!(
            commit_evaluation(
                &mut connection,
                AutomationScheduleEvaluationCommit {
                    lease: lease.token(),
                    expected_automation_revision: automation.revision,
                    expected_cursor_revision: Some(1),
                    cursor,
                    occurrences: vec![old_occurrence],
                    observed_at: second_due,
                    command: command("automation_scheduler_evaluate_v1", "window-second", 43),
                },
            ),
            Err(StoreError::Conflict)
        );
        let durable = query_cursor_optional(&connection, "automation-1")
            .expect("cursor query")
            .expect("cursor");
        assert_eq!(durable.revision, 1);
    }

    #[test]
    fn sql_scheduler_candidate_limits_and_durable_clock_floor_are_strict() {
        let (_directory, mut connection, _automation) = scheduler_fixture();
        assert_eq!(
            list_candidates(&connection, None, 0),
            Err(StoreError::Conflict)
        );
        assert_eq!(
            list_candidates(
                &connection,
                None,
                MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS + 1,
            )
            .expect("lookahead candidate")
            .len(),
            1
        );
        assert_eq!(
            list_candidates(
                &connection,
                None,
                MAX_AUTOMATION_SCHEDULER_TICK_DEFINITIONS + 2,
            ),
            Err(StoreError::Conflict)
        );
        let owner = AutomationSchedulerOwnerId::new("scheduler-owner").expect("owner");
        let acquired = acquire_lease(&mut connection, owner.clone(), 100, 100).expect("lease");
        assert!(matches!(
            acquired,
            AutomationSchedulerLeaseAcquisition::Acquired { .. }
        ));
        let regressed = acquire_lease(&mut connection, owner, 99, 100).expect("clock result");
        assert_eq!(
            regressed,
            AutomationSchedulerLeaseAcquisition::ClockRegressed { durable_floor: 100 }
        );
        let lease: (String, i64) = connection
            .query_row(
                "SELECT owner_id,fence FROM automation_scheduler_lease WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("unchanged lease");
        assert_eq!(lease, ("scheduler-owner".into(), 1));
    }
}
