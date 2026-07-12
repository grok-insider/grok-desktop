use async_trait::async_trait;
use grok_application::{
    PrivilegedDispatchAttempt, PrivilegedOperationStore, PrivilegedPreparation,
    PrivilegedRecoveryCandidate, StoreError,
};
use grok_domain::{
    ApprovalId, AuthorityGrantId, EffectId, PayloadDigest, PrivilegedAuthority,
    PrivilegedIdempotency, PrivilegedIdempotencyKey, PrivilegedOperation, PrivilegedOperationId,
    PrivilegedOperationKind, PrivilegedOperationLinks, PrivilegedOperationReview,
    PrivilegedOperationState, PrivilegedOperationTarget, PrivilegedResourceId,
    PrivilegedRetryClass, RequestDigest, RunId, UnixMillis,
};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use sha2::{Digest, Sha256};

use crate::{
    SqlCipherStore,
    store::{map_sqlite, number},
};

const OPERATION_COLUMNS: &str = "id,operation_kind,retry_class,target_vm_id,\
    target_integration_id,target_instance_id,target_application_id,\
    target_observation_revision,payload_digest,retained_payload_digest,\
    authority_grant_id,authority_expires_at,idempotency_key,request_digest,\
    run_id,effect_id,approval_id,supersedes_id,state,review_disposition,\
    attempt_count,last_attempt_sequence,last_attempt_certainty,\
    terminal_result_digest,terminal_result_payload,terminal_result_pruned,\
    revision,created_at,updated_at";

#[derive(Debug)]
struct OperationRow {
    id: String,
    kind: i64,
    retry_class: i64,
    vm_id: String,
    integration_id: Option<String>,
    instance_id: Option<String>,
    application_id: Option<String>,
    observation_revision: Option<i64>,
    payload_digest: Vec<u8>,
    retained_payload_digest: Option<Vec<u8>>,
    authority_grant_id: String,
    authority_expires_at: i64,
    idempotency_key: String,
    request_digest: Vec<u8>,
    run_id: Option<String>,
    effect_id: Option<String>,
    approval_id: Option<String>,
    supersedes_id: Option<String>,
    state: i64,
    review: Option<i64>,
    attempt_count: i64,
    last_attempt_sequence: Option<i64>,
    last_attempt_certainty: Option<i64>,
    terminal_result_digest: Option<Vec<u8>>,
    terminal_result_payload: Option<Vec<u8>>,
    terminal_result_pruned: i64,
    revision: i64,
    created_at: i64,
    updated_at: i64,
}

#[async_trait]
impl PrivilegedOperationStore for SqlCipherStore {
    async fn resolve_preparation(
        &self,
        intent: &grok_domain::PrivilegedOperationIntent,
    ) -> Result<Option<PrivilegedOperation>, StoreError> {
        let intent = intent.clone();
        self.with_store(move |connection| {
            let existing = load_operation_by_key(
                connection,
                intent.authority.grant_id.as_str(),
                intent.idempotency.key.as_str(),
            )?;
            match existing {
                Some(existing) if exact_intent(&existing, &intent) => Ok(Some(existing)),
                Some(_) => Err(StoreError::Conflict),
                None => Ok(None),
            }
        })
        .await
    }

    async fn prepare_with_payload(
        &self,
        operation: PrivilegedOperation,
        payload: Vec<u8>,
    ) -> Result<PrivilegedPreparation, StoreError> {
        if operation.state != PrivilegedOperationState::Prepared {
            return Err(invalid_journal());
        }
        validate_payload(&operation, &payload)?;
        let operation = validated_operation(operation)?;
        self.with_store(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            if let Some(existing) = load_operation_by_key(
                &transaction,
                operation.authority.grant_id.as_str(),
                operation.idempotency.key.as_str(),
            )? {
                if !exact_replay(&existing, &operation) {
                    return Err(StoreError::Conflict);
                }
                return Ok(PrivilegedPreparation {
                    operation: existing,
                    created: false,
                });
            }
            insert_operation(&transaction, &operation)?;
            transaction
                .execute(
                    "INSERT INTO privileged_operation_payloads(
                         operation_id,payload_digest,payload,created_at
                     ) VALUES (?1,?2,?3,?4)",
                    params![
                        operation.id.as_str(),
                        operation.payload_digest.as_bytes().as_slice(),
                        payload,
                        number(operation.created_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            transaction.commit().map_err(map_sqlite)?;
            let committed = load_operation_by_id(connection, &operation.id)?;
            if committed != operation {
                return Err(invalid_journal());
            }
            Ok(PrivilegedPreparation {
                operation: committed,
                created: true,
            })
        })
        .await
    }

    async fn get_privileged_operation(
        &self,
        id: &PrivilegedOperationId,
    ) -> Result<PrivilegedOperation, StoreError> {
        let id = id.clone();
        self.with_store(move |connection| load_operation_by_id(connection, &id))
            .await
    }

    async fn begin_dispatch_with_attempt(
        &self,
        operation: PrivilegedOperation,
        expected_revision: u64,
        attempt: PrivilegedDispatchAttempt,
    ) -> Result<PrivilegedOperation, StoreError> {
        let operation = validated_operation(operation)?;
        validate_attempt(&operation, &attempt)?;
        self.with_store(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            let current = load_operation_by_id(&transaction, &operation.id)?;
            let mut expected = current.clone();
            if current.revision != expected_revision
                || expected.dispatch(attempt.started_at).is_err()
                || expected != operation
            {
                return Err(StoreError::Conflict);
            }
            let changed = transaction
                .execute(
                    "UPDATE privileged_operations SET
                         state=1,attempt_count=?1,last_attempt_sequence=?1,
                         last_attempt_certainty=0,revision=?2,updated_at=?3
                     WHERE id=?4 AND revision=?5",
                    params![
                        i64::from(operation.attempt_count),
                        number(operation.revision)?,
                        number(operation.updated_at)?,
                        operation.id.as_str(),
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            if changed != 1 {
                return Err(StoreError::Conflict);
            }
            transaction
                .execute(
                    "INSERT INTO privileged_operation_attempts(
                         operation_id,sequence,transport_operation_id,wire_digest,
                         broker_boot_id,guest_boot_id,started_at,deadline_unix_ms,
                         completed_at,outcome_certainty,result_digest,failure_code
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,NULL,0,NULL,NULL)",
                    params![
                        operation.id.as_str(),
                        i64::from(attempt.sequence),
                        attempt.transport_operation_id,
                        attempt.wire_digest.as_slice(),
                        attempt.broker_boot_id.as_slice(),
                        attempt.guest_boot_id.as_slice(),
                        number(attempt.started_at)?,
                        number(attempt.deadline_unix_ms)?,
                    ],
                )
                .map_err(map_sqlite)?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(operation)
        })
        .await
    }

    async fn list_dispatching_for_recovery(
        &self,
        limit: usize,
    ) -> Result<Vec<PrivilegedRecoveryCandidate>, StoreError> {
        self.with_store(move |connection| {
            let limit = i64::try_from(limit)
                .map_err(|_| StoreError::Internal("recovery limit out of range".into()))?;
            let mut statement = connection
                .prepare(
                    "SELECT id FROM privileged_operations WHERE state=1
                     ORDER BY updated_at,id LIMIT ?1",
                )
                .map_err(map_sqlite)?;
            let ids = statement
                .query_map([limit], |row| row.get::<_, String>(0))
                .map_err(map_sqlite)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_sqlite)?;
            drop(statement);
            ids.into_iter()
                .map(|id| recovery_candidate(connection, &id))
                .collect()
        })
        .await
    }

    async fn recover_interrupted_attempt(
        &self,
        operation: PrivilegedOperation,
        expected_revision: u64,
        attempt_sequence: u32,
        completed_at: UnixMillis,
    ) -> Result<PrivilegedOperation, StoreError> {
        let operation = validated_operation(operation)?;
        self.with_store(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            let current = load_operation_by_id(&transaction, &operation.id)?;
            let mut expected = current.clone();
            if current.revision != expected_revision
                || current.attempt_count != attempt_sequence
                || expected.interrupt(completed_at).is_err()
                || expected != operation
            {
                return Err(StoreError::Conflict);
            }
            let changed = transaction
                .execute(
                    "UPDATE privileged_operations SET
                         state=?1,last_attempt_certainty=4,revision=?2,updated_at=?3
                     WHERE id=?4 AND revision=?5 AND state=1
                       AND last_attempt_sequence=?6 AND last_attempt_certainty=0",
                    params![
                        state_number(operation.state),
                        number(operation.revision)?,
                        number(operation.updated_at)?,
                        operation.id.as_str(),
                        number(expected_revision)?,
                        i64::from(attempt_sequence),
                    ],
                )
                .map_err(map_sqlite)?;
            if changed != 1 {
                return Err(StoreError::Conflict);
            }
            let changed = transaction
                .execute(
                    "UPDATE privileged_operation_attempts
                     SET completed_at=?1,outcome_certainty=4
                     WHERE operation_id=?2 AND sequence=?3 AND outcome_certainty=0",
                    params![
                        number(completed_at)?,
                        operation.id.as_str(),
                        i64::from(attempt_sequence),
                    ],
                )
                .map_err(map_sqlite)?;
            if changed != 1 {
                return Err(StoreError::Conflict);
            }
            transaction.commit().map_err(map_sqlite)?;
            Ok(operation)
        })
        .await
    }

    async fn complete_dispatch_outcome(
        &self,
        operation: PrivilegedOperation,
        expected_revision: u64,
        attempt_sequence: u32,
        completed_at: UnixMillis,
    ) -> Result<PrivilegedOperation, StoreError> {
        let operation = validated_operation(operation)?;
        match operation.state {
            PrivilegedOperationState::Succeeded
            | PrivilegedOperationState::Failed
            | PrivilegedOperationState::RetryPending
            | PrivilegedOperationState::InterruptedNeedsReview => {}
            _ => {
                return Err(StoreError::Internal(
                    "complete_dispatch_outcome requires a terminal operation state".into(),
                ));
            }
        }
        self.with_store(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            let current = load_operation_by_id(&transaction, &operation.id)?;
            if current.revision != expected_revision
                || current.attempt_count != attempt_sequence
                || current.state != PrivilegedOperationState::Dispatching
            {
                return Err(StoreError::Conflict);
            }
            let certainty: i64 = match operation.state {
                PrivilegedOperationState::Succeeded | PrivilegedOperationState::Failed => 1,
                PrivilegedOperationState::RetryPending
                | PrivilegedOperationState::InterruptedNeedsReview => 4,
                _ => 0,
            };
            let changed = transaction
                .execute(
                    "UPDATE privileged_operations SET
                         state=?1,last_attempt_certainty=?2,revision=?3,updated_at=?4
                     WHERE id=?5 AND revision=?6 AND state=1
                       AND last_attempt_sequence=?7 AND last_attempt_certainty=0",
                    params![
                        state_number(operation.state),
                        certainty,
                        number(operation.revision)?,
                        number(operation.updated_at)?,
                        operation.id.as_str(),
                        number(expected_revision)?,
                        i64::from(attempt_sequence),
                    ],
                )
                .map_err(map_sqlite)?;
            if changed != 1 {
                return Err(StoreError::Conflict);
            }
            let changed = transaction
                .execute(
                    "UPDATE privileged_operation_attempts
                     SET completed_at=?1,outcome_certainty=?2
                     WHERE operation_id=?3 AND sequence=?4 AND outcome_certainty=0",
                    params![
                        number(completed_at)?,
                        certainty,
                        operation.id.as_str(),
                        i64::from(attempt_sequence),
                    ],
                )
                .map_err(map_sqlite)?;
            if changed != 1 {
                return Err(StoreError::Conflict);
            }
            transaction.commit().map_err(map_sqlite)?;
            Ok(operation)
        })
        .await
    }
}

fn insert_operation(
    connection: &Connection,
    operation: &PrivilegedOperation,
) -> Result<(), StoreError> {
    let target = target_parts(&operation.target);
    connection
        .execute(
            "INSERT INTO privileged_operations(
                 id,operation_kind,retry_class,target_vm_id,target_integration_id,
                 target_instance_id,target_application_id,target_observation_revision,
                 payload_digest,retained_payload_digest,authority_grant_id,
                 authority_expires_at,idempotency_key,request_digest,run_id,effect_id,
                 approval_id,supersedes_id,state,review_disposition,attempt_count,
                 last_attempt_sequence,last_attempt_certainty,terminal_result_digest,
                 terminal_result_payload,terminal_result_pruned,revision,created_at,updated_at
             ) VALUES (
                 ?1,?2,?3,?4,?5,?6,?7,?8,?9,?9,?10,?11,?12,?13,?14,?15,?16,
                 ?17,0,NULL,0,NULL,NULL,NULL,NULL,0,0,?18,?18
             )",
            params![
                operation.id.as_str(),
                kind_number(operation.kind),
                retry_number(operation.retry_class()),
                target.vm_id,
                target.integration_id,
                target.instance_id,
                target.application_id,
                target.observation_revision.map(number).transpose()?,
                operation.payload_digest.as_bytes().as_slice(),
                operation.authority.grant_id.as_str(),
                number(operation.authority.expires_at)?,
                operation.idempotency.key.as_str(),
                operation.idempotency.request_digest.as_bytes().as_slice(),
                operation.links.run_id.as_ref().map(RunId::as_str),
                operation.links.effect_id.as_ref().map(EffectId::as_str),
                operation.links.approval_id.as_ref().map(ApprovalId::as_str),
                operation
                    .links
                    .supersedes_id
                    .as_ref()
                    .map(PrivilegedOperationId::as_str),
                number(operation.created_at)?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

struct TargetParts<'a> {
    vm_id: &'a str,
    integration_id: Option<&'a str>,
    instance_id: Option<&'a str>,
    application_id: Option<&'a str>,
    observation_revision: Option<u64>,
}

fn target_parts(target: &PrivilegedOperationTarget) -> TargetParts<'_> {
    match target {
        PrivilegedOperationTarget::Runner { vm_id }
        | PrivilegedOperationTarget::Catalog { vm_id } => TargetParts {
            vm_id: vm_id.as_str(),
            integration_id: None,
            instance_id: None,
            application_id: None,
            observation_revision: None,
        },
        PrivilegedOperationTarget::IntegrationStart {
            vm_id,
            integration_id,
        }
        | PrivilegedOperationTarget::ComputerObserve {
            vm_id,
            integration_id,
        } => TargetParts {
            vm_id: vm_id.as_str(),
            integration_id: Some(integration_id.as_str()),
            instance_id: None,
            application_id: None,
            observation_revision: None,
        },
        PrivilegedOperationTarget::IntegrationStop {
            vm_id,
            integration_id,
            instance_id,
        } => TargetParts {
            vm_id: vm_id.as_str(),
            integration_id: Some(integration_id.as_str()),
            instance_id: Some(instance_id.as_str()),
            application_id: None,
            observation_revision: None,
        },
        PrivilegedOperationTarget::ComputerAct {
            vm_id,
            integration_id,
            instance_id,
            application_id,
            observation_revision,
        } => TargetParts {
            vm_id: vm_id.as_str(),
            integration_id: Some(integration_id.as_str()),
            instance_id: Some(instance_id.as_str()),
            application_id: Some(application_id.as_str()),
            observation_revision: Some(*observation_revision),
        },
    }
}

fn load_operation_by_id(
    connection: &Connection,
    id: &PrivilegedOperationId,
) -> Result<PrivilegedOperation, StoreError> {
    let sql = format!("SELECT {OPERATION_COLUMNS} FROM privileged_operations WHERE id=?1");
    let raw = connection
        .query_row(&sql, [id.as_str()], operation_row)
        .optional()
        .map_err(map_sqlite)?;
    raw.map(|raw| operation_from_row(connection, raw))
        .transpose()?
        .ok_or(StoreError::NotFound)
}

fn load_operation_by_key(
    connection: &Connection,
    grant_id: &str,
    key: &str,
) -> Result<Option<PrivilegedOperation>, StoreError> {
    let sql = format!(
        "SELECT {OPERATION_COLUMNS} FROM privileged_operations
         WHERE authority_grant_id=?1 AND idempotency_key=?2"
    );
    let raw = connection
        .query_row(&sql, params![grant_id, key], operation_row)
        .optional()
        .map_err(map_sqlite)?;
    raw.map(|raw| operation_from_row(connection, raw))
        .transpose()
}

#[allow(clippy::too_many_lines)]
fn operation_from_row(
    connection: &Connection,
    raw: OperationRow,
) -> Result<PrivilegedOperation, StoreError> {
    let kind = parse_kind(raw.kind)?;
    if parse_retry(raw.retry_class)? != kind.retry_class() {
        return Err(invalid_journal());
    }
    let payload_digest = exact_digest_slice(&raw.payload_digest)?;
    let state = parse_state(raw.state)?;
    validate_storage_metadata(&raw, state, &payload_digest)?;
    if let Some(payload) = raw.terminal_result_payload.as_deref() {
        let digest = raw
            .terminal_result_digest
            .as_deref()
            .ok_or_else(invalid_journal)?;
        if Sha256::digest(payload).as_slice() != digest {
            return Err(invalid_journal());
        }
    }
    let target = parse_target(
        kind,
        raw.vm_id,
        raw.integration_id,
        raw.instance_id,
        raw.application_id,
        raw.observation_revision,
    )?;
    let operation = PrivilegedOperation {
        id: PrivilegedOperationId::new(raw.id).map_err(|_| invalid_journal())?,
        kind,
        target,
        payload_digest: PayloadDigest::new(payload_digest),
        authority: PrivilegedAuthority::new(
            AuthorityGrantId::new(raw.authority_grant_id).map_err(|_| invalid_journal())?,
            unsigned(raw.authority_expires_at)?,
        ),
        idempotency: PrivilegedIdempotency::new(
            PrivilegedIdempotencyKey::new(raw.idempotency_key).map_err(|_| invalid_journal())?,
            RequestDigest::new(exact_digest(raw.request_digest)?),
        ),
        links: PrivilegedOperationLinks {
            run_id: optional_id(raw.run_id, RunId::new)?,
            effect_id: optional_id(raw.effect_id, EffectId::new)?,
            approval_id: optional_id(raw.approval_id, ApprovalId::new)?,
            supersedes_id: optional_id(raw.supersedes_id, PrivilegedOperationId::new)?,
        },
        state,
        review: parse_review(raw.review)?,
        attempt_count: u32::try_from(raw.attempt_count).map_err(|_| invalid_journal())?,
        revision: unsigned(raw.revision)?,
        created_at: unsigned(raw.created_at)?,
        updated_at: unsigned(raw.updated_at)?,
    };
    let operation = validated_operation(operation)?;
    validate_retained_payload(
        connection,
        &operation,
        raw.retained_payload_digest.is_some(),
    )?;
    Ok(operation)
}

fn operation_row(row: &Row<'_>) -> rusqlite::Result<OperationRow> {
    Ok(OperationRow {
        id: row.get(0)?,
        kind: row.get(1)?,
        retry_class: row.get(2)?,
        vm_id: row.get(3)?,
        integration_id: row.get(4)?,
        instance_id: row.get(5)?,
        application_id: row.get(6)?,
        observation_revision: row.get(7)?,
        payload_digest: row.get(8)?,
        retained_payload_digest: row.get(9)?,
        authority_grant_id: row.get(10)?,
        authority_expires_at: row.get(11)?,
        idempotency_key: row.get(12)?,
        request_digest: row.get(13)?,
        run_id: row.get(14)?,
        effect_id: row.get(15)?,
        approval_id: row.get(16)?,
        supersedes_id: row.get(17)?,
        state: row.get(18)?,
        review: row.get(19)?,
        attempt_count: row.get(20)?,
        last_attempt_sequence: row.get(21)?,
        last_attempt_certainty: row.get(22)?,
        terminal_result_digest: row.get(23)?,
        terminal_result_payload: row.get(24)?,
        terminal_result_pruned: row.get(25)?,
        revision: row.get(26)?,
        created_at: row.get(27)?,
        updated_at: row.get(28)?,
    })
}

fn recovery_candidate(
    connection: &Connection,
    id: &str,
) -> Result<PrivilegedRecoveryCandidate, StoreError> {
    let id = PrivilegedOperationId::new(id).map_err(|_| invalid_journal())?;
    let operation = load_operation_by_id(connection, &id)?;
    if operation.state != PrivilegedOperationState::Dispatching {
        return Err(invalid_journal());
    }
    let attempt = connection
        .query_row(
            "SELECT sequence,transport_operation_id,wire_digest,broker_boot_id,guest_boot_id,
                    started_at,deadline_unix_ms,completed_at,outcome_certainty,result_digest,
                    failure_code
             FROM privileged_operation_attempts
             WHERE operation_id=?1 AND sequence=?2",
            params![operation.id.as_str(), i64::from(operation.attempt_count)],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<Vec<u8>>>(9)?,
                    row.get::<_, Option<String>>(10)?,
                ))
            },
        )
        .map_err(map_sqlite)?;
    let sequence = u32::try_from(attempt.0).map_err(|_| invalid_journal())?;
    let started_at = unsigned(attempt.5)?;
    let deadline = unsigned(attempt.6)?;
    let dispatch = PrivilegedDispatchAttempt {
        sequence,
        transport_operation_id: attempt.1,
        wire_digest: exact_digest(attempt.2)?,
        broker_boot_id: exact_array(attempt.3)?,
        guest_boot_id: exact_array(attempt.4)?,
        started_at,
        deadline_unix_ms: deadline,
    };
    if attempt.7.is_some()
        || attempt.8 != 0
        || attempt.9.is_some()
        || attempt.10.is_some()
        || validate_attempt(&operation, &dispatch).is_err()
    {
        return Err(invalid_journal());
    }
    Ok(PrivilegedRecoveryCandidate {
        operation,
        attempt_sequence: sequence,
        attempt_started_at: started_at,
    })
}

fn validate_storage_metadata(
    raw: &OperationRow,
    state: PrivilegedOperationState,
    payload_digest: &[u8; 32],
) -> Result<(), StoreError> {
    let active_payload = matches!(
        state,
        PrivilegedOperationState::Prepared
            | PrivilegedOperationState::Dispatching
            | PrivilegedOperationState::RetryPending
            | PrivilegedOperationState::InterruptedNeedsReview
    );
    let retained_valid = match (active_payload, raw.retained_payload_digest.as_deref()) {
        (true, Some(value)) => value == payload_digest,
        (false, None) => true,
        _ => false,
    };
    let attempt_valid = match state {
        PrivilegedOperationState::Prepared | PrivilegedOperationState::Cancelled => {
            raw.attempt_count == 0
                && raw.last_attempt_sequence.is_none()
                && raw.last_attempt_certainty.is_none()
        }
        PrivilegedOperationState::Dispatching => {
            raw.attempt_count > 0
                && raw.last_attempt_sequence == Some(raw.attempt_count)
                && raw.last_attempt_certainty == Some(0)
        }
        PrivilegedOperationState::RetryPending => {
            raw.attempt_count > 0
                && raw.last_attempt_sequence == Some(raw.attempt_count)
                && matches!(raw.last_attempt_certainty, Some(1 | 3 | 4))
        }
        PrivilegedOperationState::Succeeded => {
            raw.attempt_count > 0
                && raw.last_attempt_sequence == Some(raw.attempt_count)
                && raw.last_attempt_certainty == Some(2)
        }
        PrivilegedOperationState::Failed => {
            raw.attempt_count > 0
                && raw.last_attempt_sequence == Some(raw.attempt_count)
                && matches!(raw.last_attempt_certainty, Some(1 | 3))
        }
        PrivilegedOperationState::InterruptedNeedsReview | PrivilegedOperationState::Reviewed => {
            raw.attempt_count > 0
                && raw.last_attempt_sequence == Some(raw.attempt_count)
                && raw.last_attempt_certainty == Some(4)
        }
    };
    let terminal_valid = match state {
        PrivilegedOperationState::Succeeded | PrivilegedOperationState::Failed => {
            exact_optional_digest(raw.terminal_result_digest.as_deref())
                && (matches!(
                    (raw.terminal_result_pruned, raw.terminal_result_payload.as_deref()),
                    (0, Some(payload)) if (2..=8 * 1024 * 1024).contains(&payload.len())
                ) || (raw.terminal_result_pruned == 1 && raw.terminal_result_payload.is_none()))
        }
        _ => {
            raw.terminal_result_digest.is_none()
                && raw.terminal_result_payload.is_none()
                && raw.terminal_result_pruned == 0
        }
    };
    if retained_valid && attempt_valid && terminal_valid {
        Ok(())
    } else {
        Err(invalid_journal())
    }
}

fn validate_retained_payload(
    connection: &Connection,
    operation: &PrivilegedOperation,
    retained: bool,
) -> Result<(), StoreError> {
    let record = connection
        .query_row(
            "SELECT payload_digest,payload,created_at FROM privileged_operation_payloads
             WHERE operation_id=?1",
            [operation.id.as_str()],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, Vec<u8>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?;
    match (retained, record) {
        (false, None) => Ok(()),
        (true, Some((digest, payload, created_at))) => {
            let digest = exact_digest(digest)?;
            if digest == operation.payload_digest.to_bytes()
                && unsigned(created_at)? == operation.created_at
                && (2..=8 * 1024 * 1024).contains(&payload.len())
                && Sha256::digest(&payload).as_slice() == operation.payload_digest.as_bytes()
            {
                Ok(())
            } else {
                Err(invalid_journal())
            }
        }
        _ => Err(invalid_journal()),
    }
}

fn validate_payload(operation: &PrivilegedOperation, payload: &[u8]) -> Result<(), StoreError> {
    if !(2..=8 * 1024 * 1024).contains(&payload.len())
        || Sha256::digest(payload).as_slice() != operation.payload_digest.as_bytes()
    {
        return Err(invalid_journal());
    }
    Ok(())
}

fn validate_attempt(
    operation: &PrivilegedOperation,
    attempt: &PrivilegedDispatchAttempt,
) -> Result<(), StoreError> {
    let transport_valid = (16..=128).contains(&attempt.transport_operation_id.len())
        && attempt
            .transport_operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'));
    if operation.state != PrivilegedOperationState::Dispatching
        || attempt.sequence != operation.attempt_count
        || attempt.started_at != operation.updated_at
        || !transport_valid
        || attempt.transport_operation_id == operation.id.as_str()
        || attempt.broker_boot_id == [0; 16]
        || attempt.guest_boot_id == [0; 16]
        || attempt
            .deadline_unix_ms
            .checked_sub(attempt.started_at)
            .is_none_or(|duration| duration == 0 || duration > 30_000)
    {
        return Err(invalid_journal());
    }
    Ok(())
}

fn exact_replay(existing: &PrivilegedOperation, proposed: &PrivilegedOperation) -> bool {
    existing.kind == proposed.kind
        && existing.target == proposed.target
        && existing.payload_digest == proposed.payload_digest
        && existing.authority == proposed.authority
        && existing.idempotency == proposed.idempotency
        && existing.links == proposed.links
}

fn exact_intent(
    existing: &PrivilegedOperation,
    intent: &grok_domain::PrivilegedOperationIntent,
) -> bool {
    existing.kind == intent.kind
        && existing.target == intent.target
        && existing.payload_digest == intent.payload_digest
        && existing.authority == intent.authority
        && existing.idempotency == intent.idempotency
        && existing.links == intent.links
}

fn validated_operation(operation: PrivilegedOperation) -> Result<PrivilegedOperation, StoreError> {
    PrivilegedOperation::restore(operation).map_err(|_| invalid_journal())
}

fn parse_target(
    kind: PrivilegedOperationKind,
    vm_id: String,
    integration_id: Option<String>,
    instance_id: Option<String>,
    application_id: Option<String>,
    observation_revision: Option<i64>,
) -> Result<PrivilegedOperationTarget, StoreError> {
    let vm_id = PrivilegedResourceId::new(vm_id).map_err(|_| invalid_journal())?;
    match (
        kind,
        integration_id,
        instance_id,
        application_id,
        observation_revision,
    ) {
        (PrivilegedOperationKind::RunnerHealth, None, None, None, None) => {
            Ok(PrivilegedOperationTarget::Runner { vm_id })
        }
        (PrivilegedOperationKind::CatalogApply, None, None, None, None) => {
            Ok(PrivilegedOperationTarget::Catalog { vm_id })
        }
        (PrivilegedOperationKind::IntegrationStart, Some(integration), None, None, None) => {
            Ok(PrivilegedOperationTarget::IntegrationStart {
                vm_id,
                integration_id: resource(integration)?,
            })
        }
        (
            PrivilegedOperationKind::IntegrationStop,
            Some(integration),
            Some(instance),
            None,
            None,
        ) => Ok(PrivilegedOperationTarget::IntegrationStop {
            vm_id,
            integration_id: resource(integration)?,
            instance_id: resource(instance)?,
        }),
        (PrivilegedOperationKind::ComputerObserve, Some(integration), None, None, None) => {
            Ok(PrivilegedOperationTarget::ComputerObserve {
                vm_id,
                integration_id: resource(integration)?,
            })
        }
        (
            PrivilegedOperationKind::ComputerAct,
            Some(integration),
            Some(instance),
            Some(application),
            Some(observation_revision),
        ) => Ok(PrivilegedOperationTarget::ComputerAct {
            vm_id,
            integration_id: resource(integration)?,
            instance_id: resource(instance)?,
            application_id: resource(application)?,
            observation_revision: u64::try_from(observation_revision)
                .map_err(|_| invalid_journal())?,
        }),
        _ => Err(invalid_journal()),
    }
}

fn resource(value: String) -> Result<PrivilegedResourceId, StoreError> {
    PrivilegedResourceId::new(value).map_err(|_| invalid_journal())
}

fn optional_id<T, E>(
    value: Option<String>,
    constructor: impl FnOnce(String) -> Result<T, E>,
) -> Result<Option<T>, StoreError> {
    value
        .map(|value| constructor(value).map_err(|_| invalid_journal()))
        .transpose()
}

const fn kind_number(kind: PrivilegedOperationKind) -> i64 {
    match kind {
        PrivilegedOperationKind::RunnerHealth => 0,
        PrivilegedOperationKind::CatalogApply => 1,
        PrivilegedOperationKind::IntegrationStart => 2,
        PrivilegedOperationKind::IntegrationStop => 3,
        PrivilegedOperationKind::ComputerObserve => 4,
        PrivilegedOperationKind::ComputerAct => 5,
    }
}

fn parse_kind(value: i64) -> Result<PrivilegedOperationKind, StoreError> {
    match value {
        0 => Ok(PrivilegedOperationKind::RunnerHealth),
        1 => Ok(PrivilegedOperationKind::CatalogApply),
        2 => Ok(PrivilegedOperationKind::IntegrationStart),
        3 => Ok(PrivilegedOperationKind::IntegrationStop),
        4 => Ok(PrivilegedOperationKind::ComputerObserve),
        5 => Ok(PrivilegedOperationKind::ComputerAct),
        _ => Err(invalid_journal()),
    }
}

const fn retry_number(retry: PrivilegedRetryClass) -> i64 {
    match retry {
        PrivilegedRetryClass::RetrySafe => 0,
        PrivilegedRetryClass::NonIdempotent => 1,
    }
}

fn parse_retry(value: i64) -> Result<PrivilegedRetryClass, StoreError> {
    match value {
        0 => Ok(PrivilegedRetryClass::RetrySafe),
        1 => Ok(PrivilegedRetryClass::NonIdempotent),
        _ => Err(invalid_journal()),
    }
}

const fn state_number(state: PrivilegedOperationState) -> i64 {
    match state {
        PrivilegedOperationState::Prepared => 0,
        PrivilegedOperationState::Dispatching => 1,
        PrivilegedOperationState::RetryPending => 2,
        PrivilegedOperationState::Succeeded => 3,
        PrivilegedOperationState::Failed => 4,
        PrivilegedOperationState::InterruptedNeedsReview => 5,
        PrivilegedOperationState::Reviewed => 6,
        PrivilegedOperationState::Cancelled => 7,
    }
}

fn parse_state(value: i64) -> Result<PrivilegedOperationState, StoreError> {
    match value {
        0 => Ok(PrivilegedOperationState::Prepared),
        1 => Ok(PrivilegedOperationState::Dispatching),
        2 => Ok(PrivilegedOperationState::RetryPending),
        3 => Ok(PrivilegedOperationState::Succeeded),
        4 => Ok(PrivilegedOperationState::Failed),
        5 => Ok(PrivilegedOperationState::InterruptedNeedsReview),
        6 => Ok(PrivilegedOperationState::Reviewed),
        7 => Ok(PrivilegedOperationState::Cancelled),
        _ => Err(invalid_journal()),
    }
}

fn parse_review(value: Option<i64>) -> Result<Option<PrivilegedOperationReview>, StoreError> {
    value
        .map(|value| match value {
            0 => Ok(PrivilegedOperationReview::ConfirmedSucceeded),
            1 => Ok(PrivilegedOperationReview::ConfirmedFailed),
            2 => Ok(PrivilegedOperationReview::Abandoned),
            _ => Err(invalid_journal()),
        })
        .transpose()
}

fn exact_digest(value: Vec<u8>) -> Result<[u8; 32], StoreError> {
    value.try_into().map_err(|_| invalid_journal())
}

fn exact_digest_slice(value: &[u8]) -> Result<[u8; 32], StoreError> {
    value.try_into().map_err(|_| invalid_journal())
}

fn exact_array<const N: usize>(value: Vec<u8>) -> Result<[u8; N], StoreError> {
    value.try_into().map_err(|_| invalid_journal())
}

fn exact_optional_digest(value: Option<&[u8]>) -> bool {
    value.is_some_and(|value| value.len() == 32)
}

fn unsigned(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| invalid_journal())
}

fn invalid_journal() -> StoreError {
    StoreError::Internal("invalid durable privileged-operation journal".into())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use grok_application::{PrivilegedOperationStore, SecureKeyProvider};
    use grok_domain::{PrivilegedOperationIntent, PrivilegedOperationLinks};
    use grok_memory::EphemeralKeyProvider;

    use super::*;

    fn prepared_operation(payload: &[u8]) -> PrivilegedOperation {
        PrivilegedOperation::prepare(
            PrivilegedOperationId::new("corrupt-operation-0001").expect("operation id"),
            PrivilegedOperationIntent::new(
                PrivilegedOperationKind::RunnerHealth,
                PrivilegedOperationTarget::Runner {
                    vm_id: PrivilegedResourceId::new("work-vm").expect("vm id"),
                },
                PayloadDigest::new(Sha256::digest(payload).into()),
                PrivilegedAuthority::new(
                    AuthorityGrantId::new("authority-grant-0001").expect("grant id"),
                    1_000,
                ),
                PrivilegedIdempotency::new(
                    PrivilegedIdempotencyKey::new("corrupt-command-key-0001")
                        .expect("idempotency key"),
                    RequestDigest::new([1; 32]),
                ),
                PrivilegedOperationLinks::default(),
            ),
            100,
        )
        .expect("prepared operation")
    }

    #[tokio::test]
    async fn corrupt_rows_fail_closed_during_validated_rehydration() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let provider: Arc<dyn SecureKeyProvider> = Arc::new(EphemeralKeyProvider::new([73; 32]));
        let store = SqlCipherStore::open(directory.path().join("corrupt.db"), provider)
            .await
            .expect("open store");
        let payload = b"{}".to_vec();
        let operation = prepared_operation(&payload);
        store
            .prepare_with_payload(operation.clone(), payload)
            .await
            .expect("prepare operation");
        let mut dispatching = operation.clone();
        dispatching.dispatch(110).expect("dispatch transition");
        store
            .begin_dispatch_with_attempt(
                dispatching,
                0,
                PrivilegedDispatchAttempt {
                    sequence: 1,
                    transport_operation_id: "corrupt-transport-0001".into(),
                    wire_digest: [2; 32],
                    broker_boot_id: [3; 16],
                    guest_boot_id: [4; 16],
                    started_at: 110,
                    deadline_unix_ms: 1_000,
                },
            )
            .await
            .expect("commit attempt");

        store
            .with_store(move |connection| {
                connection
                    .execute_batch(
                        "PRAGMA ignore_check_constraints=ON;
                         DROP TRIGGER privileged_operations_validate_update;
                         UPDATE privileged_operations SET revision=9
                         WHERE id='corrupt-operation-0001';",
                    )
                    .map_err(map_sqlite)?;
                Ok(())
            })
            .await
            .expect("seed impossible durable snapshot");

        assert!(matches!(
            store.get_privileged_operation(&operation.id).await,
            Err(StoreError::Internal(_))
        ));
        assert!(matches!(
            store.list_dispatching_for_recovery(10).await,
            Err(StoreError::Internal(_))
        ));
    }

    #[tokio::test]
    async fn recovery_rolls_back_operation_when_attempt_update_faults() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let provider: Arc<dyn SecureKeyProvider> = Arc::new(EphemeralKeyProvider::new([74; 32]));
        let store = SqlCipherStore::open(directory.path().join("recovery-fault.db"), provider)
            .await
            .expect("open store");
        let payload = b"{}".to_vec();
        let operation = prepared_operation(&payload);
        store
            .prepare_with_payload(operation.clone(), payload)
            .await
            .expect("prepare operation");
        let mut dispatching = operation.clone();
        dispatching.dispatch(110).expect("dispatch transition");
        store
            .begin_dispatch_with_attempt(
                dispatching.clone(),
                0,
                PrivilegedDispatchAttempt {
                    sequence: 1,
                    transport_operation_id: "recovery-fault-transport-0001".into(),
                    wire_digest: [2; 32],
                    broker_boot_id: [3; 16],
                    guest_boot_id: [4; 16],
                    started_at: 110,
                    deadline_unix_ms: 1_000,
                },
            )
            .await
            .expect("commit attempt");
        store
            .with_store(move |connection| {
                connection
                    .execute_batch(
                        "CREATE TRIGGER inject_recovery_attempt_failure
                         BEFORE UPDATE OF completed_at,outcome_certainty
                         ON privileged_operation_attempts
                         BEGIN
                           SELECT RAISE(ABORT, 'injected attempt update failure');
                         END;",
                    )
                    .map_err(map_sqlite)?;
                Ok(())
            })
            .await
            .expect("install recovery fault");

        let mut recovered = dispatching.clone();
        recovered.interrupt(120).expect("recovery transition");
        assert!(matches!(
            store
                .recover_interrupted_attempt(recovered, 1, 1, 120)
                .await,
            Err(StoreError::Conflict)
        ));

        assert_eq!(
            store
                .get_privileged_operation(&operation.id)
                .await
                .expect("operation update rolled back"),
            dispatching
        );
        let candidates = store
            .list_dispatching_for_recovery(10)
            .await
            .expect("attempt remains recoverable");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].operation.id, operation.id);
        let operation_id = operation.id.to_string();
        let attempt = store
            .with_store(move |connection| {
                connection
                    .query_row(
                        "SELECT completed_at,outcome_certainty
                         FROM privileged_operation_attempts
                         WHERE operation_id=?1 AND sequence=1",
                        [operation_id],
                        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, i64>(1)?)),
                    )
                    .map_err(map_sqlite)
            })
            .await
            .expect("load rolled-back attempt");
        assert_eq!(attempt, (None, 0));
    }

    #[tokio::test]
    async fn post_expiry_dispatch_corruption_fails_closed_during_recovery() {
        let directory = tempfile::tempdir().expect("temporary database directory");
        let provider: Arc<dyn SecureKeyProvider> = Arc::new(EphemeralKeyProvider::new([75; 32]));
        let store = SqlCipherStore::open(directory.path().join("expired-dispatch.db"), provider)
            .await
            .expect("open store");
        let payload = b"{}".to_vec();
        let operation = prepared_operation(&payload);
        store
            .prepare_with_payload(operation.clone(), payload)
            .await
            .expect("prepare operation");
        let mut dispatching = operation.clone();
        dispatching
            .dispatch(1_000)
            .expect("dispatch at inclusive authority boundary");
        store
            .begin_dispatch_with_attempt(
                dispatching,
                0,
                PrivilegedDispatchAttempt {
                    sequence: 1,
                    transport_operation_id: "expired-dispatch-transport-0001".into(),
                    wire_digest: [2; 32],
                    broker_boot_id: [3; 16],
                    guest_boot_id: [4; 16],
                    started_at: 1_000,
                    deadline_unix_ms: 2_000,
                },
            )
            .await
            .expect("commit boundary attempt");

        store
            .with_store(move |connection| {
                connection
                    .execute_batch(
                        "DROP TRIGGER privileged_operations_validate_update;
                         DROP TRIGGER privileged_operation_attempts_validate_update;
                         UPDATE privileged_operations SET updated_at=1001
                         WHERE id='corrupt-operation-0001';
                         UPDATE privileged_operation_attempts
                         SET started_at=1001,deadline_unix_ms=2001
                         WHERE operation_id='corrupt-operation-0001' AND sequence=1;",
                    )
                    .map_err(map_sqlite)?;
                Ok(())
            })
            .await
            .expect("seed internally correlated post-expiry dispatch evidence");

        assert!(matches!(
            store.get_privileged_operation(&operation.id).await,
            Err(StoreError::Internal(_))
        ));
        assert!(matches!(
            store.list_dispatching_for_recovery(10).await,
            Err(StoreError::Internal(_))
        ));
    }
}
