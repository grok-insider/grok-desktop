use async_trait::async_trait;
use grok_application::{
    ArtifactContentReadyResult, ArtifactImportFailureCode, ArtifactImportPlan,
    ArtifactImportReservation, ArtifactImportState, ArtifactOpenFailureCode, ArtifactOpenPlan,
    ArtifactOpenReservation, ArtifactOpenState, ArtifactQuotaUsage, ArtifactRemovalPlan,
    ArtifactRemovalReservation, ArtifactRemovalState, ArtifactRetentionRecord,
    ArtifactRetentionState, ArtifactStore, MAX_ARTIFACT_FILE_BYTES, MAX_GLOBAL_ARTIFACT_BYTES,
    MAX_PROJECT_ARTIFACT_BYTES, MAX_PROJECT_ARTIFACT_COUNT, MutationCommand, StoreError,
};
use grok_domain::{Artifact, ArtifactId, ArtifactVersion, ProjectId, ThreadId};
use rusqlite::{
    Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params, types::Type,
};

use crate::{SqlCipherStore, mapping};

const ARTIFACT_COLUMNS: &str = "id,project_id,thread_id,name,current_content_version,\
    (SELECT media_type FROM artifact_versions \
        WHERE artifact_id=artifacts.id AND version=artifacts.current_content_version),\
    (SELECT byte_size FROM artifact_versions \
        WHERE artifact_id=artifacts.id AND version=artifacts.current_content_version),\
    state,revision,created_at,updated_at";
const ARTIFACT_VERSION_COLUMNS: &str =
    "artifact_id,version,content_sha256,media_type,byte_size,created_at";
const IMPORT_PLAN_COLUMNS: &str = "artifacts.id,artifacts.project_id,artifacts.thread_id,\
    artifacts.name,artifacts.current_content_version,\
    (SELECT media_type FROM artifact_versions \
        WHERE artifact_id=artifacts.id AND version=artifacts.current_content_version),\
    (SELECT byte_size FROM artifact_versions \
        WHERE artifact_id=artifacts.id AND version=artifacts.current_content_version),\
    artifacts.state,artifacts.revision,artifacts.created_at,artifacts.updated_at,\
    artifact_ingestions.state,artifact_ingestions.content_sha256,\
    artifact_ingestions.content_media_type,artifact_ingestions.content_byte_size,\
    artifact_ingestions.content_created_at,artifact_ingestions.failure_code,\
    artifact_ingestions.revision,artifact_ingestions.created_at,artifact_ingestions.updated_at";
const OPEN_PLAN_COLUMNS: &str = "artifact_versions.artifact_id,artifact_versions.version,\
    artifact_versions.content_sha256,artifact_versions.media_type,artifact_versions.byte_size,\
    artifact_versions.created_at,artifact_open_commands.state,\
    artifact_open_commands.failure_code,artifact_open_commands.revision,\
    artifact_open_commands.created_at,artifact_open_commands.updated_at";
const REMOVAL_PLAN_COLUMNS: &str = "artifacts.id,artifacts.project_id,artifacts.thread_id,\
    artifacts.name,artifacts.current_content_version,\
    (SELECT media_type FROM artifact_versions \
        WHERE artifact_id=artifacts.id AND version=artifacts.current_content_version),\
    (SELECT byte_size FROM artifact_versions \
        WHERE artifact_id=artifacts.id AND version=artifacts.current_content_version),\
    artifacts.state,artifacts.revision,artifacts.created_at,artifacts.updated_at,\
    artifact_removal_commands.state,artifact_removal_commands.revision,\
    artifact_removal_commands.created_at,artifact_removal_commands.updated_at";
const RETENTION_COLUMNS: &str = "artifact_versions.artifact_id,artifact_versions.version,\
    artifact_versions.content_sha256,artifact_versions.media_type,artifact_versions.byte_size,\
    artifact_versions.created_at,artifact_version_retention.state,\
    artifact_version_retention.revision,artifact_version_retention.created_at,\
    artifact_version_retention.updated_at,artifact_version_retention.purged_at";

#[async_trait]
impl ArtifactStore for SqlCipherStore {
    async fn resolve_import(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ArtifactImportPlan>, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            ensure_command_scope(&command, "import_artifact")?;
            let resolved = connection
                .query_row(
                    "SELECT request_fingerprint,artifact_id
                     FROM artifact_ingestions
                     WHERE command_scope=?1 AND idempotency_key=?2",
                    params![command.scope, command.key],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?;
            let Some((fingerprint, artifact_id)) = resolved else {
                return Ok(None);
            };
            if fingerprint.as_slice() != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            query_import_plan(connection, &artifact_id).map(Some)
        })
        .await
    }

    async fn reserve_import(
        &self,
        artifact: Artifact,
        command: &MutationCommand,
    ) -> Result<ArtifactImportReservation, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            ensure_command_scope(&command, "import_artifact")?;
            let prepared = ArtifactImportPlan::prepared(artifact.clone())
                .map_err(|_| integrity("invalid artifact import reservation"))?;
            let transaction = begin(connection)?;
            if let Some((fingerprint, artifact_id)) = transaction
                .query_row(
                    "SELECT request_fingerprint,artifact_id
                     FROM artifact_ingestions
                     WHERE command_scope=?1 AND idempotency_key=?2",
                    params![command.scope, command.key],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?
            {
                if fingerprint.as_slice() != command.fingerprint {
                    return Err(StoreError::Conflict);
                }
                let plan = query_import_plan(&transaction, &artifact_id)?;
                transaction.commit().map_err(map_sqlite)?;
                return Ok(ArtifactImportReservation::ExactReplay(plan));
            }

            ensure_active_artifact_owner(&transaction, &artifact)?;
            let count = project_artifact_count(&transaction, &artifact.project_id)?;
            if count >= MAX_PROJECT_ARTIFACT_COUNT {
                return Err(StoreError::Conflict);
            }
            transaction
                .execute(
                    "INSERT INTO artifacts(
                         id,project_id,thread_id,name,current_content_version,state,revision,
                         created_at,updated_at
                     ) VALUES (?1,?2,?3,?4,NULL,0,0,?5,?5)",
                    params![
                        artifact.id.as_str(),
                        artifact.project_id.as_str(),
                        artifact.thread_id.as_ref().map(ThreadId::as_str),
                        artifact.name,
                        number(artifact.created_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            transaction
                .execute(
                    "INSERT INTO artifact_ingestions(
                         command_scope,idempotency_key,request_fingerprint,artifact_id,
                         state,revision,content_sha256,content_media_type,content_byte_size,
                         content_created_at,active_slot,failure_code,created_at,updated_at
                     ) VALUES (?1,?2,?3,?4,0,0,NULL,NULL,NULL,NULL,1,NULL,?5,?5)",
                    params![
                        command.scope,
                        command.key,
                        command.fingerprint.as_slice(),
                        artifact.id.as_str(),
                        number(artifact.created_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            let plan = query_import_plan(&transaction, artifact.id.as_str())?;
            if plan != prepared {
                return Err(integrity(
                    "artifact import reservation changed during storage",
                ));
            }
            transaction.commit().map_err(map_sqlite)?;
            Ok(ArtifactImportReservation::NewlyPrepared(plan))
        })
        .await
    }

    async fn mark_content_ready(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        content: ArtifactVersion,
        now: u64,
    ) -> Result<ArtifactContentReadyResult, StoreError> {
        let artifact_id = artifact_id.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            let mut plan = query_import_plan(&transaction, artifact_id.as_str())?;
            if plan.revision != expected_revision
                || plan.state != ArtifactImportState::Prepared
                || content.artifact_id != artifact_id
                || content.version != 1
            {
                return Err(StoreError::Conflict);
            }
            ArtifactVersion::restore(content.clone())
                .map_err(|_| integrity("invalid artifact content metadata"))?;

            let failure =
                quota_failure_for_content(&transaction, &plan.artifact.project_id, &content)?;
            if let Some(failure) = failure {
                return Ok(ArtifactContentReadyResult::QuotaExceeded { plan, failure });
            }
            plan.record_content_ready(content.clone(), now)
                .map_err(|_| StoreError::Conflict)?;
            let changed = transaction
                .execute(
                    "UPDATE artifact_ingestions
                     SET state=1,revision=?1,content_sha256=?2,content_media_type=?3,
                         content_byte_size=?4,content_created_at=?5,updated_at=?6
                     WHERE artifact_id=?7 AND state=0 AND revision=?8",
                    params![
                        number(plan.revision)?,
                        content.sha256.as_slice(),
                        content.media_type,
                        number(content.byte_size)?,
                        number(content.created_at)?,
                        number(plan.updated_at)?,
                        artifact_id.as_str(),
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            let stored = query_import_plan(&transaction, artifact_id.as_str())?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(ArtifactContentReadyResult::ContentReady(stored))
        })
        .await
    }

    async fn commit_import(
        &self,
        artifact: Artifact,
        expected_artifact_revision: u64,
        expected_import_revision: u64,
        content: ArtifactVersion,
        now: u64,
    ) -> Result<ArtifactImportPlan, StoreError> {
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            let mut plan = query_import_plan(&transaction, artifact.id.as_str())?;
            if plan.state != ArtifactImportState::ContentReady
                || plan.revision != expected_import_revision
                || plan.artifact.revision != expected_artifact_revision
                || plan.content.as_ref() != Some(&content)
                || artifact.updated_at != now
            {
                return Err(StoreError::Conflict);
            }
            let reserved_project_id = plan.artifact.project_id.clone();
            plan.commit(artifact.clone(), now)
                .map_err(|_| StoreError::Conflict)?;
            ensure_quota_still_reserved(&transaction, &reserved_project_id)?;

            transaction
                .execute(
                    "INSERT INTO artifact_versions(
                         artifact_id,version,content_sha256,media_type,byte_size,created_at
                     ) VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        content.artifact_id.as_str(),
                        i64::from(content.version),
                        content.sha256.as_slice(),
                        content.media_type,
                        number(content.byte_size)?,
                        number(content.created_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            let changed = transaction
                .execute(
                    "UPDATE artifacts
                     SET current_content_version=?1,state=1,revision=?2,updated_at=?3
                     WHERE id=?4 AND state=0 AND revision=?5",
                    params![
                        i64::from(content.version),
                        number(artifact.revision)?,
                        number(artifact.updated_at)?,
                        artifact.id.as_str(),
                        number(expected_artifact_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            let changed = transaction
                .execute(
                    "UPDATE artifact_ingestions
                     SET state=2,revision=?1,active_slot=NULL,updated_at=?2
                     WHERE artifact_id=?3 AND state=1 AND revision=?4",
                    params![
                        number(plan.revision)?,
                        number(plan.updated_at)?,
                        artifact.id.as_str(),
                        number(expected_import_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            let stored = query_import_plan(&transaction, artifact.id.as_str())?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(stored)
        })
        .await
    }

    async fn fail_import(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        failure: ArtifactImportFailureCode,
        now: u64,
    ) -> Result<ArtifactImportPlan, StoreError> {
        let artifact_id = artifact_id.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            let mut plan = query_import_plan(&transaction, artifact_id.as_str())?;
            if plan.revision != expected_revision {
                return Err(StoreError::Conflict);
            }
            plan.fail(failure, now).map_err(|_| StoreError::Conflict)?;
            update_failed_import(&transaction, &plan, expected_revision)?;
            let stored = query_import_plan(&transaction, artifact_id.as_str())?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(stored)
        })
        .await
    }

    async fn list_incomplete_imports(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactImportPlan>, StoreError> {
        self.with_store(move |connection| {
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {IMPORT_PLAN_COLUMNS}
                     FROM artifact_ingestions
                     JOIN artifacts ON artifacts.id=artifact_ingestions.artifact_id
                     WHERE artifact_ingestions.state IN (0,1)
                     ORDER BY artifact_ingestions.created_at,artifacts.id LIMIT ?1"
                ))
                .map_err(map_sqlite)?;
            collect_rows(statement.query_map([sql_limit(limit)], import_plan_from_row))
        })
        .await
    }

    async fn get_artifact(&self, id: &ArtifactId) -> Result<Artifact, StoreError> {
        let id = id.to_string();
        self.with_store(move |connection| query_artifact(connection, &id))
            .await
    }

    async fn list_artifacts(
        &self,
        project_id: &ProjectId,
        after: Option<&ArtifactId>,
        limit: usize,
    ) -> Result<Vec<Artifact>, StoreError> {
        let project_id = project_id.clone();
        let after = after.cloned();
        self.with_store(move |connection| {
            ensure_project_exists(connection, &project_id)?;
            let cursor = after
                .as_ref()
                .map(|id| artifact_cursor(connection, &project_id, id))
                .transpose()?;
            let (updated_at, id) = cursor.map_or((None, None), |(updated_at, id)| {
                (Some(updated_at), Some(id))
            });
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {ARTIFACT_COLUMNS} FROM artifacts
                     WHERE project_id=?1
                       AND (?2 IS NULL OR updated_at<?2 OR (updated_at=?2 AND id>?3))
                     ORDER BY updated_at DESC,id LIMIT ?4"
                ))
                .map_err(map_sqlite)?;
            collect_rows(statement.query_map(
                params![project_id.as_str(), updated_at, id, sql_limit(limit)],
                mapping::artifact_from_row,
            ))
        })
        .await
    }

    async fn get_artifact_version(
        &self,
        artifact_id: &ArtifactId,
        version: u32,
    ) -> Result<ArtifactVersion, StoreError> {
        let artifact_id = artifact_id.clone();
        self.with_store(move |connection| query_artifact_version(connection, &artifact_id, version))
            .await
    }

    async fn quota_usage(&self, project_id: &ProjectId) -> Result<ArtifactQuotaUsage, StoreError> {
        let project_id = project_id.clone();
        self.with_store(move |connection| quota_usage(connection, &project_id))
            .await
    }

    async fn resolve_open(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ArtifactOpenPlan>, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            ensure_command_scope(&command, "open_artifact")?;
            let fingerprint = connection
                .query_row(
                    "SELECT request_fingerprint FROM artifact_open_commands
                     WHERE command_scope=?1 AND idempotency_key=?2",
                    params![command.scope, command.key],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .map_err(map_sqlite)?;
            let Some(fingerprint) = fingerprint else {
                return Ok(None);
            };
            if fingerprint.as_slice() != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            query_open_plan_by_command(connection, &command.key).map(Some)
        })
        .await
    }

    async fn prepare_open(
        &self,
        content: ArtifactVersion,
        command: &MutationCommand,
        now: u64,
    ) -> Result<ArtifactOpenReservation, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            ensure_command_scope(&command, "open_artifact")?;
            let transaction = begin(connection)?;
            if let Some(fingerprint) = transaction
                .query_row(
                    "SELECT request_fingerprint FROM artifact_open_commands
                     WHERE command_scope=?1 AND idempotency_key=?2",
                    params![command.scope, command.key],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .map_err(map_sqlite)?
            {
                if fingerprint.as_slice() != command.fingerprint {
                    return Err(StoreError::Conflict);
                }
                let plan = query_open_plan_by_command(&transaction, &command.key)?;
                transaction.commit().map_err(map_sqlite)?;
                return Ok(ArtifactOpenReservation::ExactReplay(plan));
            }

            let canonical = query_artifact_version(
                &transaction,
                &content.artifact_id,
                content.version,
            )?;
            if canonical != content || !is_current_available_content(&transaction, &content)? {
                return Err(StoreError::Conflict);
            }
            let prepared = ArtifactOpenPlan::prepared(content.clone(), now)
                .map_err(|_| integrity("invalid artifact open reservation"))?;
            transaction
                .execute(
                    "INSERT INTO artifact_open_commands(
                         command_scope,idempotency_key,request_fingerprint,artifact_id,
                         content_version,state,revision,active_slot,failure_code,created_at,updated_at
                     ) VALUES (?1,?2,?3,?4,?5,0,0,1,NULL,?6,?6)",
                    params![
                        command.scope,
                        command.key,
                        command.fingerprint.as_slice(),
                        content.artifact_id.as_str(),
                        i64::from(content.version),
                        number(now)?,
                    ],
                )
                .map_err(map_sqlite)?;
            let plan = query_open_plan_by_command(&transaction, &command.key)?;
            if plan != prepared {
                return Err(integrity("artifact open reservation changed during storage"));
            }
            transaction.commit().map_err(map_sqlite)?;
            Ok(ArtifactOpenReservation::NewlyPrepared(plan))
        })
        .await
    }

    async fn mark_open_dispatching(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: u64,
    ) -> Result<ArtifactOpenPlan, StoreError> {
        transition_open(
            self,
            artifact_id.clone(),
            content_version,
            expected_revision,
            now,
            OpenTransition::Dispatch,
        )
        .await
    }

    async fn complete_open(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: u64,
    ) -> Result<ArtifactOpenPlan, StoreError> {
        transition_open(
            self,
            artifact_id.clone(),
            content_version,
            expected_revision,
            now,
            OpenTransition::Complete,
        )
        .await
    }

    async fn fail_open(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        failure: ArtifactOpenFailureCode,
        now: u64,
    ) -> Result<ArtifactOpenPlan, StoreError> {
        transition_open(
            self,
            artifact_id.clone(),
            content_version,
            expected_revision,
            now,
            OpenTransition::Fail(failure),
        )
        .await
    }

    async fn interrupt_open(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: u64,
    ) -> Result<ArtifactOpenPlan, StoreError> {
        transition_open(
            self,
            artifact_id.clone(),
            content_version,
            expected_revision,
            now,
            OpenTransition::Interrupt,
        )
        .await
    }

    async fn list_incomplete_opens(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactOpenPlan>, StoreError> {
        self.with_store(move |connection| {
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {OPEN_PLAN_COLUMNS}
                     FROM artifact_open_commands
                     JOIN artifact_versions
                       ON artifact_versions.artifact_id=artifact_open_commands.artifact_id
                      AND artifact_versions.version=artifact_open_commands.content_version
                     WHERE artifact_open_commands.state IN (0,1)
                     ORDER BY artifact_open_commands.created_at,
                              artifact_open_commands.artifact_id LIMIT ?1"
                ))
                .map_err(map_sqlite)?;
            collect_rows(statement.query_map([sql_limit(limit)], open_plan_from_row))
        })
        .await
    }

    async fn resolve_removal(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ArtifactRemovalPlan>, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            ensure_command_scope(&command, "remove_artifact")?;
            let resolved = connection
                .query_row(
                    "SELECT request_fingerprint,artifact_id
                     FROM artifact_removal_commands
                     WHERE command_scope=?1 AND idempotency_key=?2",
                    params![command.scope, command.key],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?;
            let Some((fingerprint, artifact_id)) = resolved else {
                return Ok(None);
            };
            if fingerprint.as_slice() != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            query_removal_plan(connection, &artifact_id).map(Some)
        })
        .await
    }

    async fn reserve_removal(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        expected_content_version: u32,
        command: &MutationCommand,
        now: u64,
    ) -> Result<ArtifactRemovalReservation, StoreError> {
        let artifact_id = artifact_id.clone();
        let command = command.clone();
        self.with_store(move |connection| {
            ensure_command_scope(&command, "remove_artifact")?;
            let transaction = begin(connection)?;
            if let Some((fingerprint, resolved_artifact_id)) = transaction
                .query_row(
                    "SELECT request_fingerprint,artifact_id
                     FROM artifact_removal_commands
                     WHERE command_scope=?1 AND idempotency_key=?2",
                    params![command.scope, command.key],
                    |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?
            {
                if fingerprint.as_slice() != command.fingerprint {
                    return Err(StoreError::Conflict);
                }
                let plan = query_removal_plan(&transaction, &resolved_artifact_id)?;
                transaction.commit().map_err(map_sqlite)?;
                return Ok(ArtifactRemovalReservation::ExactReplay(plan));
            }

            transaction
                .execute(
                    "INSERT INTO artifact_removal_commands(
                         command_scope,idempotency_key,request_fingerprint,artifact_id,
                         content_version,state,revision,active_slot,created_at,updated_at
                     ) VALUES (?1,?2,?3,?4,?5,0,0,1,?6,?6)",
                    params![
                        command.scope,
                        command.key,
                        command.fingerprint.as_slice(),
                        artifact_id.as_str(),
                        i64::from(expected_content_version),
                        number(now)?,
                    ],
                )
                .map_err(map_sqlite)?;
            let changed = transaction
                .execute(
                    "UPDATE artifacts
                     SET current_content_version=NULL,state=2,revision=revision+1,updated_at=?1
                     WHERE id=?2 AND state=1 AND revision=?3
                       AND current_content_version=?4",
                    params![
                        number(now)?,
                        artifact_id.as_str(),
                        number(expected_revision)?,
                        i64::from(expected_content_version),
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            let pending_versions = transaction
                .execute(
                    "UPDATE artifact_version_retention
                     SET state=1,revision=1,updated_at=?1
                     WHERE artifact_id=?2 AND state=0 AND revision=0",
                    params![number(now)?, artifact_id.as_str()],
                )
                .map_err(map_sqlite)?;
            if pending_versions == 0 {
                return Err(integrity("artifact removal reserved no retained content"));
            }
            let unreserved: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM artifact_version_retention
                         WHERE artifact_id=?1 AND state=0
                     )",
                    [artifact_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if unreserved {
                return Err(integrity(
                    "artifact removal left retained content unreserved",
                ));
            }
            let plan = query_removal_plan(&transaction, artifact_id.as_str())?;
            let prepared = ArtifactRemovalPlan::pending(plan.artifact.clone())
                .map_err(|_| integrity("invalid artifact removal reservation"))?;
            if plan != prepared {
                return Err(integrity(
                    "artifact removal reservation changed during storage",
                ));
            }
            transaction.commit().map_err(map_sqlite)?;
            Ok(ArtifactRemovalReservation::NewlyPending(plan))
        })
        .await
    }

    async fn list_pending_removal_versions(
        &self,
        artifact_id: &ArtifactId,
        limit: usize,
    ) -> Result<Vec<ArtifactRetentionRecord>, StoreError> {
        let artifact_id = artifact_id.clone();
        self.with_store(move |connection| {
            let plan = query_removal_plan(connection, artifact_id.as_str())?;
            if plan.state != ArtifactRemovalState::Pending {
                return Err(StoreError::Conflict);
            }
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {RETENTION_COLUMNS}
                     FROM artifact_version_retention
                     JOIN artifact_versions
                       ON artifact_versions.artifact_id=artifact_version_retention.artifact_id
                      AND artifact_versions.version=artifact_version_retention.content_version
                     WHERE artifact_version_retention.artifact_id=?1
                       AND artifact_version_retention.state=1
                     ORDER BY artifact_version_retention.content_version LIMIT ?2"
                ))
                .map_err(map_sqlite)?;
            collect_rows(statement.query_map(
                params![artifact_id.as_str(), sql_limit(limit)],
                retention_record_from_row,
            ))
        })
        .await
    }

    async fn mark_content_purged(
        &self,
        artifact_id: &ArtifactId,
        content_version: u32,
        expected_revision: u64,
        now: u64,
    ) -> Result<ArtifactRetentionRecord, StoreError> {
        let artifact_id = artifact_id.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            let plan = query_removal_plan(&transaction, artifact_id.as_str())?;
            if plan.state != ArtifactRemovalState::Pending {
                return Err(StoreError::Conflict);
            }
            let mut record = query_retention_record(&transaction, &artifact_id, content_version)?;
            if record.revision != expected_revision {
                return Err(StoreError::Conflict);
            }
            record
                .record_purged(now)
                .map_err(|_| StoreError::Conflict)?;
            let changed = transaction
                .execute(
                    "UPDATE artifact_version_retention
                     SET state=2,revision=?1,updated_at=?2,purged_at=?2
                     WHERE artifact_id=?3 AND content_version=?4
                       AND state=1 AND revision=?5",
                    params![
                        number(record.revision)?,
                        number(record.updated_at)?,
                        artifact_id.as_str(),
                        i64::from(content_version),
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            let stored = query_retention_record(&transaction, &artifact_id, content_version)?;
            if stored != record {
                return Err(integrity(
                    "artifact purge transition changed during storage",
                ));
            }
            transaction.commit().map_err(map_sqlite)?;
            Ok(stored)
        })
        .await
    }

    async fn commit_removal(
        &self,
        artifact_id: &ArtifactId,
        expected_revision: u64,
        now: u64,
    ) -> Result<ArtifactRemovalPlan, StoreError> {
        let artifact_id = artifact_id.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            let mut plan = query_removal_plan(&transaction, artifact_id.as_str())?;
            if plan.revision != expected_revision {
                return Err(StoreError::Conflict);
            }
            let incomplete: bool = transaction
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM artifact_version_retention
                         WHERE artifact_id=?1 AND state!=2
                     )",
                    [artifact_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if incomplete {
                return Err(StoreError::Conflict);
            }
            plan.commit(now).map_err(|_| StoreError::Conflict)?;
            let changed = transaction
                .execute(
                    "UPDATE artifact_removal_commands
                     SET state=1,revision=?1,active_slot=NULL,updated_at=?2
                     WHERE artifact_id=?3 AND state=0 AND revision=?4",
                    params![
                        number(plan.revision)?,
                        number(plan.updated_at)?,
                        artifact_id.as_str(),
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            let stored = query_removal_plan(&transaction, artifact_id.as_str())?;
            if stored != plan {
                return Err(integrity("artifact removal commit changed during storage"));
            }
            transaction.commit().map_err(map_sqlite)?;
            Ok(stored)
        })
        .await
    }

    async fn list_incomplete_removals(
        &self,
        limit: usize,
    ) -> Result<Vec<ArtifactRemovalPlan>, StoreError> {
        self.with_store(move |connection| {
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {REMOVAL_PLAN_COLUMNS}
                     FROM artifact_removal_commands
                     JOIN artifacts ON artifacts.id=artifact_removal_commands.artifact_id
                     WHERE artifact_removal_commands.state=0
                     ORDER BY artifact_removal_commands.created_at,
                              artifact_removal_commands.artifact_id LIMIT ?1"
                ))
                .map_err(map_sqlite)?;
            collect_rows(statement.query_map([sql_limit(limit)], removal_plan_from_row))
        })
        .await
    }
}

fn begin(connection: &mut Connection) -> Result<Transaction<'_>, StoreError> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite)
}

fn ensure_command_scope(command: &MutationCommand, expected: &str) -> Result<(), StoreError> {
    if command.scope != expected {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn ensure_active_artifact_owner(
    connection: &Connection,
    artifact: &Artifact,
) -> Result<(), StoreError> {
    let project_state = connection
        .query_row(
            "SELECT state FROM projects WHERE id=?1",
            [artifact.project_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    match project_state {
        None => return Err(StoreError::NotFound),
        Some(0) => {}
        Some(_) => return Err(StoreError::Conflict),
    }
    if let Some(thread_id) = &artifact.thread_id {
        let thread_is_open = connection
            .query_row(
                "SELECT 1 FROM threads
                 WHERE id=?1 AND project_id=?2 AND state=0",
                params![thread_id.as_str(), artifact.project_id.as_str()],
                |_| Ok(true),
            )
            .optional()
            .map_err(map_sqlite)?
            .unwrap_or(false);
        if !thread_is_open {
            return Err(StoreError::Conflict);
        }
    }
    Ok(())
}

fn project_artifact_count(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<u64, StoreError> {
    unsigned_value(
        connection
            .query_row(
                "SELECT count(*) FROM artifacts WHERE project_id=?1 AND state!=2",
                [project_id.as_str()],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?,
    )
}

fn query_artifact(connection: &Connection, id: &str) -> Result<Artifact, StoreError> {
    connection
        .query_row(
            &format!("SELECT {ARTIFACT_COLUMNS} FROM artifacts WHERE id=?1"),
            [id],
            mapping::artifact_from_row,
        )
        .map_err(map_sqlite)
}

fn query_artifact_version(
    connection: &Connection,
    artifact_id: &ArtifactId,
    version: u32,
) -> Result<ArtifactVersion, StoreError> {
    connection
        .query_row(
            &format!(
                "SELECT {ARTIFACT_VERSION_COLUMNS} FROM artifact_versions
                 WHERE artifact_id=?1 AND version=?2"
            ),
            params![artifact_id.as_str(), i64::from(version)],
            mapping::artifact_version_from_row,
        )
        .map_err(map_sqlite)
}

fn query_import_plan(
    connection: &Connection,
    artifact_id: &str,
) -> Result<ArtifactImportPlan, StoreError> {
    connection
        .query_row(
            &format!(
                "SELECT {IMPORT_PLAN_COLUMNS}
                 FROM artifact_ingestions
                 JOIN artifacts ON artifacts.id=artifact_ingestions.artifact_id
                 WHERE artifact_ingestions.artifact_id=?1"
            ),
            [artifact_id],
            import_plan_from_row,
        )
        .map_err(map_sqlite)
}

fn import_plan_from_row(row: &Row<'_>) -> rusqlite::Result<ArtifactImportPlan> {
    let artifact = mapping::artifact_from_row(row)?;
    let state = import_state_from_i64(row.get(11)?)?;
    let digest = row.get::<_, Option<Vec<u8>>>(12)?;
    let media_type = row.get::<_, Option<String>>(13)?;
    let byte_size = optional_unsigned(row, 14)?;
    let content_created_at = optional_unsigned(row, 15)?;
    let content = match (digest, media_type, byte_size, content_created_at) {
        (None, None, None, None) => None,
        (Some(digest), Some(media_type), Some(byte_size), Some(created_at)) => {
            let sha256 = digest
                .try_into()
                .map_err(|_| invalid_row(12, "artifact digest"))?;
            Some(
                ArtifactVersion::restore(ArtifactVersion {
                    artifact_id: artifact.id.clone(),
                    version: 1,
                    sha256,
                    media_type,
                    byte_size,
                    created_at,
                })
                .map_err(|_| invalid_row(12, "artifact version"))?,
            )
        }
        _ => return Err(invalid_row(12, "artifact content journal")),
    };
    let failure = row
        .get::<_, Option<String>>(16)?
        .map(|value| import_failure_from_str(&value))
        .transpose()?;
    let plan = ArtifactImportPlan {
        artifact,
        state,
        content,
        failure,
        revision: unsigned(row, 17)?,
        created_at: unsigned(row, 18)?,
        updated_at: unsigned(row, 19)?,
    };
    ArtifactImportPlan::restore(plan).map_err(|_| invalid_row(11, "artifact import plan"))
}

fn query_removal_plan(
    connection: &Connection,
    artifact_id: &str,
) -> Result<ArtifactRemovalPlan, StoreError> {
    let plan = connection
        .query_row(
            &format!(
                "SELECT {REMOVAL_PLAN_COLUMNS}
                 FROM artifact_removal_commands
                 JOIN artifacts ON artifacts.id=artifact_removal_commands.artifact_id
                 WHERE artifact_removal_commands.artifact_id=?1"
            ),
            [artifact_id],
            removal_plan_from_row,
        )
        .map_err(map_sqlite)?;
    validate_removal_retention(connection, &plan)?;
    Ok(plan)
}

fn validate_removal_retention(
    connection: &Connection,
    plan: &ArtifactRemovalPlan,
) -> Result<(), StoreError> {
    let expected_state = match plan.state {
        ArtifactRemovalState::Pending => 0_i64,
        ArtifactRemovalState::Committed => 1_i64,
    };
    let (versions, retention, invalid): (i64, i64, i64) = connection
        .query_row(
            "SELECT count(version.version),count(retention.content_version),
                    COALESCE(sum(CASE
                        WHEN ?2=0 AND retention.state IN (1,2) THEN 0
                        WHEN ?2=1 AND retention.state=2 THEN 0
                        ELSE 1 END),0)
             FROM artifact_versions version
             LEFT JOIN artifact_version_retention retention
               ON retention.artifact_id=version.artifact_id
              AND retention.content_version=version.version
             WHERE version.artifact_id=?1",
            params![plan.artifact.id.as_str(), expected_state],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(map_sqlite)?;
    let extra_retention: bool = connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM artifact_version_retention retention
                 LEFT JOIN artifact_versions version
                   ON version.artifact_id=retention.artifact_id
                  AND version.version=retention.content_version
                 WHERE retention.artifact_id=?1 AND version.artifact_id IS NULL
             )",
            [plan.artifact.id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if versions <= 0 || versions != retention || invalid != 0 || extra_retention {
        return Err(integrity("artifact removal retention is inconsistent"));
    }
    Ok(())
}

fn removal_plan_from_row(row: &Row<'_>) -> rusqlite::Result<ArtifactRemovalPlan> {
    let plan = ArtifactRemovalPlan {
        artifact: mapping::artifact_from_row(row)?,
        state: removal_state_from_i64(row.get(11)?)?,
        revision: unsigned(row, 12)?,
        created_at: unsigned(row, 13)?,
        updated_at: unsigned(row, 14)?,
    };
    ArtifactRemovalPlan::restore(plan).map_err(|_| invalid_row(11, "artifact removal plan"))
}

fn query_retention_record(
    connection: &Connection,
    artifact_id: &ArtifactId,
    content_version: u32,
) -> Result<ArtifactRetentionRecord, StoreError> {
    connection
        .query_row(
            &format!(
                "SELECT {RETENTION_COLUMNS}
                 FROM artifact_version_retention
                 JOIN artifact_versions
                   ON artifact_versions.artifact_id=artifact_version_retention.artifact_id
                  AND artifact_versions.version=artifact_version_retention.content_version
                 WHERE artifact_version_retention.artifact_id=?1
                   AND artifact_version_retention.content_version=?2"
            ),
            params![artifact_id.as_str(), i64::from(content_version)],
            retention_record_from_row,
        )
        .map_err(map_sqlite)
}

fn retention_record_from_row(row: &Row<'_>) -> rusqlite::Result<ArtifactRetentionRecord> {
    let record = ArtifactRetentionRecord {
        content: mapping::artifact_version_from_row(row)?,
        state: retention_state_from_i64(row.get(6)?)?,
        revision: unsigned(row, 7)?,
        created_at: unsigned(row, 8)?,
        updated_at: unsigned(row, 9)?,
        purged_at: optional_unsigned(row, 10)?,
    };
    ArtifactRetentionRecord::restore(record)
        .map_err(|_| invalid_row(6, "artifact version retention record"))
}

fn update_failed_import(
    transaction: &Transaction<'_>,
    plan: &ArtifactImportPlan,
    expected_revision: u64,
) -> Result<(), StoreError> {
    let failure = plan
        .failure
        .ok_or_else(|| integrity("failed artifact import has no failure code"))?;
    let changed = transaction
        .execute(
            "UPDATE artifact_ingestions
             SET state=3,revision=?1,active_slot=NULL,failure_code=?2,updated_at=?3
             WHERE artifact_id=?4 AND state IN (0,1) AND revision=?5",
            params![
                number(plan.revision)?,
                import_failure_to_str(failure),
                number(plan.updated_at)?,
                plan.artifact.id.as_str(),
                number(expected_revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    ensure_changed(changed)
}

fn quota_failure_for_content(
    connection: &Connection,
    project_id: &ProjectId,
    content: &ArtifactVersion,
) -> Result<Option<ArtifactImportFailureCode>, StoreError> {
    if content.byte_size > MAX_ARTIFACT_FILE_BYTES {
        return Ok(Some(ArtifactImportFailureCode::FileTooLarge));
    }
    let (project_committed, global_committed) = committed_bytes(connection, project_id)?;
    let (project_reserved, global_reserved) = active_reserved_bytes(connection, project_id)?;
    let project_total = project_committed
        .checked_add(project_reserved)
        .and_then(|value| value.checked_add(content.byte_size))
        .ok_or_else(|| integrity("artifact project byte accounting overflow"))?;
    if project_total > MAX_PROJECT_ARTIFACT_BYTES {
        return Ok(Some(ArtifactImportFailureCode::ProjectByteQuotaExceeded));
    }
    let global_total = global_committed
        .checked_add(global_reserved)
        .and_then(|value| value.checked_add(content.byte_size))
        .ok_or_else(|| integrity("artifact global byte accounting overflow"))?;
    if global_total > MAX_GLOBAL_ARTIFACT_BYTES {
        return Ok(Some(ArtifactImportFailureCode::GlobalByteQuotaExceeded));
    }
    Ok(None)
}

fn ensure_quota_still_reserved(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<(), StoreError> {
    let (project_committed, global_committed) = committed_bytes(connection, project_id)?;
    let (project_reserved, global_reserved) = active_reserved_bytes(connection, project_id)?;
    if project_committed
        .checked_add(project_reserved)
        .is_none_or(|total| total > MAX_PROJECT_ARTIFACT_BYTES)
        || global_committed
            .checked_add(global_reserved)
            .is_none_or(|total| total > MAX_GLOBAL_ARTIFACT_BYTES)
    {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn committed_bytes(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<(u64, u64), StoreError> {
    ensure_complete_retention(connection)?;
    let project = sum_bytes(
        connection,
        "SELECT COALESCE(sum(artifact_versions.byte_size),0)
         FROM artifact_versions
         JOIN artifacts ON artifacts.id=artifact_versions.artifact_id
         JOIN artifact_version_retention
           ON artifact_version_retention.artifact_id=artifact_versions.artifact_id
          AND artifact_version_retention.content_version=artifact_versions.version
         WHERE artifacts.project_id=?1 AND artifact_version_retention.state!=2",
        Some(project_id.as_str()),
    )?;
    let global = sum_bytes(
        connection,
        "SELECT COALESCE(sum(artifact_versions.byte_size),0)
         FROM artifact_versions
         JOIN artifact_version_retention
           ON artifact_version_retention.artifact_id=artifact_versions.artifact_id
          AND artifact_version_retention.content_version=artifact_versions.version
         WHERE artifact_version_retention.state!=2",
        None,
    )?;
    Ok((project, global))
}

fn ensure_complete_retention(connection: &Connection) -> Result<(), StoreError> {
    let missing: bool = connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM artifact_versions version
                 LEFT JOIN artifact_version_retention retention
                   ON retention.artifact_id=version.artifact_id
                  AND retention.content_version=version.version
                 WHERE retention.artifact_id IS NULL
             )",
            [],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if missing {
        return Err(integrity("artifact version retention is incomplete"));
    }
    Ok(())
}

fn active_reserved_bytes(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<(u64, u64), StoreError> {
    let project = sum_bytes(
        connection,
        "SELECT COALESCE(sum(artifact_ingestions.content_byte_size),0)
         FROM artifact_ingestions
         JOIN artifacts ON artifacts.id=artifact_ingestions.artifact_id
         WHERE artifact_ingestions.state=1 AND artifacts.project_id=?1",
        Some(project_id.as_str()),
    )?;
    let global = sum_bytes(
        connection,
        "SELECT COALESCE(sum(content_byte_size),0)
         FROM artifact_ingestions WHERE state=1",
        None,
    )?;
    Ok((project, global))
}

fn sum_bytes(connection: &Connection, sql: &str, value: Option<&str>) -> Result<u64, StoreError> {
    let total = match value {
        Some(value) => connection.query_row(sql, [value], |row| row.get::<_, i64>(0)),
        None => connection.query_row(sql, [], |row| row.get::<_, i64>(0)),
    }
    .map_err(map_sqlite)?;
    unsigned_value(total)
}

fn quota_usage(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<ArtifactQuotaUsage, StoreError> {
    ensure_project_exists(connection, project_id)?;
    let project_artifact_count = project_artifact_count(connection, project_id)?;
    let (project_bytes, global_bytes) = committed_bytes(connection, project_id)?;
    Ok(ArtifactQuotaUsage {
        project_artifact_count,
        project_bytes,
        global_bytes,
    })
}

fn ensure_project_exists(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<(), StoreError> {
    let exists = connection
        .query_row(
            "SELECT 1 FROM projects WHERE id=?1",
            [project_id.as_str()],
            |_| Ok(true),
        )
        .optional()
        .map_err(map_sqlite)?
        .unwrap_or(false);
    if !exists {
        return Err(StoreError::NotFound);
    }
    Ok(())
}

fn artifact_cursor(
    connection: &Connection,
    project_id: &ProjectId,
    artifact_id: &ArtifactId,
) -> Result<(i64, String), StoreError> {
    connection
        .query_row(
            "SELECT updated_at,id FROM artifacts WHERE id=?1 AND project_id=?2",
            params![artifact_id.as_str(), project_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(map_sqlite)
}

fn is_current_available_content(
    connection: &Connection,
    content: &ArtifactVersion,
) -> Result<bool, StoreError> {
    connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM artifacts
                 WHERE id=?1 AND state=1 AND current_content_version=?2
             )",
            params![content.artifact_id.as_str(), i64::from(content.version)],
            |row| row.get(0),
        )
        .map_err(map_sqlite)
}

fn query_open_plan_by_command(
    connection: &Connection,
    idempotency_key: &str,
) -> Result<ArtifactOpenPlan, StoreError> {
    connection
        .query_row(
            &format!(
                "SELECT {OPEN_PLAN_COLUMNS}
                 FROM artifact_open_commands
                 JOIN artifact_versions
                   ON artifact_versions.artifact_id=artifact_open_commands.artifact_id
                  AND artifact_versions.version=artifact_open_commands.content_version
                 WHERE artifact_open_commands.command_scope='open_artifact'
                   AND artifact_open_commands.idempotency_key=?1"
            ),
            [idempotency_key],
            open_plan_from_row,
        )
        .map_err(map_sqlite)
}

fn open_plan_from_row(row: &Row<'_>) -> rusqlite::Result<ArtifactOpenPlan> {
    let plan = ArtifactOpenPlan {
        content: mapping::artifact_version_from_row(row)?,
        state: open_state_from_i64(row.get(6)?)?,
        failure: row
            .get::<_, Option<String>>(7)?
            .map(|value| open_failure_from_str(&value))
            .transpose()?,
        revision: unsigned(row, 8)?,
        created_at: unsigned(row, 9)?,
        updated_at: unsigned(row, 10)?,
    };
    ArtifactOpenPlan::restore(plan).map_err(|_| invalid_row(6, "artifact open plan"))
}

struct StoredOpenPlan {
    idempotency_key: String,
    plan: ArtifactOpenPlan,
}

fn query_active_open_plan(
    connection: &Connection,
    artifact_id: &ArtifactId,
    content_version: u32,
) -> Result<StoredOpenPlan, StoreError> {
    connection
        .query_row(
            &format!(
                "SELECT artifact_open_commands.idempotency_key,{OPEN_PLAN_COLUMNS}
                 FROM artifact_open_commands
                 JOIN artifact_versions
                   ON artifact_versions.artifact_id=artifact_open_commands.artifact_id
                  AND artifact_versions.version=artifact_open_commands.content_version
                 WHERE artifact_open_commands.artifact_id=?1
                   AND artifact_open_commands.content_version=?2
                   AND artifact_open_commands.state IN (0,1)"
            ),
            params![artifact_id.as_str(), i64::from(content_version)],
            |row| {
                let idempotency_key = row.get(0)?;
                let plan = open_plan_from_offset_row(row, 1)?;
                Ok(StoredOpenPlan {
                    idempotency_key,
                    plan,
                })
            },
        )
        .map_err(map_sqlite)
}

fn open_plan_from_offset_row(row: &Row<'_>, offset: usize) -> rusqlite::Result<ArtifactOpenPlan> {
    let digest = row.get::<_, Vec<u8>>(offset + 2)?;
    let content = ArtifactVersion::restore(ArtifactVersion {
        artifact_id: ArtifactId::new(row.get::<_, String>(offset)?)
            .map_err(|_| invalid_row(offset, "artifact identifier"))?,
        version: u32::try_from(unsigned(row, offset + 1)?)
            .map_err(|_| invalid_row(offset + 1, "artifact version"))?,
        sha256: digest
            .try_into()
            .map_err(|_| invalid_row(offset + 2, "artifact digest"))?,
        media_type: row.get(offset + 3)?,
        byte_size: unsigned(row, offset + 4)?,
        created_at: unsigned(row, offset + 5)?,
    })
    .map_err(|_| invalid_row(offset + 1, "artifact version"))?;
    let plan = ArtifactOpenPlan {
        content,
        state: open_state_from_i64(row.get(offset + 6)?)?,
        failure: row
            .get::<_, Option<String>>(offset + 7)?
            .map(|value| open_failure_from_str(&value))
            .transpose()?,
        revision: unsigned(row, offset + 8)?,
        created_at: unsigned(row, offset + 9)?,
        updated_at: unsigned(row, offset + 10)?,
    };
    ArtifactOpenPlan::restore(plan).map_err(|_| invalid_row(offset + 6, "artifact open plan"))
}

enum OpenTransition {
    Dispatch,
    Complete,
    Fail(ArtifactOpenFailureCode),
    Interrupt,
}

async fn transition_open(
    store: &SqlCipherStore,
    artifact_id: ArtifactId,
    content_version: u32,
    expected_revision: u64,
    now: u64,
    transition: OpenTransition,
) -> Result<ArtifactOpenPlan, StoreError> {
    store
        .with_store(move |connection| {
            let transaction = begin(connection)?;
            let stored = query_active_open_plan(&transaction, &artifact_id, content_version)?;
            if stored.plan.revision != expected_revision {
                return Err(StoreError::Conflict);
            }
            let mut plan = stored.plan;
            match transition {
                OpenTransition::Dispatch => plan.begin_dispatch(now),
                OpenTransition::Complete => plan.complete(now),
                OpenTransition::Fail(failure) => plan.fail(failure, now),
                OpenTransition::Interrupt => plan.interrupt(now),
            }
            .map_err(|_| StoreError::Conflict)?;
            let changed = transaction
                .execute(
                    "UPDATE artifact_open_commands
                     SET state=?1,revision=?2,active_slot=?3,failure_code=?4,updated_at=?5
                     WHERE command_scope='open_artifact' AND idempotency_key=?6
                       AND state IN (0,1) AND revision=?7",
                    params![
                        open_state_to_i64(plan.state),
                        number(plan.revision)?,
                        open_active_slot(plan.state),
                        plan.failure.map(open_failure_to_str),
                        number(plan.updated_at)?,
                        stored.idempotency_key,
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            let result = query_open_plan_by_command(&transaction, &stored.idempotency_key)?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(result)
        })
        .await
}

fn import_state_from_i64(value: i64) -> rusqlite::Result<ArtifactImportState> {
    match value {
        0 => Ok(ArtifactImportState::Prepared),
        1 => Ok(ArtifactImportState::ContentReady),
        2 => Ok(ArtifactImportState::Committed),
        3 => Ok(ArtifactImportState::Failed),
        _ => Err(invalid_row(11, "artifact import state")),
    }
}

fn removal_state_from_i64(value: i64) -> rusqlite::Result<ArtifactRemovalState> {
    match value {
        0 => Ok(ArtifactRemovalState::Pending),
        1 => Ok(ArtifactRemovalState::Committed),
        _ => Err(invalid_row(11, "artifact removal state")),
    }
}

fn retention_state_from_i64(value: i64) -> rusqlite::Result<ArtifactRetentionState> {
    match value {
        0 => Ok(ArtifactRetentionState::Retained),
        1 => Ok(ArtifactRetentionState::PurgePending),
        2 => Ok(ArtifactRetentionState::Purged),
        _ => Err(invalid_row(6, "artifact retention state")),
    }
}

fn open_state_from_i64(value: i64) -> rusqlite::Result<ArtifactOpenState> {
    match value {
        0 => Ok(ArtifactOpenState::Prepared),
        1 => Ok(ArtifactOpenState::Dispatching),
        2 => Ok(ArtifactOpenState::Opened),
        3 => Ok(ArtifactOpenState::Failed),
        4 => Ok(ArtifactOpenState::InterruptedNeedsReview),
        _ => Err(invalid_row(6, "artifact open state")),
    }
}

const fn open_state_to_i64(value: ArtifactOpenState) -> i64 {
    match value {
        ArtifactOpenState::Prepared => 0,
        ArtifactOpenState::Dispatching => 1,
        ArtifactOpenState::Opened => 2,
        ArtifactOpenState::Failed => 3,
        ArtifactOpenState::InterruptedNeedsReview => 4,
    }
}

const fn open_active_slot(value: ArtifactOpenState) -> Option<i64> {
    match value {
        ArtifactOpenState::Prepared | ArtifactOpenState::Dispatching => Some(1),
        ArtifactOpenState::Opened
        | ArtifactOpenState::Failed
        | ArtifactOpenState::InterruptedNeedsReview => None,
    }
}

const fn import_failure_to_str(value: ArtifactImportFailureCode) -> &'static str {
    match value {
        ArtifactImportFailureCode::SourceUnavailable => "source_unavailable",
        ArtifactImportFailureCode::SourceChanged => "source_changed",
        ArtifactImportFailureCode::FileTooLarge => "file_too_large",
        ArtifactImportFailureCode::ProjectByteQuotaExceeded => "project_byte_quota_exceeded",
        ArtifactImportFailureCode::GlobalByteQuotaExceeded => "global_byte_quota_exceeded",
        ArtifactImportFailureCode::ProjectCountQuotaExceeded => "project_count_quota_exceeded",
        ArtifactImportFailureCode::DeadlineExceeded => "deadline_exceeded",
        ArtifactImportFailureCode::IntegrityFailure => "integrity_failure",
        ArtifactImportFailureCode::ContentStoreUnavailable => "content_store_unavailable",
        ArtifactImportFailureCode::InterruptedBeforeContentReady => {
            "interrupted_before_content_ready"
        }
    }
}

fn import_failure_from_str(value: &str) -> rusqlite::Result<ArtifactImportFailureCode> {
    match value {
        "source_unavailable" => Ok(ArtifactImportFailureCode::SourceUnavailable),
        "source_changed" => Ok(ArtifactImportFailureCode::SourceChanged),
        "file_too_large" => Ok(ArtifactImportFailureCode::FileTooLarge),
        "project_byte_quota_exceeded" => Ok(ArtifactImportFailureCode::ProjectByteQuotaExceeded),
        "global_byte_quota_exceeded" => Ok(ArtifactImportFailureCode::GlobalByteQuotaExceeded),
        "project_count_quota_exceeded" => Ok(ArtifactImportFailureCode::ProjectCountQuotaExceeded),
        "deadline_exceeded" => Ok(ArtifactImportFailureCode::DeadlineExceeded),
        "integrity_failure" => Ok(ArtifactImportFailureCode::IntegrityFailure),
        "content_store_unavailable" => Ok(ArtifactImportFailureCode::ContentStoreUnavailable),
        "interrupted_before_content_ready" => {
            Ok(ArtifactImportFailureCode::InterruptedBeforeContentReady)
        }
        _ => Err(invalid_row(16, "artifact import failure")),
    }
}

const fn open_failure_to_str(value: ArtifactOpenFailureCode) -> &'static str {
    match value {
        ArtifactOpenFailureCode::ContentUnavailable => "content_unavailable",
        ArtifactOpenFailureCode::PlatformUnavailable => "platform_unavailable",
        ArtifactOpenFailureCode::DeadlineExceeded => "deadline_exceeded",
        ArtifactOpenFailureCode::IntegrityFailure => "integrity_failure",
        ArtifactOpenFailureCode::InterruptedBeforeDispatch => "interrupted_before_dispatch",
    }
}

fn open_failure_from_str(value: &str) -> rusqlite::Result<ArtifactOpenFailureCode> {
    match value {
        "content_unavailable" => Ok(ArtifactOpenFailureCode::ContentUnavailable),
        "platform_unavailable" => Ok(ArtifactOpenFailureCode::PlatformUnavailable),
        "deadline_exceeded" => Ok(ArtifactOpenFailureCode::DeadlineExceeded),
        "integrity_failure" => Ok(ArtifactOpenFailureCode::IntegrityFailure),
        "interrupted_before_dispatch" => Ok(ArtifactOpenFailureCode::InterruptedBeforeDispatch),
        _ => Err(invalid_row(7, "artifact open failure")),
    }
}

fn optional_unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<Option<u64>> {
    row.get::<_, Option<i64>>(index)?
        .map(|value| {
            u64::try_from(value).map_err(|_| invalid_row(index, "unsigned artifact value"))
        })
        .transpose()
}

fn unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    u64::try_from(row.get::<_, i64>(index)?)
        .map_err(|_| invalid_row(index, "unsigned artifact value"))
}

fn unsigned_value(value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| integrity("negative artifact accounting value"))
}

fn number(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| integrity("artifact numeric value exceeds SQLite range"))
}

fn sql_limit(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn ensure_changed(changed: usize) -> Result<(), StoreError> {
    if changed != 1 {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn collect_rows<T>(
    rows: rusqlite::Result<rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<T>>>,
) -> Result<Vec<T>, StoreError> {
    rows.map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)
}

fn invalid_row(index: usize, description: &str) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        Type::Text,
        std::io::Error::new(std::io::ErrorKind::InvalidData, description).into(),
    )
}

fn integrity(message: &str) -> StoreError {
    StoreError::Internal(message.into())
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
