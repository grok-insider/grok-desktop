use async_trait::async_trait;
use grok_application::{
    ApplyManagedIntegrationLifecycle, MAX_MANAGED_INTEGRATION_RECOVERY_BATCH,
    ManagedIntegrationLifecycle, ManagedIntegrationLifecycleCommit,
    ManagedIntegrationLifecycleStore, ManagedIntegrationMutation, ManagedIntegrationPhase,
    ManagedIntegrationRecoveryEntry, StoreError,
};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{
    SqlCipherStore,
    store::{map_sqlite, number},
};

#[allow(clippy::too_many_lines)]
#[async_trait]
impl ManagedIntegrationLifecycleStore for SqlCipherStore {
    async fn get_managed_integration(
        &self,
        integration_id: &str,
    ) -> Result<Option<ManagedIntegrationLifecycle>, StoreError> {
        let integration_id = integration_id.to_owned();
        self.with_store(move |connection| load_lifecycle(connection, &integration_id))
            .await
    }

    async fn get_published_managed_integration(
        &self,
        integration_id: &str,
    ) -> Result<Option<ManagedIntegrationLifecycle>, StoreError> {
        let integration_id = integration_id.to_owned();
        self.with_store(move |connection| {
            connection
                .query_row(
                    "SELECT outcome_phase,outcome_installed_version,outcome_installed_digest,
                            candidate_version,candidate_manifest_digest,outcome_rollback_version,
                            outcome_rollback_digest,committed_revision,observed_at
                     FROM managed_integration_lifecycle_journal
                     WHERE integration_id=?1 AND published_at IS NOT NULL
                     ORDER BY committed_revision DESC LIMIT 1",
                    [&integration_id],
                    |row| {
                        Ok((
                            row.get::<_, i64>(0)?,
                            row.get::<_, Option<String>>(1)?,
                            row.get::<_, Option<Vec<u8>>>(2)?,
                            row.get::<_, String>(3)?,
                            row.get::<_, Vec<u8>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<Vec<u8>>>(6)?,
                            row.get::<_, i64>(7)?,
                            row.get::<_, i64>(8)?,
                        ))
                    },
                )
                .optional()
                .map_err(map_sqlite)?
                .map(|row| decode_lifecycle(integration_id, row))
                .transpose()
        })
        .await
    }

    async fn apply_managed_integration_lifecycle(
        &self,
        command: ApplyManagedIntegrationLifecycle,
    ) -> Result<ManagedIntegrationLifecycleCommit, StoreError> {
        validate_command(&command)?;
        self.with_store(move |connection| {
            let transaction = connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(map_sqlite)?;
            if let Some((fingerprint, lifecycle)) =
                load_journal(&transaction, &command.idempotency_key)?
            {
                if fingerprint != command.request_fingerprint {
                    return Err(StoreError::Conflict);
                }
                return Ok(ManagedIntegrationLifecycleCommit {
                    lifecycle,
                    replayed: true,
                });
            }
            let pending: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM managed_integration_lifecycle_journal
                         WHERE integration_id=?1 AND published_at IS NULL
                     )",
                    [&command.integration_id],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if pending {
                return Err(StoreError::Conflict);
            }
            let current = load_lifecycle(&transaction, &command.integration_id)?;
            let revision = current.as_ref().map_or(0, |value| value.revision);
            if revision != command.expected_revision {
                return Err(StoreError::Conflict);
            }
            let next_revision = revision.checked_add(1).ok_or(StoreError::Conflict)?;
            let lifecycle = transition(&command, current.as_ref(), next_revision)?;
            let changed = transaction
                .execute(
                    "INSERT INTO managed_integration_lifecycles(
                         integration_id,phase,installed_version,installed_manifest_digest,
                         available_version,available_manifest_digest,rollback_version,
                         rollback_manifest_digest,revision,updated_at
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
                     ON CONFLICT(integration_id) DO UPDATE SET
                         phase=excluded.phase,
                         installed_version=excluded.installed_version,
                         installed_manifest_digest=excluded.installed_manifest_digest,
                         available_version=excluded.available_version,
                         available_manifest_digest=excluded.available_manifest_digest,
                         rollback_version=excluded.rollback_version,
                         rollback_manifest_digest=excluded.rollback_manifest_digest,
                         revision=excluded.revision,
                         updated_at=excluded.updated_at
                     WHERE managed_integration_lifecycles.revision=?11",
                    params![
                        lifecycle.integration_id,
                        phase_number(lifecycle.phase),
                        lifecycle.installed_version,
                        lifecycle
                            .installed_manifest_digest
                            .as_ref()
                            .map(<[u8; 32]>::as_slice),
                        lifecycle.available_version,
                        lifecycle.available_manifest_digest.as_slice(),
                        lifecycle.rollback_version,
                        lifecycle
                            .rollback_manifest_digest
                            .as_ref()
                            .map(<[u8; 32]>::as_slice),
                        number(lifecycle.revision)?,
                        number(lifecycle.updated_at)?,
                        number(command.expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            if changed != 1 {
                return Err(StoreError::Conflict);
            }
            transaction
                .execute(
                    "INSERT INTO managed_integration_lifecycle_journal(
                         idempotency_key,request_fingerprint,integration_id,mutation,
                         committed_revision,candidate_version,candidate_manifest_digest,
                         outcome_phase,outcome_installed_version,outcome_installed_digest,
                         outcome_rollback_version,outcome_rollback_digest,observed_at,published_at
                     ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,NULL)",
                    params![
                        command.idempotency_key,
                        command.request_fingerprint.as_slice(),
                        command.integration_id,
                        mutation_number(command.mutation),
                        number(lifecycle.revision)?,
                        command.candidate_version,
                        command.candidate_manifest_digest.as_slice(),
                        phase_number(lifecycle.phase),
                        lifecycle.installed_version,
                        lifecycle
                            .installed_manifest_digest
                            .as_ref()
                            .map(<[u8; 32]>::as_slice),
                        lifecycle.rollback_version,
                        lifecycle
                            .rollback_manifest_digest
                            .as_ref()
                            .map(<[u8; 32]>::as_slice),
                        number(command.observed_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(ManagedIntegrationLifecycleCommit {
                lifecycle,
                replayed: false,
            })
        })
        .await
    }

    async fn pending_managed_integration_publications(
        &self,
        limit: usize,
    ) -> Result<Vec<ManagedIntegrationRecoveryEntry>, StoreError> {
        if limit == 0 || limit > MAX_MANAGED_INTEGRATION_RECOVERY_BATCH + 1 {
            return Err(StoreError::Conflict);
        }
        self.with_store(move |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT idempotency_key,integration_id,mutation,committed_revision,
                            candidate_manifest_digest
                     FROM managed_integration_lifecycle_journal
                     WHERE published_at IS NULL
                     ORDER BY idempotency_key LIMIT ?1",
                )
                .map_err(map_sqlite)?;
            let rows = statement
                .query_map(
                    [i64::try_from(limit).map_err(|_| StoreError::Conflict)?],
                    |row| {
                        let mutation: i64 = row.get(2)?;
                        let revision: i64 = row.get(3)?;
                        let digest: Vec<u8> = row.get(4)?;
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            mutation,
                            revision,
                            digest,
                        ))
                    },
                )
                .map_err(map_sqlite)?;
            rows.map(|row| {
                let (idempotency_key, integration_id, mutation, revision, digest) =
                    row.map_err(map_sqlite)?;
                Ok(ManagedIntegrationRecoveryEntry {
                    idempotency_key,
                    integration_id,
                    mutation: parse_mutation(mutation)?,
                    committed_revision: positive(revision)?,
                    candidate_manifest_digest: digest32(digest)?,
                })
            })
            .collect()
        })
        .await
    }

    async fn acknowledge_managed_integration_publication(
        &self,
        idempotency_key: &str,
        committed_revision: u64,
        published_at: u64,
    ) -> Result<(), StoreError> {
        let idempotency_key = idempotency_key.to_owned();
        self.with_store(move |connection| {
            let existing: Option<(i64, Option<i64>, i64)> = connection
                .query_row(
                    "SELECT committed_revision,published_at,observed_at
                     FROM managed_integration_lifecycle_journal WHERE idempotency_key=?1",
                    [&idempotency_key],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(map_sqlite)?;
            let (revision, prior, observed_at) = existing.ok_or(StoreError::NotFound)?;
            if positive(revision)? != committed_revision || published_at < positive(observed_at)? {
                return Err(StoreError::Conflict);
            }
            if let Some(prior) = prior {
                return if positive(prior)? == published_at {
                    Ok(())
                } else {
                    Err(StoreError::Conflict)
                };
            }
            let changed = connection
                .execute(
                    "UPDATE managed_integration_lifecycle_journal SET published_at=?1
                     WHERE idempotency_key=?2 AND committed_revision=?3 AND published_at IS NULL",
                    params![
                        number(published_at)?,
                        idempotency_key,
                        number(committed_revision)?
                    ],
                )
                .map_err(map_sqlite)?;
            if changed == 1 {
                Ok(())
            } else {
                Err(StoreError::Conflict)
            }
        })
        .await
    }
}

fn load_lifecycle(
    connection: &rusqlite::Connection,
    integration_id: &str,
) -> Result<Option<ManagedIntegrationLifecycle>, StoreError> {
    connection
        .query_row(
            "SELECT phase,installed_version,installed_manifest_digest,available_version,
                    available_manifest_digest,rollback_version,rollback_manifest_digest,
                    revision,updated_at
             FROM managed_integration_lifecycles WHERE integration_id=?1",
            [integration_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<Vec<u8>>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?
        .map(|row| decode_lifecycle(integration_id.to_owned(), row))
        .transpose()
}

fn load_journal(
    transaction: &Transaction<'_>,
    key: &str,
) -> Result<Option<(Vec<u8>, ManagedIntegrationLifecycle)>, StoreError> {
    transaction
        .query_row(
            "SELECT request_fingerprint,integration_id,outcome_phase,outcome_installed_version,
                    outcome_installed_digest,candidate_version,candidate_manifest_digest,
                    outcome_rollback_version,outcome_rollback_digest,committed_revision,observed_at
             FROM managed_integration_lifecycle_journal WHERE idempotency_key=?1",
            [key],
            |row| {
                Ok((
                    row.get::<_, Vec<u8>>(0)?,
                    row.get::<_, String>(1)?,
                    (
                        row.get::<_, i64>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<Vec<u8>>>(4)?,
                        row.get::<_, String>(5)?,
                        row.get::<_, Vec<u8>>(6)?,
                        row.get::<_, Option<String>>(7)?,
                        row.get::<_, Option<Vec<u8>>>(8)?,
                        row.get::<_, i64>(9)?,
                        row.get::<_, i64>(10)?,
                    ),
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?
        .map(|(fingerprint, id, row)| Ok((fingerprint, decode_lifecycle(id, row)?)))
        .transpose()
}

type LifecycleRow = (
    i64,
    Option<String>,
    Option<Vec<u8>>,
    String,
    Vec<u8>,
    Option<String>,
    Option<Vec<u8>>,
    i64,
    i64,
);

fn decode_lifecycle(
    id: String,
    row: LifecycleRow,
) -> Result<ManagedIntegrationLifecycle, StoreError> {
    Ok(ManagedIntegrationLifecycle {
        integration_id: id,
        phase: parse_phase(row.0)?,
        installed_version: row.1,
        installed_manifest_digest: row.2.map(digest32).transpose()?,
        available_version: row.3,
        available_manifest_digest: digest32(row.4)?,
        rollback_version: row.5,
        rollback_manifest_digest: row.6.map(digest32).transpose()?,
        revision: positive(row.7)?,
        updated_at: nonnegative(row.8)?,
    })
}

fn transition(
    command: &ApplyManagedIntegrationLifecycle,
    current: Option<&ManagedIntegrationLifecycle>,
    revision: u64,
) -> Result<ManagedIntegrationLifecycle, StoreError> {
    let (phase, installed_version, installed_digest, rollback_version, rollback_digest) =
        match command.mutation {
            ManagedIntegrationMutation::Install if current.is_none() => (
                ManagedIntegrationPhase::Installed,
                Some(command.candidate_version.clone()),
                Some(command.candidate_manifest_digest),
                None,
                None,
            ),
            ManagedIntegrationMutation::Update => {
                let current = current.ok_or(StoreError::Conflict)?;
                let version = current
                    .installed_version
                    .clone()
                    .ok_or(StoreError::Conflict)?;
                let digest = current
                    .installed_manifest_digest
                    .ok_or(StoreError::Conflict)?;
                if version == command.candidate_version {
                    return Err(StoreError::Conflict);
                }
                (
                    ManagedIntegrationPhase::RollbackAvailable,
                    Some(command.candidate_version.clone()),
                    Some(command.candidate_manifest_digest),
                    Some(version),
                    Some(digest),
                )
            }
            ManagedIntegrationMutation::Rollback => {
                let current = current.ok_or(StoreError::Conflict)?;
                let version = current
                    .rollback_version
                    .clone()
                    .ok_or(StoreError::Conflict)?;
                let digest = current
                    .rollback_manifest_digest
                    .ok_or(StoreError::Conflict)?;
                (
                    ManagedIntegrationPhase::UpdateAvailable,
                    Some(version),
                    Some(digest),
                    None,
                    None,
                )
            }
            ManagedIntegrationMutation::Install => return Err(StoreError::Conflict),
        };
    Ok(ManagedIntegrationLifecycle {
        integration_id: command.integration_id.clone(),
        phase,
        installed_version,
        installed_manifest_digest: installed_digest,
        available_version: command.candidate_version.clone(),
        available_manifest_digest: command.candidate_manifest_digest,
        rollback_version,
        rollback_manifest_digest: rollback_digest,
        revision,
        updated_at: command.observed_at,
    })
}

fn validate_command(command: &ApplyManagedIntegrationLifecycle) -> Result<(), StoreError> {
    if command.idempotency_key.is_empty()
        || command.idempotency_key.len() > 128
        || command.integration_id.is_empty()
        || command.integration_id.len() > 128
        || command.candidate_version.is_empty()
        || command.candidate_version.len() > 64
    {
        Err(StoreError::Conflict)
    } else {
        Ok(())
    }
}
fn phase_number(value: ManagedIntegrationPhase) -> i64 {
    match value {
        ManagedIntegrationPhase::Available => 0,
        ManagedIntegrationPhase::Installed => 1,
        ManagedIntegrationPhase::UpdateAvailable => 2,
        ManagedIntegrationPhase::RollbackAvailable => 3,
    }
}
fn mutation_number(value: ManagedIntegrationMutation) -> i64 {
    match value {
        ManagedIntegrationMutation::Install => 0,
        ManagedIntegrationMutation::Update => 1,
        ManagedIntegrationMutation::Rollback => 2,
    }
}
fn parse_phase(value: i64) -> Result<ManagedIntegrationPhase, StoreError> {
    match value {
        0 => Ok(ManagedIntegrationPhase::Available),
        1 => Ok(ManagedIntegrationPhase::Installed),
        2 => Ok(ManagedIntegrationPhase::UpdateAvailable),
        3 => Ok(ManagedIntegrationPhase::RollbackAvailable),
        _ => Err(StoreError::Internal(
            "invalid managed integration phase".into(),
        )),
    }
}
fn parse_mutation(value: i64) -> Result<ManagedIntegrationMutation, StoreError> {
    match value {
        0 => Ok(ManagedIntegrationMutation::Install),
        1 => Ok(ManagedIntegrationMutation::Update),
        2 => Ok(ManagedIntegrationMutation::Rollback),
        _ => Err(StoreError::Internal(
            "invalid managed integration mutation".into(),
        )),
    }
}
fn positive(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value)
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| StoreError::Internal("invalid positive managed integration number".into()))
}
fn nonnegative(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value)
        .map_err(|_| StoreError::Internal("invalid managed integration timestamp".into()))
}
fn digest32(value: Vec<u8>) -> Result<[u8; 32], StoreError> {
    value
        .try_into()
        .map_err(|_| StoreError::Internal("invalid managed integration digest".into()))
}
