use std::path::Path;

use grok_application::DatabaseKey;
use grok_domain::{
    Automation, AutomationHistoryEntry, AutomationSchedule, AutomationState,
    MAX_CONVERSATION_TEXT_CHUNK_BYTES, MAX_MESSAGE_BYTES,
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use crate::{SqlCipherStoreError, mapping};

pub(crate) const LATEST_SCHEMA_VERSION: u32 = 19;

pub(crate) fn open_encrypted(
    path: &Path,
    key: &DatabaseKey,
) -> Result<Connection, SqlCipherStoreError> {
    prepare_database_file(path)?;
    harden_database_files(path)?;
    let mut connection = Connection::open(path)?;
    apply_encryption_key(&connection, key)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA secure_delete = ON;
         PRAGMA cipher_memory_security = ON;
         PRAGMA temp_store = MEMORY;",
    )?;
    migrate(&mut connection)?;
    connection.execute_batch("PRAGMA trusted_schema = OFF;")?;
    harden_database_files(path)?;
    Ok(connection)
}

pub(crate) fn apply_encryption_key(
    connection: &Connection,
    key: &DatabaseKey,
) -> Result<(), SqlCipherStoreError> {
    let key_hex = zeroize::Zeroizing::new(hex::encode(key.expose_secret()));
    let key_pragma = zeroize::Zeroizing::new(format!("PRAGMA key = \"x'{}'\";", key_hex.as_str()));
    connection.execute_batch(key_pragma.as_str())?;

    let cipher_version: Option<String> = connection
        .query_row("PRAGMA cipher_version", [], |row| row.get(0))
        .optional()?;
    if cipher_version.as_deref().is_none_or(str::is_empty) {
        return Err(SqlCipherStoreError::CipherUnavailable);
    }

    // Reading the schema forces SQLCipher to validate an existing database key.
    connection.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn prepare_database_file(path: &Path) -> Result<(), SqlCipherStoreError> {
    use std::fs::{OpenOptions, Permissions};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;
    file.set_permissions(Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn prepare_database_file(_path: &Path) -> Result<(), SqlCipherStoreError> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn harden_database_files(path: &Path) -> Result<(), SqlCipherStoreError> {
    use std::os::unix::fs::PermissionsExt;

    for suffix in ["", "-wal", "-shm"] {
        let mut candidate = path.as_os_str().to_os_string();
        candidate.push(suffix);
        match std::fs::set_permissions(candidate, std::fs::Permissions::from_mode(0o600)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn harden_database_files(_path: &Path) -> Result<(), SqlCipherStoreError> {
    Ok(())
}

fn migrate(connection: &mut Connection) -> Result<(), SqlCipherStoreError> {
    let current: u32 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if current > LATEST_SCHEMA_VERSION {
        return Err(SqlCipherStoreError::NewerSchema {
            found: current,
            supported: LATEST_SCHEMA_VERSION,
        });
    }
    for version in (current + 1)..=LATEST_SCHEMA_VERSION {
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        match version {
            1 => transaction.execute_batch(MIGRATION_1)?,
            2 => transaction.execute_batch(MIGRATION_2)?,
            3 => transaction.execute_batch(MIGRATION_3)?,
            4 => transaction.execute_batch(MIGRATION_4)?,
            5 => transaction.execute_batch(MIGRATION_5)?,
            6 => transaction.execute_batch(MIGRATION_6)?,
            7 => transaction.execute_batch(MIGRATION_7)?,
            8 => transaction.execute_batch(MIGRATION_8)?,
            9 => transaction.execute_batch(MIGRATION_9)?,
            10 => transaction.execute_batch(MIGRATION_10)?,
            11 => {
                let invalid_attempts: bool = transaction.query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM privileged_operation_attempts
                         WHERE deadline_unix_ms <= started_at
                            OR broker_boot_id = zeroblob(16)
                            OR guest_boot_id = zeroblob(16)
                            OR transport_operation_id = operation_id
                     )",
                    [],
                    |row| row.get(0),
                )?;
                if invalid_attempts {
                    return Err(rusqlite::Error::InvalidQuery.into());
                }
                transaction.execute_batch(MIGRATION_11)?;
            }
            12 => migrate_conversation_turn_events(&transaction)?,
            13 => transaction.execute_batch(MIGRATION_13)?,
            14 => transaction.execute_batch(MIGRATION_14)?,
            15 => transaction.execute_batch(MIGRATION_15)?,
            16 => transaction.execute_batch(MIGRATION_16)?,
            17 => migrate_artifacts_v17(&transaction)?,
            18 => migrate_artifact_retention_v18(&transaction)?,
            19 => migrate_automation_scheduler_v19(&transaction)?,
            _ => unreachable!("bounded by latest schema"),
        }
        transaction.execute(
            "INSERT INTO schema_migrations(version, applied_at_unix_ms)
             VALUES (?1, CAST(unixepoch('subsec') * 1000 AS INTEGER))",
            [version],
        )?;
        transaction.execute_batch(&format!("PRAGMA user_version = {version};"))?;
        transaction.commit()?;
    }
    Ok(())
}

const MIGRATION_1: &str = r"
CREATE TABLE schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at_unix_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE runs (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 8),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at)
) STRICT;
CREATE INDEX runs_project_updated ON runs(project_id, updated_at DESC);

CREATE TABLE approvals (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    action TEXT NOT NULL,
    target TEXT NOT NULL,
    data_summary TEXT NOT NULL,
    risk INTEGER NOT NULL,
    scope INTEGER NOT NULL,
    resource_id TEXT,
    status INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    decided_at INTEGER
) STRICT;
CREATE INDEX approvals_run_status ON approvals(run_id, status);

CREATE TABLE side_effects (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    kind INTEGER NOT NULL,
    target TEXT NOT NULL,
    idempotency INTEGER NOT NULL,
    state INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;
CREATE INDEX side_effects_run_state ON side_effects(run_id, state);

CREATE TABLE run_events (
    run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence BETWEEN 1 AND 4100),
    occurred_at INTEGER NOT NULL,
    kind INTEGER NOT NULL,
    from_state INTEGER,
    to_state INTEGER,
    related_id TEXT,
    PRIMARY KEY(run_id, sequence)
) WITHOUT ROWID, STRICT;
";

const MIGRATION_2: &str = r"
CREATE TABLE search_documents (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    updated_at INTEGER NOT NULL
) STRICT;

CREATE VIRTUAL TABLE search_documents_fts USING fts5(
    title,
    body,
    content='search_documents',
    content_rowid='rowid',
    tokenize='unicode61 remove_diacritics 2'
);

CREATE TRIGGER search_documents_ai AFTER INSERT ON search_documents BEGIN
    INSERT INTO search_documents_fts(rowid, title, body)
    VALUES (new.rowid, new.title, new.body);
END;
CREATE TRIGGER search_documents_ad AFTER DELETE ON search_documents BEGIN
    INSERT INTO search_documents_fts(search_documents_fts, rowid, title, body)
    VALUES ('delete', old.rowid, old.title, old.body);
END;
CREATE TRIGGER search_documents_au AFTER UPDATE ON search_documents BEGIN
    INSERT INTO search_documents_fts(search_documents_fts, rowid, title, body)
    VALUES ('delete', old.rowid, old.title, old.body);
    INSERT INTO search_documents_fts(rowid, title, body)
    VALUES (new.rowid, new.title, new.body);
END;
";

const MIGRATION_3: &str = r"
CREATE TABLE projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT NOT NULL,
    state INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at)
) STRICT;
CREATE INDEX projects_recent ON projects(updated_at DESC, id);

CREATE TABLE threads (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    title TEXT NOT NULL,
    state INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    UNIQUE(id, project_id)
) STRICT;
CREATE INDEX threads_project_recent ON threads(project_id, updated_at DESC, id);

CREATE TABLE messages (
    id TEXT PRIMARY KEY,
    thread_id TEXT NOT NULL REFERENCES threads(id),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    role INTEGER NOT NULL,
    content TEXT NOT NULL,
    state INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    UNIQUE(thread_id, sequence)
) STRICT;
CREATE INDEX messages_thread_order ON messages(thread_id, sequence);

CREATE TABLE artifacts (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    thread_id TEXT,
    name TEXT NOT NULL,
    relative_path TEXT NOT NULL,
    media_type TEXT NOT NULL,
    byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
    state INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    FOREIGN KEY(thread_id, project_id) REFERENCES threads(id, project_id)
) STRICT;
CREATE INDEX artifacts_project_recent ON artifacts(project_id, updated_at DESC, id);

CREATE TABLE automations (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL REFERENCES projects(id),
    title TEXT NOT NULL,
    prompt TEXT NOT NULL,
    schedule TEXT NOT NULL,
    timezone TEXT NOT NULL,
    missed_run_policy INTEGER NOT NULL,
    overlap_policy INTEGER NOT NULL,
    state INTEGER NOT NULL,
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at)
) STRICT;
CREATE INDEX automations_project_recent ON automations(project_id, updated_at DESC, id);

CREATE TABLE automation_history (
    automation_id TEXT NOT NULL REFERENCES automations(id),
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    scheduled_for INTEGER NOT NULL CHECK (scheduled_for >= 0),
    recorded_at INTEGER NOT NULL CHECK (recorded_at >= scheduled_for),
    status INTEGER NOT NULL,
    summary TEXT NOT NULL,
    PRIMARY KEY(automation_id, sequence),
    UNIQUE(automation_id, scheduled_for)
) WITHOUT ROWID, STRICT;

CREATE TABLE workspace_commands (
    scope TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    entity_id TEXT NOT NULL,
    PRIMARY KEY(scope, idempotency_key)
) WITHOUT ROWID, STRICT;

CREATE TRIGGER projects_search_ai AFTER INSERT ON projects BEGIN
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    VALUES (new.id, new.id, 'project', new.name, new.description, new.updated_at);
END;
CREATE TRIGGER projects_search_au AFTER UPDATE ON projects BEGIN
    UPDATE search_documents SET title=new.name, body=new.description, updated_at=new.updated_at
    WHERE id=new.id;
END;

CREATE TRIGGER threads_search_ai AFTER INSERT ON threads BEGIN
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    VALUES (new.id, new.project_id, 'thread', new.title, '', new.updated_at);
END;
CREATE TRIGGER threads_search_au AFTER UPDATE ON threads BEGIN
    UPDATE search_documents SET title=new.title, updated_at=new.updated_at WHERE id=new.id;
    UPDATE search_documents SET title=new.title
    WHERE id IN (SELECT id FROM messages WHERE thread_id=new.id);
END;

CREATE TRIGGER messages_search_ai AFTER INSERT ON messages WHEN new.state=0 BEGIN
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    SELECT new.id, threads.project_id, 'message', threads.title, new.content, new.updated_at
    FROM threads WHERE threads.id=new.thread_id;
END;
CREATE TRIGGER messages_search_au AFTER UPDATE ON messages BEGIN
    DELETE FROM search_documents WHERE id=new.id;
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    SELECT new.id, threads.project_id, 'message', threads.title, new.content, new.updated_at
    FROM threads WHERE threads.id=new.thread_id AND new.state=0;
END;

CREATE TRIGGER artifacts_search_ai AFTER INSERT ON artifacts WHEN new.state=0 BEGIN
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    VALUES (new.id, new.project_id, 'artifact', new.name, new.relative_path, new.updated_at);
END;
CREATE TRIGGER artifacts_search_au AFTER UPDATE ON artifacts BEGIN
    DELETE FROM search_documents WHERE id=new.id;
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    SELECT new.id, new.project_id, 'artifact', new.name, new.relative_path, new.updated_at
    WHERE new.state=0;
END;

CREATE TRIGGER automations_search_ai AFTER INSERT ON automations BEGIN
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    VALUES (new.id, new.project_id, 'automation', new.title, new.prompt, new.updated_at);
END;
CREATE TRIGGER automations_search_au AFTER UPDATE ON automations BEGIN
    UPDATE search_documents SET title=new.title, body=new.prompt, updated_at=new.updated_at
    WHERE id=new.id;
END;
";

const MIGRATION_4: &str = r"
CREATE TABLE execution_commands (
    scope TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    outcome_json TEXT NOT NULL CHECK (length(outcome_json) BETWEEN 2 AND 1048576),
    PRIMARY KEY(scope, idempotency_key)
) WITHOUT ROWID, STRICT;
";

const MIGRATION_5: &str = r"
CREATE TABLE credential_commands (
    scope TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    completed INTEGER NOT NULL CHECK (completed IN (0, 1)),
    xai_api_key_configured INTEGER CHECK (xai_api_key_configured IN (0, 1)),
    CHECK (
        (completed = 0 AND xai_api_key_configured IS NULL) OR
        (completed = 1 AND xai_api_key_configured IS NOT NULL)
    ),
    PRIMARY KEY(scope, idempotency_key)
) WITHOUT ROWID, STRICT;
";

const MIGRATION_6: &str = r"
ALTER TABLE credential_commands
ADD COLUMN xai_capabilities_resolved INTEGER CHECK (xai_capabilities_resolved IN (0, 1));

CREATE TABLE conversation_turns (
    id TEXT PRIMARY KEY,
    idempotency_key TEXT NOT NULL UNIQUE,
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    provider_request_fingerprint BLOB CHECK (
        provider_request_fingerprint IS NULL OR length(provider_request_fingerprint) = 32
    ),
    project_id TEXT NOT NULL REFERENCES projects(id),
    thread_id TEXT NOT NULL REFERENCES threads(id),
    user_message_id TEXT NOT NULL UNIQUE REFERENCES messages(id),
    run_id TEXT NOT NULL UNIQUE REFERENCES runs(id),
    model_id TEXT NOT NULL CHECK (length(model_id) BETWEEN 1 AND 512),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 5),
    effect_id TEXT UNIQUE REFERENCES side_effects(id),
    assistant_message_id TEXT UNIQUE REFERENCES messages(id),
    failure_kind INTEGER CHECK (failure_kind BETWEEN 0 AND 5),
    failure_message TEXT,
    failure_retryable INTEGER CHECK (failure_retryable IN (0, 1)),
    provider_response_id TEXT CHECK (
        provider_response_id IS NULL OR length(provider_response_id) BETWEEN 1 AND 512
    ),
    citations_json TEXT NOT NULL DEFAULT '[]' CHECK (
        length(citations_json) BETWEEN 2 AND 2500000
    ),
    input_tokens INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    cost_in_usd_ticks INTEGER NOT NULL DEFAULT 0 CHECK (cost_in_usd_ticks >= 0),
    zero_data_retention INTEGER CHECK (zero_data_retention IN (0, 1)),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    FOREIGN KEY(thread_id, project_id) REFERENCES threads(id, project_id),
    CHECK (
        (state = 0 AND effect_id IS NULL AND assistant_message_id IS NULL) OR
        (state = 1 AND effect_id IS NOT NULL AND assistant_message_id IS NULL) OR
        (state = 2 AND effect_id IS NOT NULL AND assistant_message_id IS NOT NULL) OR
        (state = 3 AND effect_id IS NOT NULL AND assistant_message_id IS NULL) OR
        (state = 4 AND effect_id IS NULL AND assistant_message_id IS NULL) OR
        (state = 5 AND effect_id IS NOT NULL AND assistant_message_id IS NULL)
    ),
    CHECK (
        (state = 3 AND failure_kind IS NOT NULL AND failure_message IS NOT NULL
                   AND failure_retryable IS NOT NULL) OR
        (state != 3 AND failure_kind IS NULL AND failure_message IS NULL
                    AND failure_retryable IS NULL)
    )
) STRICT;
CREATE UNIQUE INDEX conversation_turns_one_active_per_thread
ON conversation_turns(thread_id) WHERE state IN (0, 1);
CREATE INDEX conversation_turns_recovery
ON conversation_turns(state, created_at, id);

CREATE TABLE conversation_turn_context (
    turn_id TEXT NOT NULL REFERENCES conversation_turns(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    message_id TEXT NOT NULL,
    role INTEGER NOT NULL CHECK (role BETWEEN 0 AND 2),
    content TEXT NOT NULL CHECK (length(content) BETWEEN 1 AND 1048576),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    PRIMARY KEY(turn_id, sequence)
) WITHOUT ROWID, STRICT;
";

// Stable storage mappings for the closed privileged-operation domain:
// kind: 0 runner_health, 1 catalog_apply, 2 integration_start,
//       3 integration_stop, 4 computer_observe, 5 computer_act.
// retry_class: 0 retry_safe, 1 non_idempotent.
// state: 0 prepared, 1 dispatching, 2 retry_pending, 3 succeeded, 4 failed,
//        5 interrupted_needs_review, 6 reviewed, 7 cancelled.
// review disposition: 0 confirmed_succeeded, 1 confirmed_failed, 2 abandoned.
// attempt certainty: 0 dispatching, 1 definitely_not_dispatched,
//        2 known_success, 3 known_failure, 4 outcome_unknown.
const MIGRATION_7: &str = r"
CREATE UNIQUE INDEX side_effects_identity_run
ON side_effects(id, run_id);
CREATE UNIQUE INDEX approvals_identity_run
ON approvals(id, run_id);

CREATE TABLE privileged_operations (
    id TEXT PRIMARY KEY CHECK (
        length(CAST(id AS BLOB)) BETWEEN 1 AND 128
    ),
    operation_kind INTEGER NOT NULL CHECK (operation_kind BETWEEN 0 AND 5),
    retry_class INTEGER NOT NULL CHECK (
        retry_class = CASE
            WHEN operation_kind IN (0, 4) THEN 0
            ELSE 1
        END
    ),
    target_vm_id TEXT NOT NULL CHECK (
        length(CAST(target_vm_id AS BLOB)) BETWEEN 1 AND 128 AND
        target_vm_id NOT GLOB '*[^-A-Za-z0-9._:]*'
    ),
    target_integration_id TEXT CHECK (
        target_integration_id IS NULL OR (
            length(CAST(target_integration_id AS BLOB)) BETWEEN 1 AND 128 AND
            target_integration_id NOT GLOB '*[^-A-Za-z0-9._:]*'
        )
    ),
    target_instance_id TEXT CHECK (
        target_instance_id IS NULL OR (
            length(CAST(target_instance_id AS BLOB)) BETWEEN 1 AND 128 AND
            target_instance_id NOT GLOB '*[^-A-Za-z0-9._:]*'
        )
    ),
    target_application_id TEXT CHECK (
        target_application_id IS NULL OR (
            length(CAST(target_application_id AS BLOB)) BETWEEN 1 AND 128 AND
            target_application_id NOT GLOB '*[^-A-Za-z0-9._:]*'
        )
    ),
    target_observation_revision INTEGER CHECK (
        target_observation_revision IS NULL OR target_observation_revision > 0
    ),
    payload_digest BLOB NOT NULL CHECK (length(payload_digest) = 32),
    retained_payload_digest BLOB CHECK (
        retained_payload_digest IS NULL OR length(retained_payload_digest) = 32
    ),
    authority_grant_id TEXT NOT NULL CHECK (
        length(CAST(authority_grant_id AS BLOB)) BETWEEN 16 AND 128 AND
        authority_grant_id NOT GLOB '*[^-A-Za-z0-9._:]*'
    ),
    authority_expires_at INTEGER NOT NULL CHECK (authority_expires_at >= 0),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 16 AND 128 AND
        idempotency_key NOT GLOB '*[^-A-Za-z0-9._:]*'
    ),
    request_digest BLOB NOT NULL CHECK (length(request_digest) = 32),
    run_id TEXT REFERENCES runs(id) CHECK (
        run_id IS NULL OR length(CAST(run_id AS BLOB)) BETWEEN 1 AND 128
    ),
    effect_id TEXT CHECK (
        effect_id IS NULL OR length(CAST(effect_id AS BLOB)) BETWEEN 1 AND 128
    ),
    approval_id TEXT CHECK (
        approval_id IS NULL OR length(CAST(approval_id AS BLOB)) BETWEEN 1 AND 128
    ),
    supersedes_id TEXT REFERENCES privileged_operations(id) CHECK (
        supersedes_id IS NULL OR (
            length(CAST(supersedes_id AS BLOB)) BETWEEN 1 AND 128 AND
            supersedes_id <> id
        )
    ),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 7),
    review_disposition INTEGER CHECK (
        (state = 6 AND review_disposition IS NOT NULL AND
            review_disposition BETWEEN 0 AND 2) OR
        (state <> 6 AND review_disposition IS NULL)
    ),
    attempt_count INTEGER NOT NULL CHECK (attempt_count BETWEEN 0 AND 4294967295),
    last_attempt_sequence INTEGER CHECK (
        last_attempt_sequence IS NULL OR
        last_attempt_sequence BETWEEN 1 AND 4294967295
    ),
    last_attempt_certainty INTEGER CHECK (
        last_attempt_certainty IS NULL OR last_attempt_certainty BETWEEN 0 AND 4
    ),
    terminal_result_digest BLOB CHECK (
        terminal_result_digest IS NULL OR length(terminal_result_digest) = 32
    ),
    terminal_result_payload BLOB CHECK (
        terminal_result_payload IS NULL OR
        length(terminal_result_payload) BETWEEN 2 AND 8388608
    ),
    terminal_result_pruned INTEGER NOT NULL DEFAULT 0 CHECK (
        terminal_result_pruned IN (0, 1)
    ),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    UNIQUE(authority_grant_id, idempotency_key),
    UNIQUE(id, payload_digest),
    UNIQUE(id, retained_payload_digest),
    UNIQUE(id, review_disposition),
    UNIQUE(id, last_attempt_sequence, last_attempt_certainty),
    FOREIGN KEY(effect_id, run_id) REFERENCES side_effects(id, run_id),
    FOREIGN KEY(approval_id, run_id) REFERENCES approvals(id, run_id),
    FOREIGN KEY(id, retained_payload_digest)
        REFERENCES privileged_operation_payloads(operation_id, payload_digest)
        DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(id, last_attempt_sequence, last_attempt_certainty)
        REFERENCES privileged_operation_attempts(
            operation_id, sequence, outcome_certainty
        ) DEFERRABLE INITIALLY DEFERRED,
    FOREIGN KEY(id, review_disposition)
        REFERENCES privileged_operation_reviews(operation_id, disposition)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK (
        (effect_id IS NULL AND approval_id IS NULL) OR run_id IS NOT NULL
    ),
    CHECK (
        (operation_kind IN (0, 1) AND
            target_integration_id IS NULL AND
            target_instance_id IS NULL AND
            target_application_id IS NULL AND
            target_observation_revision IS NULL) OR
        (operation_kind IN (2, 4) AND
            target_integration_id IS NOT NULL AND
            target_instance_id IS NULL AND
            target_application_id IS NULL AND
            target_observation_revision IS NULL) OR
        (operation_kind = 3 AND
            target_integration_id IS NOT NULL AND
            target_instance_id IS NOT NULL AND
            target_application_id IS NULL AND
            target_observation_revision IS NULL) OR
        (operation_kind = 5 AND
            target_integration_id IS NOT NULL AND
            target_instance_id IS NOT NULL AND
            target_application_id IS NOT NULL AND
            target_observation_revision IS NOT NULL AND
            target_observation_revision > 0)
    ),
    CHECK (
        (state IN (0, 7) AND
            attempt_count = 0 AND
            last_attempt_sequence IS NULL AND
            last_attempt_certainty IS NULL) OR
        (state = 1 AND
            attempt_count > 0 AND
            last_attempt_sequence IS attempt_count AND
            last_attempt_certainty IS 0) OR
        (state = 2 AND retry_class = 0 AND
            attempt_count > 0 AND
            last_attempt_sequence IS attempt_count AND
            last_attempt_certainty IS NOT NULL AND
            last_attempt_certainty IN (1, 3, 4)) OR
        (state = 3 AND
            attempt_count > 0 AND
            last_attempt_sequence IS attempt_count AND
            last_attempt_certainty IS 2) OR
        (state = 4 AND
            attempt_count > 0 AND
            last_attempt_sequence IS attempt_count AND
            last_attempt_certainty IS NOT NULL AND
            last_attempt_certainty IN (1, 3)) OR
        (state IN (5, 6) AND retry_class = 1 AND
            attempt_count > 0 AND
            last_attempt_sequence IS attempt_count AND
            last_attempt_certainty IS 4)
    ),
    CHECK (
        (state = 0 AND revision = 0 AND updated_at = created_at) OR
        (state <> 0 AND revision > 0)
    ),
    CHECK (attempt_count <= revision),
    CHECK (authority_expires_at >= created_at),
    CHECK (
        (state IN (0, 1, 2, 5) AND retained_payload_digest IS payload_digest) OR
        (state IN (3, 4, 6, 7) AND retained_payload_digest IS NULL)
    ),
    CHECK (
        (state IN (3, 4) AND
            terminal_result_digest IS NOT NULL AND (
                (terminal_result_pruned = 0 AND terminal_result_payload IS NOT NULL) OR
                (terminal_result_pruned = 1 AND terminal_result_payload IS NULL)
            )) OR
        (state NOT IN (3, 4) AND
            terminal_result_digest IS NULL AND
            terminal_result_payload IS NULL AND
            terminal_result_pruned = 0)
    )
) STRICT;

CREATE TABLE privileged_operation_payloads (
    operation_id TEXT PRIMARY KEY,
    payload_digest BLOB NOT NULL CHECK (length(payload_digest) = 32),
    payload BLOB NOT NULL CHECK (length(payload) BETWEEN 2 AND 8388608),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    UNIQUE(operation_id, payload_digest),
    FOREIGN KEY(operation_id, payload_digest)
        REFERENCES privileged_operations(id, payload_digest)
        ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED
) STRICT;

CREATE TABLE privileged_operation_attempts (
    operation_id TEXT NOT NULL REFERENCES privileged_operations(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence BETWEEN 1 AND 4294967295),
    transport_operation_id TEXT NOT NULL UNIQUE CHECK (
        length(CAST(transport_operation_id AS BLOB)) BETWEEN 16 AND 128 AND
        transport_operation_id NOT GLOB '*[^-A-Za-z0-9._:]*'
    ),
    wire_digest BLOB NOT NULL CHECK (length(wire_digest) = 32),
    broker_boot_id BLOB NOT NULL CHECK (length(broker_boot_id) = 16),
    guest_boot_id BLOB NOT NULL CHECK (length(guest_boot_id) = 16),
    started_at INTEGER NOT NULL CHECK (started_at >= 0),
    deadline_unix_ms INTEGER NOT NULL CHECK (
        deadline_unix_ms > started_at AND
        deadline_unix_ms - started_at <= 30000
    ),
    completed_at INTEGER CHECK (
        completed_at IS NULL OR completed_at >= started_at
    ),
    outcome_certainty INTEGER NOT NULL CHECK (outcome_certainty BETWEEN 0 AND 4),
    result_digest BLOB CHECK (
        result_digest IS NULL OR length(result_digest) = 32
    ),
    failure_code TEXT CHECK (
        failure_code IS NULL OR (
            length(CAST(failure_code AS BLOB)) BETWEEN 1 AND 128 AND
            failure_code NOT GLOB '*[^-A-Za-z0-9._:]*'
        )
    ),
    PRIMARY KEY(operation_id, sequence),
    UNIQUE(operation_id, sequence, outcome_certainty),
    CHECK (transport_operation_id <> operation_id),
    CHECK (broker_boot_id <> zeroblob(16)),
    CHECK (guest_boot_id <> zeroblob(16)),
    CHECK (
        (outcome_certainty = 0 AND
            completed_at IS NULL AND result_digest IS NULL AND failure_code IS NULL) OR
        (outcome_certainty = 1 AND
            completed_at IS NOT NULL AND result_digest IS NULL) OR
        (outcome_certainty = 2 AND
            completed_at IS NOT NULL AND result_digest IS NOT NULL AND
            failure_code IS NULL) OR
        (outcome_certainty = 3 AND
            completed_at IS NOT NULL AND result_digest IS NOT NULL AND
            failure_code IS NOT NULL) OR
        (outcome_certainty = 4 AND
            completed_at IS NOT NULL AND result_digest IS NULL)
    )
) WITHOUT ROWID, STRICT;

CREATE TABLE privileged_operation_reviews (
    operation_id TEXT PRIMARY KEY REFERENCES privileged_operations(id) ON DELETE CASCADE,
    disposition INTEGER NOT NULL CHECK (disposition BETWEEN 0 AND 2),
    operation_revision INTEGER NOT NULL CHECK (operation_revision > 0),
    reviewed_at INTEGER NOT NULL CHECK (reviewed_at >= 0),
    actor_id TEXT NOT NULL CHECK (
        length(CAST(actor_id AS BLOB)) BETWEEN 1 AND 128 AND
        actor_id NOT GLOB '*[^-A-Za-z0-9._:]*'
    ),
    rationale TEXT NOT NULL CHECK (
        length(CAST(rationale AS BLOB)) BETWEEN 1 AND 4096
    ),
    replacement_operation_id TEXT REFERENCES privileged_operations(id) CHECK (
        replacement_operation_id IS NULL OR
        replacement_operation_id <> operation_id
    ),
    UNIQUE(operation_id, disposition)
) STRICT;

CREATE INDEX privileged_operations_recovery
ON privileged_operations(state, updated_at, id)
WHERE state IN (0, 1, 2, 5);
CREATE INDEX privileged_operations_run_lookup
ON privileged_operations(run_id, created_at, id)
WHERE run_id IS NOT NULL;
CREATE INDEX privileged_operations_effect_lookup
ON privileged_operations(effect_id)
WHERE effect_id IS NOT NULL;
CREATE INDEX privileged_operations_supersedes_lookup
ON privileged_operations(supersedes_id)
WHERE supersedes_id IS NOT NULL;
CREATE INDEX privileged_operations_target_lookup
ON privileged_operations(
    operation_kind, target_vm_id, target_integration_id, updated_at
);
CREATE INDEX privileged_operation_attempts_recovery
ON privileged_operation_attempts(deadline_unix_ms, operation_id, sequence)
WHERE outcome_certainty = 0;
CREATE INDEX privileged_operation_reviews_recent
ON privileged_operation_reviews(reviewed_at, operation_id);

CREATE TRIGGER privileged_operations_validate_insert
BEFORE INSERT ON privileged_operations BEGIN
    SELECT CASE WHEN
        new.state <> 0 OR
        new.review_disposition IS NOT NULL OR
        new.attempt_count <> 0 OR
        new.last_attempt_sequence IS NOT NULL OR
        new.last_attempt_certainty IS NOT NULL OR
        new.retained_payload_digest IS NOT new.payload_digest OR
        new.terminal_result_digest IS NOT NULL OR
        new.terminal_result_payload IS NOT NULL OR
        new.terminal_result_pruned <> 0 OR
        new.revision <> 0 OR
        new.updated_at <> new.created_at
    THEN RAISE(ABORT, 'privileged operation must begin prepared') END;
    SELECT CASE WHEN new.supersedes_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM privileged_operations
        WHERE id = new.supersedes_id AND state = 6
    ) THEN RAISE(ABORT, 'superseded operation must already be reviewed') END;
END;

CREATE TRIGGER privileged_operations_validate_update
BEFORE UPDATE ON privileged_operations BEGIN
    SELECT CASE WHEN
        new.id IS NOT old.id OR
        new.operation_kind IS NOT old.operation_kind OR
        new.retry_class IS NOT old.retry_class OR
        new.target_vm_id IS NOT old.target_vm_id OR
        new.target_integration_id IS NOT old.target_integration_id OR
        new.target_instance_id IS NOT old.target_instance_id OR
        new.target_application_id IS NOT old.target_application_id OR
        new.target_observation_revision IS NOT old.target_observation_revision OR
        new.payload_digest IS NOT old.payload_digest OR
        new.authority_grant_id IS NOT old.authority_grant_id OR
        new.authority_expires_at IS NOT old.authority_expires_at OR
        new.idempotency_key IS NOT old.idempotency_key OR
        new.request_digest IS NOT old.request_digest OR
        new.run_id IS NOT old.run_id OR
        new.effect_id IS NOT old.effect_id OR
        new.approval_id IS NOT old.approval_id OR
        new.supersedes_id IS NOT old.supersedes_id OR
        new.created_at IS NOT old.created_at
    THEN RAISE(ABORT, 'privileged operation intent is immutable') END;

    SELECT CASE WHEN
        new.revision <> old.revision + 1 OR
        new.updated_at < old.updated_at OR
        NOT (
            (old.state IN (0, 2) AND new.state = 1 AND
                new.attempt_count = old.attempt_count + 1 AND
                new.last_attempt_sequence = new.attempt_count AND
                new.last_attempt_certainty = 0 AND
                new.retained_payload_digest IS old.retained_payload_digest AND
                new.review_disposition IS old.review_disposition AND
                new.terminal_result_digest IS old.terminal_result_digest AND
                new.terminal_result_payload IS old.terminal_result_payload AND
                new.terminal_result_pruned = old.terminal_result_pruned) OR
            (old.state = 0 AND new.state = 7 AND
                new.attempt_count = old.attempt_count AND
                new.last_attempt_sequence IS old.last_attempt_sequence AND
                new.last_attempt_certainty IS old.last_attempt_certainty AND
                new.retained_payload_digest IS NULL AND
                new.review_disposition IS old.review_disposition AND
                new.terminal_result_digest IS old.terminal_result_digest AND
                new.terminal_result_payload IS old.terminal_result_payload AND
                new.terminal_result_pruned = old.terminal_result_pruned) OR
            (old.state = 1 AND new.state IN (2, 5) AND
                new.attempt_count = old.attempt_count AND
                new.last_attempt_sequence IS old.last_attempt_sequence AND
                new.last_attempt_certainty <> 0 AND
                new.retained_payload_digest IS old.retained_payload_digest AND
                new.review_disposition IS old.review_disposition AND
                new.terminal_result_digest IS old.terminal_result_digest AND
                new.terminal_result_payload IS old.terminal_result_payload AND
                new.terminal_result_pruned = old.terminal_result_pruned) OR
            (old.state = 1 AND new.state IN (3, 4) AND
                new.attempt_count = old.attempt_count AND
                new.last_attempt_sequence IS old.last_attempt_sequence AND
                new.last_attempt_certainty <> 0 AND
                new.retained_payload_digest IS NULL AND
                new.review_disposition IS old.review_disposition AND
                new.terminal_result_digest IS NOT NULL AND
                new.terminal_result_payload IS NOT NULL AND
                new.terminal_result_pruned = 0) OR
            (old.state = 5 AND new.state = 6 AND
                new.attempt_count = old.attempt_count AND
                new.last_attempt_sequence IS old.last_attempt_sequence AND
                new.last_attempt_certainty IS old.last_attempt_certainty AND
                new.retained_payload_digest IS NULL AND
                new.review_disposition IS NOT NULL AND
                new.terminal_result_digest IS old.terminal_result_digest AND
                new.terminal_result_payload IS old.terminal_result_payload AND
                new.terminal_result_pruned = old.terminal_result_pruned) OR
            (old.state IN (3, 4) AND new.state = old.state AND
                new.attempt_count = old.attempt_count AND
                new.last_attempt_sequence IS old.last_attempt_sequence AND
                new.last_attempt_certainty IS old.last_attempt_certainty AND
                new.retained_payload_digest IS old.retained_payload_digest AND
                new.review_disposition IS old.review_disposition AND
                new.terminal_result_digest IS old.terminal_result_digest AND
                old.terminal_result_payload IS NOT NULL AND
                new.terminal_result_payload IS NULL AND
                old.terminal_result_pruned = 0 AND
                new.terminal_result_pruned = 1)
        )
    THEN RAISE(ABORT, 'invalid privileged operation lifecycle mutation') END;
END;

CREATE TRIGGER privileged_operations_reject_delete
BEFORE DELETE ON privileged_operations BEGIN
    SELECT RAISE(ABORT, 'privileged operation tombstones are immutable');
END;

CREATE TRIGGER privileged_operation_payloads_validate_insert
BEFORE INSERT ON privileged_operation_payloads BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM privileged_operations
        WHERE id = new.operation_id
          AND payload_digest = new.payload_digest
          AND retained_payload_digest = new.payload_digest
          AND created_at = new.created_at
    ) THEN RAISE(ABORT, 'payload does not match retained operation intent') END;
END;

CREATE TRIGGER privileged_operation_payloads_reject_update
BEFORE UPDATE ON privileged_operation_payloads BEGIN
    SELECT RAISE(ABORT, 'privileged operation payloads are immutable');
END;

CREATE TRIGGER privileged_operation_attempts_validate_insert
BEFORE INSERT ON privileged_operation_attempts BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM privileged_operations
        WHERE id = new.operation_id
          AND attempt_count = new.sequence
          AND last_attempt_sequence = new.sequence
          AND last_attempt_certainty = new.outcome_certainty
          AND state = 1
          AND new.outcome_certainty = 0
          AND new.started_at <= authority_expires_at
    ) THEN RAISE(ABORT, 'attempt does not match the dispatching operation') END;
    SELECT CASE WHEN new.sequence > 1 AND NOT EXISTS (
        SELECT 1 FROM privileged_operation_attempts
        WHERE operation_id = new.operation_id
          AND sequence = new.sequence - 1
          AND outcome_certainty <> 0
    ) THEN RAISE(ABORT, 'attempt sequence is not contiguous') END;
END;

CREATE TRIGGER privileged_operation_attempts_validate_update
BEFORE UPDATE ON privileged_operation_attempts BEGIN
    SELECT CASE WHEN
        old.outcome_certainty <> 0 OR new.outcome_certainty = 0 OR
        new.operation_id IS NOT old.operation_id OR
        new.sequence IS NOT old.sequence OR
        new.transport_operation_id IS NOT old.transport_operation_id OR
        new.wire_digest IS NOT old.wire_digest OR
        new.broker_boot_id IS NOT old.broker_boot_id OR
        new.guest_boot_id IS NOT old.guest_boot_id OR
        new.started_at IS NOT old.started_at OR
        new.deadline_unix_ms IS NOT old.deadline_unix_ms OR
        NOT EXISTS (
            SELECT 1 FROM privileged_operations
            WHERE id = new.operation_id
              AND attempt_count = new.sequence
              AND last_attempt_sequence = new.sequence
              AND last_attempt_certainty = new.outcome_certainty
        )
    THEN RAISE(ABORT, 'attempt completion is inconsistent or immutable') END;
END;

CREATE TRIGGER privileged_operation_attempts_reject_delete
BEFORE DELETE ON privileged_operation_attempts BEGIN
    SELECT RAISE(ABORT, 'attempt records are immutable');
END;

CREATE TRIGGER privileged_operation_reviews_validate_insert
BEFORE INSERT ON privileged_operation_reviews BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM privileged_operations
        WHERE id = new.operation_id
          AND state = 6
          AND review_disposition = new.disposition
          AND revision = new.operation_revision
          AND updated_at = new.reviewed_at
    ) THEN RAISE(ABORT, 'review does not match the reviewed operation') END;
    SELECT CASE WHEN new.replacement_operation_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM privileged_operations
        WHERE id = new.replacement_operation_id
          AND supersedes_id = new.operation_id
    ) THEN RAISE(ABORT, 'review replacement does not point back to the operation') END;
END;

CREATE TRIGGER privileged_operation_reviews_reject_update
BEFORE UPDATE ON privileged_operation_reviews BEGIN
    SELECT RAISE(ABORT, 'review records are immutable');
END;

CREATE TRIGGER privileged_operation_reviews_reject_delete
BEFORE DELETE ON privileged_operation_reviews BEGIN
    SELECT RAISE(ABORT, 'review records are immutable');
END;
";

const MIGRATION_8: &str = r"
CREATE TABLE desktop_preferences (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    keep_running_in_notification_area INTEGER NOT NULL CHECK (
        keep_running_in_notification_area IN (0, 1)
    ),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
) STRICT;

INSERT INTO desktop_preferences(
    singleton, keep_running_in_notification_area, revision, updated_at
) VALUES (1, 1, 0, 0);

CREATE TABLE desktop_preference_commands (
    scope TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    keep_running_in_notification_area INTEGER NOT NULL CHECK (
        keep_running_in_notification_area IN (0, 1)
    ),
    revision INTEGER NOT NULL CHECK (revision > 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
    PRIMARY KEY(scope, idempotency_key)
) WITHOUT ROWID, STRICT;
";

const MIGRATION_9: &str = r"
CREATE TABLE chat_model_preferences (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    selected_model_id TEXT NOT NULL CHECK (
        length(CAST(selected_model_id AS BLOB)) BETWEEN 1 AND 512
    ),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0)
) STRICT;

INSERT INTO chat_model_preferences(
    singleton, selected_model_id, revision, updated_at
) VALUES (1, 'grok-4.3', 0, 0);

CREATE TABLE chat_model_preference_commands (
    scope TEXT NOT NULL,
    idempotency_key TEXT NOT NULL,
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    selected_model_id TEXT NOT NULL CHECK (
        length(CAST(selected_model_id AS BLOB)) BETWEEN 1 AND 512
    ),
    revision INTEGER NOT NULL CHECK (revision > 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= 0),
    PRIMARY KEY(scope, idempotency_key)
) WITHOUT ROWID, STRICT;
";

// `search_documents` is a derived cache. Earlier schemas could retain rows
// created before canonical workspace tables existed, and a damaged cache could
// carry forged ownership or display fields. Rebuild it transactionally from
// canonical entities so migration is both fail-closed and restartable.
const MIGRATION_10: &str = r"
DELETE FROM search_documents;

INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
SELECT id, id, 'project', name, description, updated_at
FROM projects;

INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
SELECT threads.id, threads.project_id, 'thread', threads.title, '', threads.updated_at
FROM threads
JOIN projects ON projects.id=threads.project_id;

INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
SELECT messages.id, threads.project_id, 'message', threads.title,
       messages.content, messages.updated_at
FROM messages
JOIN threads ON threads.id=messages.thread_id
JOIN projects ON projects.id=threads.project_id
WHERE messages.state=0;

INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
SELECT artifacts.id, artifacts.project_id, 'artifact', artifacts.name,
       artifacts.relative_path, artifacts.updated_at
FROM artifacts
JOIN projects ON projects.id=artifacts.project_id
LEFT JOIN threads
  ON threads.id=artifacts.thread_id AND threads.project_id=artifacts.project_id
WHERE artifacts.state=0
  AND (artifacts.thread_id IS NULL OR threads.id IS NOT NULL);

INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
SELECT automations.id, automations.project_id, 'automation', automations.title,
       automations.prompt, automations.updated_at
FROM automations
JOIN projects ON projects.id=automations.project_id;
";

const MIGRATION_11: &str = r"
CREATE TRIGGER privileged_operation_attempts_validate_epoch_insert
BEFORE INSERT ON privileged_operation_attempts BEGIN
    SELECT CASE WHEN
        new.deadline_unix_ms <= new.started_at OR
        new.broker_boot_id = zeroblob(16) OR
        new.guest_boot_id = zeroblob(16) OR
        new.transport_operation_id = new.operation_id
    THEN RAISE(ABORT, 'invalid privileged attempt epoch evidence') END;
END;
";

// Conversation output is an immutable, turn-owned event journal. The primary
// key is also the paging index; the secondary kind index keeps bounded text
// projection and integrity checks local to one turn.
const MIGRATION_12_TABLE: &str = r"
CREATE TABLE conversation_turn_events (
    turn_id TEXT NOT NULL REFERENCES conversation_turns(id) ON DELETE CASCADE,
    sequence INTEGER NOT NULL CHECK (sequence > 0),
    kind INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 2),
    from_state INTEGER CHECK (from_state BETWEEN 0 AND 5),
    to_state INTEGER CHECK (to_state BETWEEN 0 AND 5),
    start_utf8_offset INTEGER CHECK (
        start_utf8_offset BETWEEN 0 AND 1048576
    ),
    text TEXT CHECK (
        text IS NULL OR length(CAST(text AS BLOB)) BETWEEN 1 AND 16384
    ),
    PRIMARY KEY(turn_id, sequence),
    CHECK (
        (kind = 0 AND sequence = 1
                  AND from_state IS NULL AND to_state IS NULL
                  AND start_utf8_offset IS NULL AND text IS NULL) OR
        (kind = 1 AND from_state IS NOT NULL AND to_state IS NOT NULL
                  AND from_state != to_state
                  AND start_utf8_offset IS NULL AND text IS NULL) OR
        (kind = 2 AND from_state IS NULL AND to_state IS NULL
                  AND start_utf8_offset IS NOT NULL AND text IS NOT NULL)
    )
) WITHOUT ROWID, STRICT;

CREATE INDEX conversation_turn_events_kind_sequence
ON conversation_turn_events(turn_id, kind, sequence);

CREATE TABLE conversation_turn_cancel_commands (
    command_scope TEXT NOT NULL CHECK (
        command_scope IN (
            'cancel_conversation_turn',
            'reconcile_conversation_dispatch_exit'
        )
    ),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
    ),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    turn_id TEXT NOT NULL REFERENCES conversation_turns(id),
    outcome_state INTEGER NOT NULL CHECK (outcome_state IN (2, 3, 4, 5)),
    outcome_revision INTEGER NOT NULL CHECK (
        (outcome_state = 4 AND outcome_revision = 1) OR
        (outcome_state IN (2, 3, 5) AND outcome_revision = 2)
    ),
    PRIMARY KEY(command_scope, idempotency_key)
) WITHOUT ROWID, STRICT;

CREATE INDEX conversation_turn_cancel_commands_turn
ON conversation_turn_cancel_commands(turn_id, command_scope, idempotency_key);
";

const MIGRATION_12_TRIGGERS: &str = r"
CREATE TRIGGER conversation_turn_events_validate_insert
BEFORE INSERT ON conversation_turn_events BEGIN
    SELECT CASE WHEN new.sequence != COALESCE((
        SELECT MAX(sequence) + 1 FROM conversation_turn_events
        WHERE turn_id = new.turn_id
    ), 1) THEN RAISE(ABORT, 'non-contiguous conversation event sequence') END;

    SELECT CASE WHEN new.kind = 0 AND (
        new.sequence != 1 OR
        (SELECT state FROM conversation_turns WHERE id = new.turn_id) != 0
    ) THEN RAISE(ABORT, 'invalid conversation created event') END;

    SELECT CASE WHEN new.kind = 1 AND (
        NOT (
            (new.from_state = 0 AND new.to_state IN (1, 4)) OR
            (new.from_state = 1 AND new.to_state IN (2, 3, 5))
        ) OR
        new.to_state != (
            SELECT state FROM conversation_turns WHERE id = new.turn_id
        )
    ) THEN RAISE(ABORT, 'invalid conversation state event') END;

    SELECT CASE WHEN new.kind = 2 AND (
        (SELECT state FROM conversation_turns WHERE id = new.turn_id) != 1 OR
        (SELECT COUNT(*) FROM conversation_turn_events
         WHERE turn_id = new.turn_id AND kind = 2) >= 4097 OR
        new.start_utf8_offset != COALESCE((
            SELECT SUM(length(CAST(text AS BLOB)))
            FROM conversation_turn_events
            WHERE turn_id = new.turn_id AND kind = 2
        ), 0) OR
        new.start_utf8_offset + length(CAST(new.text AS BLOB)) > 1048576
    ) THEN RAISE(ABORT, 'invalid conversation text event') END;

    SELECT CASE WHEN new.kind = 1 AND new.to_state = 2 AND
        COALESCE((
            SELECT group_concat(text, '') FROM (
                SELECT text FROM conversation_turn_events
                WHERE turn_id = new.turn_id AND kind = 2
                ORDER BY sequence
            )
        ), '') != COALESCE((
            SELECT messages.content
            FROM conversation_turns turns
            JOIN messages ON messages.id = turns.assistant_message_id
            WHERE turns.id = new.turn_id
        ), '')
    THEN RAISE(ABORT, 'completed conversation text mismatch') END;
END;

CREATE TRIGGER conversation_turn_events_immutable_update
BEFORE UPDATE ON conversation_turn_events BEGIN
    SELECT RAISE(ABORT, 'conversation turn events are immutable');
END;

CREATE TRIGGER conversation_turn_events_immutable_delete
BEFORE DELETE ON conversation_turn_events BEGIN
    SELECT RAISE(ABORT, 'conversation turn events are immutable');
END;

CREATE TRIGGER conversation_turn_cancel_commands_validate_insert
BEFORE INSERT ON conversation_turn_cancel_commands BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM conversation_turns turns
        WHERE turns.id = new.turn_id
          AND turns.state = new.outcome_state
          AND turns.revision = new.outcome_revision
          AND turns.state IN (2, 3, 4, 5)
    ) THEN RAISE(ABORT, 'invalid conversation cancellation outcome') END;
END;

CREATE TRIGGER conversation_turn_cancel_commands_immutable_update
BEFORE UPDATE ON conversation_turn_cancel_commands BEGIN
    SELECT RAISE(ABORT, 'conversation cancellation commands are immutable');
END;

CREATE TRIGGER conversation_turn_cancel_commands_immutable_delete
BEFORE DELETE ON conversation_turn_cancel_commands BEGIN
    SELECT RAISE(ABORT, 'conversation cancellation commands are immutable');
END;
";

// Direct Chat lineage and the thread's one-time local credential-generation
// binding remain separate from existing rows so schema-12 databases can be
// upgraded without rebuilding the thread/turn/event foreign-key graph. Source
// 0 is the official xAI API. It is deliberately not an xAI account identity.
// Legacy threads and turns remain readable while unbound, but only an empty
// thread can claim a generation and that binding can never be replaced.
const MIGRATION_13: &str = r"
CREATE TABLE conversation_thread_identity (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id),
    source INTEGER NOT NULL CHECK (source = 0),
    credential_binding_id TEXT CHECK (
        credential_binding_id IS NULL OR (
            length(CAST(credential_binding_id AS BLOB)) BETWEEN 1 AND 128 AND
            credential_binding_id NOT GLOB '*[^A-Za-z0-9_.:-]*'
        )
    )
) WITHOUT ROWID, STRICT;

INSERT INTO conversation_thread_identity(thread_id,source,credential_binding_id)
SELECT id,0,NULL FROM threads;

CREATE TRIGGER conversation_thread_identity_validate_insert
BEFORE INSERT ON conversation_thread_identity BEGIN
    SELECT CASE WHEN new.source != 0 OR new.credential_binding_id IS NOT NULL
    THEN RAISE(ABORT, 'conversation thread identity must start unbound') END;
END;

CREATE TRIGGER threads_create_conversation_thread_identity
AFTER INSERT ON threads BEGIN
    INSERT INTO conversation_thread_identity(thread_id,source,credential_binding_id)
    VALUES (new.id,0,NULL);
END;

CREATE TRIGGER conversation_thread_identity_bind_once
BEFORE UPDATE ON conversation_thread_identity
WHEN NOT (
    new.thread_id = old.thread_id AND
    new.source = old.source AND
    old.credential_binding_id IS NULL AND
    new.credential_binding_id IS NOT NULL AND
    NOT EXISTS (
        SELECT 1 FROM conversation_turns turns
        WHERE turns.thread_id = old.thread_id
    )
) BEGIN
    SELECT RAISE(ABORT, 'conversation thread identity is immutable');
END;

CREATE TRIGGER conversation_thread_identity_immutable_delete
BEFORE DELETE ON conversation_thread_identity BEGIN
    SELECT RAISE(ABORT, 'conversation thread identity is immutable');
END;

CREATE TABLE conversation_turn_lineage (
    turn_id TEXT PRIMARY KEY REFERENCES conversation_turns(id) ON DELETE CASCADE,
    origin INTEGER NOT NULL CHECK (origin IN (0, 1)),
    source_turn_id TEXT REFERENCES conversation_turns(id),
    credential_binding_id TEXT CHECK (
        credential_binding_id IS NULL OR (
            length(CAST(credential_binding_id AS BLOB)) BETWEEN 1 AND 128 AND
            credential_binding_id NOT GLOB '*[^A-Za-z0-9_.:-]*'
        )
    ),
    retry_depth INTEGER NOT NULL CHECK (retry_depth BETWEEN 0 AND 64),
    CHECK (
        (origin = 0 AND source_turn_id IS NULL AND retry_depth = 0) OR
        (origin = 1 AND source_turn_id IS NOT NULL
                    AND source_turn_id != turn_id
                    AND credential_binding_id IS NOT NULL
                    AND retry_depth BETWEEN 1 AND 64)
    )
) WITHOUT ROWID, STRICT;

CREATE UNIQUE INDEX conversation_turn_lineage_one_retry_child
ON conversation_turn_lineage(source_turn_id) WHERE origin = 1;

INSERT INTO conversation_turn_lineage(
    turn_id,origin,source_turn_id,credential_binding_id,retry_depth
)
SELECT id,0,NULL,NULL,0 FROM conversation_turns;

CREATE TRIGGER conversation_turn_lineage_validate_insert
BEFORE INSERT ON conversation_turn_lineage BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM conversation_turns turns
        WHERE turns.id = new.turn_id AND turns.state = 0 AND turns.revision = 0
    ) THEN RAISE(ABORT, 'lineage owner is not a reserved conversation turn') END;

    SELECT CASE WHEN new.origin = 0 AND NOT EXISTS (
        SELECT 1
        FROM conversation_turns turns
        JOIN conversation_thread_identity thread_identity
          ON thread_identity.thread_id = turns.thread_id
        WHERE turns.id = new.turn_id
          AND new.credential_binding_id IS NOT NULL
          AND thread_identity.source = 0
          AND thread_identity.credential_binding_id = new.credential_binding_id
    ) THEN RAISE(ABORT, 'new original conversation lineage requires the thread binding') END;

    SELECT CASE WHEN new.origin = 1 AND NOT EXISTS (
        SELECT 1
        FROM conversation_turns retry
        JOIN messages retry_user ON retry_user.id = retry.user_message_id
        JOIN conversation_turns source ON source.id = new.source_turn_id
        JOIN messages source_user ON source_user.id = source.user_message_id
        JOIN conversation_turn_lineage source_lineage
          ON source_lineage.turn_id = source.id
        JOIN conversation_thread_identity thread_identity
          ON thread_identity.thread_id = source.thread_id
        WHERE retry.id = new.turn_id
          AND retry.project_id = source.project_id
          AND retry.thread_id = source.thread_id
          AND retry.model_id = source.model_id
          AND retry_user.thread_id = source_user.thread_id
          AND retry_user.role = 1
          AND retry_user.state = 0
          AND retry_user.revision = 0
          AND retry_user.content = source_user.content
          AND retry_user.sequence = source_user.sequence + 1
          AND (
              source.state = 4 OR
              (source.state = 3 AND source.failure_retryable = 1)
          )
          AND source_lineage.credential_binding_id IS NOT NULL
          AND source_lineage.credential_binding_id = new.credential_binding_id
          AND thread_identity.source = 0
          AND thread_identity.credential_binding_id = new.credential_binding_id
          AND source_lineage.retry_depth + 1 = new.retry_depth
          AND NOT EXISTS (
              SELECT 1 FROM messages later
              WHERE later.thread_id = source.thread_id
                AND later.sequence > source_user.sequence
                AND later.id != retry_user.id
          )
    ) THEN RAISE(ABORT, 'invalid conversation retry lineage') END;
END;

CREATE TRIGGER conversation_turn_lineage_immutable_update
BEFORE UPDATE ON conversation_turn_lineage BEGIN
    SELECT RAISE(ABORT, 'conversation turn lineage is immutable');
END;

CREATE TRIGGER conversation_turn_lineage_immutable_delete
BEFORE DELETE ON conversation_turn_lineage BEGIN
    SELECT RAISE(ABORT, 'conversation turn lineage is immutable');
END;

CREATE TRIGGER conversation_turn_context_validate_insert
BEFORE INSERT ON conversation_turn_context BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM conversation_turns turns
        WHERE turns.id = new.turn_id AND turns.state = 0 AND turns.revision = 0
    ) THEN RAISE(ABORT, 'conversation context owner is not a new reserved turn') END;
    SELECT CASE WHEN EXISTS (
        SELECT 1 FROM conversation_turn_lineage lineage
        WHERE lineage.turn_id = new.turn_id
    ) THEN RAISE(ABORT, 'conversation context is sealed') END;
END;

CREATE TRIGGER conversation_turn_context_immutable_update
BEFORE UPDATE ON conversation_turn_context BEGIN
    SELECT RAISE(ABORT, 'conversation context is immutable');
END;

CREATE TRIGGER conversation_turn_context_immutable_delete
BEFORE DELETE ON conversation_turn_context BEGIN
    SELECT RAISE(ABORT, 'conversation context is immutable');
END;
";

// Epoch 9 adds immutable child-thread ancestry and copied-message provenance.
// Existing schema-13 thread/message rows remain roots/originals by absence from
// the new side tables. The turn-lineage table is rebuilt transactionally only
// to widen its closed origin set for Edit-and-branch and Regenerate attempts.
const MIGRATION_14: &str = r"
CREATE TABLE conversation_thread_forks (
    child_thread_id TEXT PRIMARY KEY REFERENCES threads(id),
    parent_thread_id TEXT NOT NULL REFERENCES threads(id),
    root_thread_id TEXT NOT NULL REFERENCES threads(id),
    source_turn_id TEXT NOT NULL REFERENCES conversation_turns(id),
    source_message_id TEXT NOT NULL REFERENCES messages(id),
    kind INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 2),
    fork_depth INTEGER NOT NULL CHECK (fork_depth BETWEEN 1 AND 64),
    CHECK (child_thread_id != parent_thread_id),
    CHECK (child_thread_id != root_thread_id)
) WITHOUT ROWID, STRICT;
CREATE INDEX conversation_thread_forks_parent
ON conversation_thread_forks(parent_thread_id, child_thread_id);
CREATE INDEX conversation_thread_forks_root
ON conversation_thread_forks(root_thread_id, fork_depth, child_thread_id);

CREATE TRIGGER conversation_thread_forks_validate_insert
BEFORE INSERT ON conversation_thread_forks BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1
        FROM threads child
        JOIN threads parent ON parent.id = new.parent_thread_id
        JOIN projects project ON project.id = child.project_id
        JOIN conversation_turns source ON source.id = new.source_turn_id
        JOIN messages source_message ON source_message.id = new.source_message_id
        JOIN conversation_thread_identity child_identity
          ON child_identity.thread_id = child.id
        JOIN conversation_thread_identity parent_identity
          ON parent_identity.thread_id = parent.id
        LEFT JOIN conversation_thread_forks parent_fork
          ON parent_fork.child_thread_id = parent.id
        WHERE child.id = new.child_thread_id
          AND child.id != parent.id
          AND child.project_id = parent.project_id
          AND child.project_id = source.project_id
          AND child.title = parent.title
          AND child.state = 0 AND child.revision = 0
          AND child.created_at = child.updated_at
          AND child.created_at >= parent.updated_at
          AND child.created_at >= source.updated_at
          AND project.state = 0
          AND parent.state IN (0, 1)
          AND source.thread_id = parent.id
          AND source_message.thread_id = parent.id
          AND source_message.state = 0
          AND new.root_thread_id = COALESCE(parent_fork.root_thread_id, parent.id)
          AND new.fork_depth = COALESCE(parent_fork.fork_depth, 0) + 1
          AND child_identity.source = 0
          AND parent_identity.source = 0
          AND child_identity.credential_binding_id IS NOT NULL
          AND child_identity.credential_binding_id = parent_identity.credential_binding_id
          AND (
              (new.kind = 0 AND source.state = 2
                  AND source.assistant_message_id = source_message.id
                  AND source_message.role = 2) OR
              (new.kind = 1 AND source.state IN (2, 3, 4)
                  AND source.user_message_id = source_message.id
                  AND source_message.role = 1) OR
              (new.kind = 2 AND source.state = 2
                  AND source.assistant_message_id = source_message.id
                  AND source_message.role = 2)
          )
    ) THEN RAISE(ABORT, 'invalid conversation thread fork') END;
    SELECT CASE WHEN (
        SELECT count(*) FROM conversation_thread_forks
        WHERE parent_thread_id = new.parent_thread_id
    ) >= 64 THEN RAISE(ABORT, 'conversation fork direct-child bound exceeded') END;
    SELECT CASE WHEN 1 + (
        SELECT count(*) FROM conversation_thread_forks
        WHERE root_thread_id = new.root_thread_id
    ) >= 256 THEN RAISE(ABORT, 'conversation fork family bound exceeded') END;
END;

CREATE TRIGGER conversation_thread_forks_immutable_update
BEFORE UPDATE ON conversation_thread_forks BEGIN
    SELECT RAISE(ABORT, 'conversation thread fork lineage is immutable');
END;
CREATE TRIGGER conversation_thread_forks_immutable_delete
BEFORE DELETE ON conversation_thread_forks BEGIN
    SELECT RAISE(ABORT, 'conversation thread fork lineage is immutable');
END;
CREATE TRIGGER conversation_fork_thread_identity_immutable
BEFORE UPDATE OF id, project_id, created_at ON threads
WHEN EXISTS (
    SELECT 1 FROM conversation_thread_forks forks
    WHERE forks.child_thread_id = old.id
) BEGIN
    SELECT RAISE(ABORT, 'conversation fork thread identity is immutable');
END;

CREATE TABLE conversation_message_derivations (
    child_message_id TEXT PRIMARY KEY REFERENCES messages(id),
    source_message_id TEXT NOT NULL REFERENCES messages(id),
    source_turn_id TEXT NOT NULL REFERENCES conversation_turns(id),
    kind INTEGER NOT NULL CHECK (kind BETWEEN 0 AND 2),
    source_context_sequence INTEGER CHECK (
        source_context_sequence IS NULL OR source_context_sequence BETWEEN 1 AND 1000
    ),
    CHECK (child_message_id != source_message_id),
    CHECK (
        (kind IN (0, 2) AND source_context_sequence IS NOT NULL) OR
        (kind = 1 AND source_context_sequence IS NULL)
    )
) WITHOUT ROWID, STRICT;
CREATE INDEX conversation_message_derivations_source
ON conversation_message_derivations(source_message_id, child_message_id);

CREATE TRIGGER conversation_message_derivations_validate_insert
BEFORE INSERT ON conversation_message_derivations BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1
        FROM messages child
        JOIN conversation_thread_forks fork ON fork.child_thread_id = child.thread_id
        JOIN threads child_thread ON child_thread.id = child.thread_id
        JOIN conversation_turns source_turn ON source_turn.id = new.source_turn_id
        JOIN messages source_message ON source_message.id = new.source_message_id
        WHERE child.id = new.child_message_id
          AND child.thread_id = fork.child_thread_id
          AND child.state = 0 AND child.revision = 0
          AND child.created_at = child.updated_at
          AND child.created_at = child_thread.created_at
          AND source_turn.id = fork.source_turn_id
          AND source_turn.thread_id = fork.parent_thread_id
          AND source_message.thread_id = fork.parent_thread_id
          AND source_message.state = 0
          AND (
              (new.kind = 0
                  AND EXISTS (
                      SELECT 1 FROM conversation_turn_context context
                      WHERE context.turn_id = source_turn.id
                        AND context.message_id = source_message.id
                        AND context.role = source_message.role
                        AND context.content = source_message.content
                        AND context.revision = source_message.revision
                        AND context.created_at = source_message.created_at
                        AND context.updated_at = source_message.updated_at
                        AND new.source_context_sequence = (
                            SELECT count(*)
                            FROM conversation_turn_context ordinal
                            WHERE ordinal.turn_id = context.turn_id
                              AND ordinal.sequence <= context.sequence
                        )
                  )
                  AND child.sequence = new.source_context_sequence
                  AND child.role = source_message.role
                  AND child.content = source_message.content) OR
              (new.kind = 1
                  AND fork.kind = 0
                  AND source_turn.state = 2
                  AND source_turn.assistant_message_id = source_message.id
                  AND child.role = 2
                  AND child.content = source_message.content
                  AND child.sequence = 1 + (
                      SELECT count(*) FROM conversation_turn_context context
                      WHERE context.turn_id = source_turn.id
                  )) OR
              (new.kind = 2
                  AND fork.kind = 1
                  AND source_turn.user_message_id = source_message.id
                  AND child.role = 1
                  AND child.content != source_message.content
                  AND child.sequence = new.source_context_sequence
                  AND new.source_context_sequence = (
                      SELECT count(*) FROM conversation_turn_context context
                      WHERE context.turn_id = source_turn.id
                  ))
          )
    ) THEN RAISE(ABORT, 'invalid conversation message derivation') END;
END;

CREATE TRIGGER conversation_message_derivations_immutable_update
BEFORE UPDATE ON conversation_message_derivations BEGIN
    SELECT RAISE(ABORT, 'conversation message derivation is immutable');
END;
CREATE TRIGGER conversation_message_derivations_immutable_delete
BEFORE DELETE ON conversation_message_derivations BEGIN
    SELECT RAISE(ABORT, 'conversation message derivation is immutable');
END;
CREATE TRIGGER conversation_derived_messages_immutable_update
BEFORE UPDATE ON messages
WHEN EXISTS (
    SELECT 1 FROM conversation_message_derivations derivation
    WHERE derivation.child_message_id = old.id
) BEGIN
    SELECT RAISE(ABORT, 'derived conversation messages are immutable');
END;
CREATE TRIGGER conversation_derived_messages_immutable_delete
BEFORE DELETE ON messages
WHEN EXISTS (
    SELECT 1 FROM conversation_message_derivations derivation
    WHERE derivation.child_message_id = old.id
) BEGIN
    SELECT RAISE(ABORT, 'derived conversation messages are immutable');
END;

CREATE TABLE conversation_inherited_assistant_outcomes (
    child_assistant_message_id TEXT PRIMARY KEY REFERENCES messages(id),
    source_turn_id TEXT NOT NULL REFERENCES conversation_turns(id)
) WITHOUT ROWID, STRICT;
CREATE INDEX conversation_inherited_assistant_outcomes_source
ON conversation_inherited_assistant_outcomes(source_turn_id, child_assistant_message_id);

CREATE TRIGGER conversation_inherited_assistant_outcomes_validate_insert
BEFORE INSERT ON conversation_inherited_assistant_outcomes BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1
        FROM messages child
        JOIN conversation_message_derivations derivation
          ON derivation.child_message_id = child.id
        JOIN conversation_turns source ON source.id = new.source_turn_id
        JOIN messages source_assistant ON source_assistant.id = source.assistant_message_id
        WHERE child.id = new.child_assistant_message_id
          AND child.role = 2 AND child.state = 0 AND child.revision = 0
          AND source.state = 2
          AND source_assistant.role = 2 AND source_assistant.state = 0
          AND source_assistant.content = child.content
          AND (
              (derivation.kind = 1
                  AND derivation.source_turn_id = source.id
                  AND derivation.source_message_id = source_assistant.id) OR
              (derivation.kind = 0
                  AND EXISTS (
                      SELECT 1 FROM messages parent_assistant
                      WHERE parent_assistant.id = derivation.source_message_id
                        AND parent_assistant.role = 2
                        AND parent_assistant.state = 0
                        AND (
                            parent_assistant.id = source_assistant.id OR
                            EXISTS (
                                SELECT 1
                                FROM conversation_inherited_assistant_outcomes inherited
                                WHERE inherited.child_assistant_message_id = parent_assistant.id
                                  AND inherited.source_turn_id = source.id
                            )
                        )
                  ))
          )
    ) THEN RAISE(ABORT, 'invalid inherited conversation assistant outcome') END;
END;
CREATE TRIGGER conversation_inherited_assistant_outcomes_immutable_update
BEFORE UPDATE ON conversation_inherited_assistant_outcomes BEGIN
    SELECT RAISE(ABORT, 'inherited conversation assistant outcomes are immutable');
END;
CREATE TRIGGER conversation_inherited_assistant_outcomes_immutable_delete
BEFORE DELETE ON conversation_inherited_assistant_outcomes BEGIN
    SELECT RAISE(ABORT, 'inherited conversation assistant outcomes are immutable');
END;

DROP TRIGGER conversation_turn_context_validate_insert;
DROP TRIGGER conversation_turn_context_immutable_update;
DROP TRIGGER conversation_turn_context_immutable_delete;
DROP TRIGGER conversation_turn_lineage_validate_insert;
DROP TRIGGER conversation_turn_lineage_immutable_update;
DROP TRIGGER conversation_turn_lineage_immutable_delete;
DROP INDEX conversation_turn_lineage_one_retry_child;
ALTER TABLE conversation_turn_lineage RENAME TO conversation_turn_lineage_v13;

CREATE TABLE conversation_turn_lineage (
    turn_id TEXT PRIMARY KEY REFERENCES conversation_turns(id) ON DELETE CASCADE,
    origin INTEGER NOT NULL CHECK (origin BETWEEN 0 AND 3),
    source_turn_id TEXT REFERENCES conversation_turns(id),
    credential_binding_id TEXT CHECK (
        credential_binding_id IS NULL OR (
            length(CAST(credential_binding_id AS BLOB)) BETWEEN 1 AND 128 AND
            credential_binding_id NOT GLOB '*[^A-Za-z0-9_.:-]*'
        )
    ),
    retry_depth INTEGER NOT NULL CHECK (retry_depth BETWEEN 0 AND 64),
    CHECK (
        (origin = 0 AND source_turn_id IS NULL AND retry_depth = 0) OR
        (origin = 1 AND source_turn_id IS NOT NULL
                    AND source_turn_id != turn_id
                    AND credential_binding_id IS NOT NULL
                    AND retry_depth BETWEEN 1 AND 64) OR
        (origin IN (2, 3) AND source_turn_id IS NOT NULL
                           AND source_turn_id != turn_id
                           AND credential_binding_id IS NOT NULL
                           AND retry_depth = 0)
    )
) WITHOUT ROWID, STRICT;
INSERT INTO conversation_turn_lineage(
    turn_id,origin,source_turn_id,credential_binding_id,retry_depth
)
SELECT turn_id,origin,source_turn_id,credential_binding_id,retry_depth
FROM conversation_turn_lineage_v13;
DROP TABLE conversation_turn_lineage_v13;
CREATE UNIQUE INDEX conversation_turn_lineage_one_retry_child
ON conversation_turn_lineage(source_turn_id) WHERE origin = 1;

CREATE TRIGGER conversation_turn_lineage_validate_insert
BEFORE INSERT ON conversation_turn_lineage BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM conversation_turns turns
        WHERE turns.id = new.turn_id AND turns.state = 0 AND turns.revision = 0
    ) THEN RAISE(ABORT, 'lineage owner is not a reserved conversation turn') END;

    SELECT CASE WHEN new.origin = 0 AND NOT EXISTS (
        SELECT 1
        FROM conversation_turns turns
        JOIN conversation_thread_identity thread_identity
          ON thread_identity.thread_id = turns.thread_id
        WHERE turns.id = new.turn_id
          AND new.credential_binding_id IS NOT NULL
          AND thread_identity.source = 0
          AND thread_identity.credential_binding_id = new.credential_binding_id
    ) THEN RAISE(ABORT, 'new original conversation lineage requires the thread binding') END;

    SELECT CASE WHEN new.origin = 1 AND NOT EXISTS (
        SELECT 1
        FROM conversation_turns retry
        JOIN messages retry_user ON retry_user.id = retry.user_message_id
        JOIN conversation_turns source ON source.id = new.source_turn_id
        JOIN messages source_user ON source_user.id = source.user_message_id
        JOIN conversation_turn_lineage source_lineage
          ON source_lineage.turn_id = source.id
        JOIN conversation_thread_identity thread_identity
          ON thread_identity.thread_id = source.thread_id
        WHERE retry.id = new.turn_id
          AND retry.project_id = source.project_id
          AND retry.thread_id = source.thread_id
          AND retry.model_id = source.model_id
          AND retry_user.thread_id = source_user.thread_id
          AND retry_user.role = 1
          AND retry_user.state = 0
          AND retry_user.revision = 0
          AND retry_user.content = source_user.content
          AND retry_user.sequence = source_user.sequence + 1
          AND (
              source.state = 4 OR
              (source.state = 3 AND source.failure_retryable = 1)
          )
          AND source_lineage.credential_binding_id IS NOT NULL
          AND source_lineage.credential_binding_id = new.credential_binding_id
          AND thread_identity.source = 0
          AND thread_identity.credential_binding_id = new.credential_binding_id
          AND source_lineage.retry_depth + 1 = new.retry_depth
          AND NOT EXISTS (
              SELECT 1 FROM messages later
              WHERE later.thread_id = source.thread_id
                AND later.sequence > source_user.sequence
                AND later.id != retry_user.id
          )
    ) THEN RAISE(ABORT, 'invalid conversation retry lineage') END;

    SELECT CASE WHEN new.origin IN (2, 3) AND NOT EXISTS (
        SELECT 1
        FROM conversation_turns child_turn
        JOIN conversation_thread_forks fork
          ON fork.child_thread_id = child_turn.thread_id
        JOIN conversation_turns source ON source.id = new.source_turn_id
        JOIN messages child_user ON child_user.id = child_turn.user_message_id
        JOIN conversation_message_derivations child_derivation
          ON child_derivation.child_message_id = child_user.id
        JOIN conversation_thread_identity child_identity
          ON child_identity.thread_id = child_turn.thread_id
        JOIN conversation_thread_identity parent_identity
          ON parent_identity.thread_id = source.thread_id
        WHERE child_turn.id = new.turn_id
          AND fork.source_turn_id = source.id
          AND child_turn.project_id = source.project_id
          AND source.thread_id = fork.parent_thread_id
          AND child_turn.model_id = source.model_id
          AND child_turn.request_fingerprint IS NOT NULL
          AND child_user.thread_id = child_turn.thread_id
          AND child_user.role = 1 AND child_user.state = 0 AND child_user.revision = 0
          AND child_derivation.source_turn_id = source.id
          AND ((new.origin = 2 AND fork.kind = 1 AND child_derivation.kind = 2) OR
               (new.origin = 3 AND fork.kind = 2 AND child_derivation.kind = 0))
          AND child_identity.source = 0 AND parent_identity.source = 0
          AND child_identity.credential_binding_id = new.credential_binding_id
          AND parent_identity.credential_binding_id = new.credential_binding_id
          AND (SELECT count(*) FROM conversation_turn_context context
               WHERE context.turn_id = child_turn.id) =
              (SELECT count(*) FROM messages child_message
               WHERE child_message.thread_id = child_turn.thread_id)
          AND NOT EXISTS (
              SELECT 1
              FROM conversation_turn_context context
              LEFT JOIN messages child_message
                ON child_message.id = context.message_id
               AND child_message.thread_id = child_turn.thread_id
               AND child_message.sequence = context.sequence
               AND child_message.role = context.role
               AND child_message.content = context.content
               AND child_message.revision = context.revision
               AND child_message.created_at = context.created_at
               AND child_message.updated_at = context.updated_at
              WHERE context.turn_id = child_turn.id AND child_message.id IS NULL
          )
    ) THEN RAISE(ABORT, 'invalid conversation fork turn lineage') END;
END;

CREATE TRIGGER conversation_turn_lineage_immutable_update
BEFORE UPDATE ON conversation_turn_lineage BEGIN
    SELECT RAISE(ABORT, 'conversation turn lineage is immutable');
END;
CREATE TRIGGER conversation_turn_lineage_immutable_delete
BEFORE DELETE ON conversation_turn_lineage BEGIN
    SELECT RAISE(ABORT, 'conversation turn lineage is immutable');
END;
CREATE TRIGGER conversation_turn_context_validate_insert
BEFORE INSERT ON conversation_turn_context BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM conversation_turns turns
        WHERE turns.id = new.turn_id AND turns.state = 0 AND turns.revision = 0
    ) THEN RAISE(ABORT, 'conversation context owner is not a new reserved turn') END;
    SELECT CASE WHEN EXISTS (
        SELECT 1 FROM conversation_turn_lineage lineage
        WHERE lineage.turn_id = new.turn_id
    ) THEN RAISE(ABORT, 'conversation context is sealed') END;
END;
CREATE TRIGGER conversation_turn_context_immutable_update
BEFORE UPDATE ON conversation_turn_context BEGIN
    SELECT RAISE(ABORT, 'conversation context is immutable');
END;
CREATE TRIGGER conversation_turn_context_immutable_delete
BEFORE DELETE ON conversation_turn_context BEGIN
    SELECT RAISE(ABORT, 'conversation context is immutable');
END;

CREATE TABLE conversation_fork_commands (
    command_scope TEXT NOT NULL CHECK (command_scope IN (
        'branch_conversation_thread',
        'edit_and_branch_conversation_turn',
        'regenerate_conversation_turn'
    )),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
    ),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    source_turn_id TEXT NOT NULL REFERENCES conversation_turns(id),
    expected_source_revision INTEGER NOT NULL CHECK (expected_source_revision >= 0),
    child_thread_id TEXT NOT NULL UNIQUE REFERENCES conversation_thread_forks(child_thread_id),
    started_turn_id TEXT UNIQUE REFERENCES conversation_turns(id),
    PRIMARY KEY(command_scope, idempotency_key)
) WITHOUT ROWID, STRICT;

CREATE TRIGGER conversation_fork_commands_validate_insert
BEFORE INSERT ON conversation_fork_commands BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1
        FROM conversation_thread_forks fork
        JOIN conversation_turns source ON source.id = new.source_turn_id
        WHERE fork.child_thread_id = new.child_thread_id
          AND fork.source_turn_id = source.id
          AND source.revision = new.expected_source_revision
          AND ((fork.kind = 0 AND new.command_scope = 'branch_conversation_thread'
                              AND new.started_turn_id IS NULL) OR
               (fork.kind = 1 AND new.command_scope = 'edit_and_branch_conversation_turn'
                              AND new.started_turn_id IS NOT NULL) OR
               (fork.kind = 2 AND new.command_scope = 'regenerate_conversation_turn'
                              AND new.started_turn_id IS NOT NULL))
          AND (new.started_turn_id IS NULL OR EXISTS (
              SELECT 1
              FROM conversation_turns started
              JOIN conversation_turn_lineage lineage ON lineage.turn_id = started.id
              WHERE started.id = new.started_turn_id
                AND started.thread_id = fork.child_thread_id
                AND started.idempotency_key = new.idempotency_key
                AND started.request_fingerprint = new.request_fingerprint
                AND started.state = 0 AND started.revision = 0
                AND ((fork.kind = 1 AND lineage.origin = 2) OR
                     (fork.kind = 2 AND lineage.origin = 3))
          ))
          AND (SELECT count(*) FROM messages child_message
               WHERE child_message.thread_id = fork.child_thread_id) =
              (SELECT count(*) FROM conversation_message_derivations derivation
               JOIN messages child_message ON child_message.id = derivation.child_message_id
               WHERE child_message.thread_id = fork.child_thread_id)
          AND NOT EXISTS (
              SELECT 1
              FROM conversation_message_derivations derivation
              JOIN messages child_message ON child_message.id = derivation.child_message_id
              LEFT JOIN conversation_inherited_assistant_outcomes outcome
                ON outcome.child_assistant_message_id = child_message.id
              WHERE child_message.thread_id = fork.child_thread_id
                AND child_message.role = 2
                AND outcome.child_assistant_message_id IS NULL
          )
    ) THEN RAISE(ABORT, 'invalid conversation fork command') END;
END;
CREATE TRIGGER conversation_fork_commands_immutable_update
BEFORE UPDATE ON conversation_fork_commands BEGIN
    SELECT RAISE(ABORT, 'conversation fork commands are immutable');
END;
CREATE TRIGGER conversation_fork_commands_immutable_delete
BEFORE DELETE ON conversation_fork_commands BEGIN
    SELECT RAISE(ABORT, 'conversation fork commands are immutable');
END;
";

// Epoch 10 closes the renderer-crash ambiguity around fork responses. A fork
// command owns one mutable delivery projection, while every reconciled request
// key is retained as an immutable alias to that canonical result. Only pending
// deliveries participate in request coalescing, so acknowledgement releases a
// request fingerprint for a deliberate subsequent fork. Existing epoch-9
// commands predate the handshake and are conservatively backfilled as already
// acknowledged.
const MIGRATION_15: &str = r"
CREATE TABLE conversation_fork_deliveries (
    child_thread_id TEXT PRIMARY KEY
        REFERENCES conversation_fork_commands(child_thread_id),
    command_scope TEXT NOT NULL CHECK (command_scope IN (
        'branch_conversation_thread',
        'edit_and_branch_conversation_turn',
        'regenerate_conversation_turn'
    )),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 1),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 0 AND 1),
    CHECK ((state = 0 AND revision = 0) OR (state = 1 AND revision = 1))
) WITHOUT ROWID, STRICT;

CREATE UNIQUE INDEX conversation_fork_deliveries_one_pending_request
ON conversation_fork_deliveries(command_scope, request_fingerprint)
WHERE state = 0;

CREATE TABLE conversation_fork_delivery_aliases (
    command_scope TEXT NOT NULL CHECK (command_scope IN (
        'branch_conversation_thread',
        'edit_and_branch_conversation_turn',
        'regenerate_conversation_turn'
    )),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
    ),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    child_thread_id TEXT NOT NULL REFERENCES conversation_fork_deliveries(child_thread_id),
    PRIMARY KEY(command_scope, idempotency_key)
) WITHOUT ROWID, STRICT;
CREATE INDEX conversation_fork_delivery_aliases_child
ON conversation_fork_delivery_aliases(child_thread_id, command_scope, idempotency_key);

CREATE TABLE conversation_fork_delivery_ack_commands (
    command_scope TEXT NOT NULL CHECK (
        command_scope = 'acknowledge_conversation_fork_delivery'
    ),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
    ),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    child_thread_id TEXT NOT NULL UNIQUE REFERENCES conversation_fork_deliveries(child_thread_id),
    expected_delivery_revision INTEGER NOT NULL CHECK (expected_delivery_revision = 0),
    resulting_delivery_revision INTEGER NOT NULL CHECK (resulting_delivery_revision = 1),
    PRIMARY KEY(command_scope, idempotency_key)
) WITHOUT ROWID, STRICT;
CREATE INDEX conversation_fork_delivery_ack_commands_child
ON conversation_fork_delivery_ack_commands(child_thread_id, command_scope, idempotency_key);

INSERT INTO conversation_fork_deliveries(
    child_thread_id,command_scope,request_fingerprint,state,revision
)
SELECT child_thread_id,command_scope,request_fingerprint,1,1
FROM conversation_fork_commands;

CREATE TRIGGER conversation_fork_deliveries_validate_insert
BEFORE INSERT ON conversation_fork_deliveries BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM conversation_fork_commands command
        WHERE command.child_thread_id = new.child_thread_id
          AND command.command_scope = new.command_scope
          AND command.request_fingerprint = new.request_fingerprint
    ) THEN RAISE(ABORT, 'invalid conversation fork delivery') END;
END;

CREATE TRIGGER conversation_fork_deliveries_validate_update
BEFORE UPDATE ON conversation_fork_deliveries BEGIN
    SELECT CASE WHEN new.child_thread_id != old.child_thread_id
          OR new.command_scope != old.command_scope
          OR new.request_fingerprint != old.request_fingerprint
          OR old.state != 0 OR old.revision != 0
          OR new.state != 1 OR new.revision != 1
        THEN RAISE(ABORT, 'invalid conversation fork delivery transition') END;
END;
CREATE TRIGGER conversation_fork_deliveries_immutable_delete
BEFORE DELETE ON conversation_fork_deliveries BEGIN
    SELECT RAISE(ABORT, 'conversation fork deliveries cannot be deleted');
END;

CREATE TRIGGER conversation_fork_delivery_aliases_validate_insert
BEFORE INSERT ON conversation_fork_delivery_aliases BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM conversation_fork_deliveries delivery
        WHERE delivery.child_thread_id = new.child_thread_id
          AND delivery.command_scope = new.command_scope
          AND delivery.request_fingerprint = new.request_fingerprint
    ) THEN RAISE(ABORT, 'invalid conversation fork delivery alias') END;
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM conversation_fork_deliveries delivery
        WHERE delivery.child_thread_id = new.child_thread_id
          AND delivery.state = 0 AND delivery.revision = 0
    ) THEN RAISE(ABORT, 'conversation fork delivery is not pending') END;
    SELECT CASE WHEN EXISTS (
        SELECT 1 FROM conversation_fork_commands command
        WHERE command.command_scope = new.command_scope
          AND command.idempotency_key = new.idempotency_key
    ) THEN RAISE(ABORT, 'conversation fork delivery key collides with a command') END;
    SELECT CASE WHEN (
        SELECT count(*) FROM conversation_fork_delivery_aliases alias
        WHERE alias.child_thread_id = new.child_thread_id
    ) >= 64 THEN RAISE(ABORT, 'conversation fork delivery request bound exceeded') END;
END;
CREATE TRIGGER conversation_fork_delivery_aliases_immutable_update
BEFORE UPDATE ON conversation_fork_delivery_aliases BEGIN
    SELECT RAISE(ABORT, 'conversation fork delivery aliases are immutable');
END;
CREATE TRIGGER conversation_fork_delivery_aliases_immutable_delete
BEFORE DELETE ON conversation_fork_delivery_aliases BEGIN
    SELECT RAISE(ABORT, 'conversation fork delivery aliases are immutable');
END;

CREATE TRIGGER conversation_fork_commands_reject_delivery_alias_key
BEFORE INSERT ON conversation_fork_commands
WHEN EXISTS (
    SELECT 1 FROM conversation_fork_delivery_aliases alias
    WHERE alias.command_scope = new.command_scope
      AND alias.idempotency_key = new.idempotency_key
) BEGIN
    SELECT RAISE(ABORT, 'conversation fork command key collides with a delivery alias');
END;

CREATE TRIGGER conversation_fork_delivery_ack_commands_validate_insert
BEFORE INSERT ON conversation_fork_delivery_ack_commands BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM conversation_fork_deliveries delivery
        WHERE delivery.child_thread_id = new.child_thread_id
          AND delivery.state = 1
          AND delivery.revision = new.resulting_delivery_revision
          AND new.expected_delivery_revision + 1 = delivery.revision
    ) THEN RAISE(ABORT, 'invalid conversation fork delivery acknowledgement') END;
END;
CREATE TRIGGER conversation_fork_delivery_ack_commands_immutable_update
BEFORE UPDATE ON conversation_fork_delivery_ack_commands BEGIN
    SELECT RAISE(ABORT, 'conversation fork delivery acknowledgement commands are immutable');
END;
CREATE TRIGGER conversation_fork_delivery_ack_commands_immutable_delete
BEFORE DELETE ON conversation_fork_delivery_ack_commands BEGIN
    SELECT RAISE(ABORT, 'conversation fork delivery acknowledgement commands are immutable');
END;

CREATE TRIGGER conversation_fork_commands_create_pending_delivery
AFTER INSERT ON conversation_fork_commands BEGIN
    INSERT INTO conversation_fork_deliveries(
        child_thread_id,command_scope,request_fingerprint,state,revision
    ) VALUES (
        new.child_thread_id,new.command_scope,new.request_fingerprint,0,0
    );
END;
";

// Epoch 13 removes daemon-owned storage paths from the public artifact
// projection. Artifact search is metadata-only as well: names remain indexed,
// but the derived cache must neither disclose nor act as a query oracle for a
// relative storage path retained by schema-15 rows.
const MIGRATION_16: &str = r"
DROP TRIGGER artifacts_search_ai;
DROP TRIGGER artifacts_search_au;

CREATE TRIGGER artifacts_search_ai AFTER INSERT ON artifacts WHEN new.state=0 BEGIN
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    VALUES (new.id, new.project_id, 'artifact', new.name, '', new.updated_at);
END;
CREATE TRIGGER artifacts_search_au AFTER UPDATE ON artifacts BEGIN
    DELETE FROM search_documents WHERE id=new.id;
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    SELECT new.id, new.project_id, 'artifact', new.name, '', new.updated_at
    WHERE new.state=0;
END;

DELETE FROM search_documents WHERE kind='artifact';
INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
SELECT artifacts.id, artifacts.project_id, 'artifact', artifacts.name, '', artifacts.updated_at
FROM artifacts
JOIN projects ON projects.id=artifacts.project_id
LEFT JOIN threads
  ON threads.id=artifacts.thread_id AND threads.project_id=artifacts.project_id
WHERE artifacts.state=0
  AND (artifacts.thread_id IS NULL OR threads.id IS NOT NULL);

INSERT INTO search_documents_fts(search_documents_fts) VALUES ('rebuild');
";

// Epoch 14 replaces unqualified artifact path metadata with immutable,
// digest-bound content versions. Schema-16 `Available` rows never proved that
// their relative path named a stable object, so they migrate to `Unavailable`
// without carrying path, media type, or byte count forward. Only the typed
// ingestion journal can create an `Available` row from this schema onward.
const MIGRATION_17: &str = r"
DROP TRIGGER artifacts_search_ai;
DROP TRIGGER artifacts_search_au;
DROP INDEX artifacts_project_recent;
DELETE FROM search_documents WHERE kind='artifact';

ALTER TABLE artifacts RENAME TO artifacts_v16;

CREATE TABLE artifacts (
    id TEXT PRIMARY KEY CHECK (length(CAST(id AS BLOB)) BETWEEN 1 AND 128),
    project_id TEXT NOT NULL REFERENCES projects(id) CHECK (
        length(CAST(project_id AS BLOB)) BETWEEN 1 AND 128
    ),
    thread_id TEXT CHECK (
        thread_id IS NULL OR length(CAST(thread_id AS BLOB)) BETWEEN 1 AND 128
    ),
    name TEXT NOT NULL CHECK (length(CAST(name AS BLOB)) BETWEEN 1 AND 200),
    current_content_version INTEGER CHECK (
        current_content_version BETWEEN 1 AND 1000000
    ),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 2),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    FOREIGN KEY(thread_id, project_id) REFERENCES threads(id, project_id),
    FOREIGN KEY(id, current_content_version)
        REFERENCES artifact_versions(artifact_id, version)
        DEFERRABLE INITIALLY DEFERRED,
    CHECK (
        (state = 0 AND current_content_version IS NULL) OR
        (state = 1 AND current_content_version IS NOT NULL) OR
        (state = 2 AND current_content_version IS NULL)
    ),
    CHECK (state != 2 OR revision > 0),
    CHECK (state != 1 OR revision = current_content_version),
    CHECK (revision != 0 OR updated_at = created_at)
) STRICT;
CREATE INDEX artifacts_project_recent
ON artifacts(project_id, updated_at DESC, id);

CREATE TABLE artifact_versions (
    artifact_id TEXT NOT NULL,
    version INTEGER NOT NULL CHECK (version BETWEEN 1 AND 1000000),
    content_sha256 BLOB NOT NULL CHECK (length(content_sha256) = 32),
    media_type TEXT NOT NULL CHECK (
        length(CAST(media_type AS BLOB)) BETWEEN 1 AND 255
    ),
    byte_size INTEGER NOT NULL CHECK (byte_size BETWEEN 0 AND 67108864),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    PRIMARY KEY(artifact_id, version),
    FOREIGN KEY(artifact_id) REFERENCES artifacts(id)
        DEFERRABLE INITIALLY DEFERRED
) WITHOUT ROWID, STRICT;

CREATE TRIGGER artifact_versions_immutable_update
BEFORE UPDATE ON artifact_versions BEGIN
    SELECT RAISE(ABORT, 'artifact versions are immutable');
END;
CREATE TRIGGER artifact_versions_immutable_delete
BEFORE DELETE ON artifact_versions BEGIN
    SELECT RAISE(ABORT, 'artifact versions are immutable');
END;

INSERT INTO artifacts(
    id,project_id,thread_id,name,current_content_version,state,revision,created_at,updated_at
)
SELECT id,project_id,thread_id,name,NULL,
       CASE state WHEN 0 THEN 0 WHEN 1 THEN 2 END,
       CASE state WHEN 0 THEN 0 ELSE revision END,
       created_at,
       CASE state WHEN 0 THEN created_at ELSE updated_at END
FROM artifacts_v16;

DROP TABLE artifacts_v16;

DELETE FROM workspace_commands
WHERE scope IN ('create_artifact', 'update_artifact', 'delete_artifact');

CREATE TABLE artifact_ingestions (
    command_scope TEXT NOT NULL CHECK (command_scope = 'import_artifact'),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
    ),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    artifact_id TEXT NOT NULL UNIQUE CHECK (
        length(CAST(artifact_id AS BLOB)) BETWEEN 1 AND 128
    ),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 3),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 0 AND 2),
    content_sha256 BLOB CHECK (
        content_sha256 IS NULL OR length(content_sha256) = 32
    ),
    content_media_type TEXT CHECK (
        content_media_type IS NULL OR
        length(CAST(content_media_type AS BLOB)) BETWEEN 1 AND 255
    ),
    content_byte_size INTEGER CHECK (
        content_byte_size IS NULL OR content_byte_size BETWEEN 0 AND 67108864
    ),
    content_created_at INTEGER CHECK (
        content_created_at IS NULL OR content_created_at >= 0
    ),
    active_slot INTEGER UNIQUE CHECK (active_slot IS NULL OR active_slot = 1),
    failure_code TEXT CHECK (
        failure_code IS NULL OR
        length(CAST(failure_code AS BLOB)) BETWEEN 1 AND 64
    ),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    PRIMARY KEY(command_scope, idempotency_key),
    FOREIGN KEY(artifact_id) REFERENCES artifacts(id),
    CHECK (
        (content_sha256 IS NULL) = (content_media_type IS NULL) AND
        (content_sha256 IS NULL) = (content_byte_size IS NULL) AND
        (content_sha256 IS NULL) = (content_created_at IS NULL)
    ),
    CHECK (
        (state = 0 AND revision = 0 AND content_sha256 IS NULL AND active_slot = 1
                   AND failure_code IS NULL) OR
        (state = 1 AND revision = 1 AND content_sha256 IS NOT NULL AND active_slot = 1
                   AND failure_code IS NULL) OR
        (state = 2 AND revision = 2 AND content_sha256 IS NOT NULL AND active_slot IS NULL
                   AND failure_code IS NULL) OR
        (state = 3 AND active_slot IS NULL AND failure_code IS NOT NULL AND (
            (revision = 1 AND content_sha256 IS NULL) OR
            (revision = 2 AND content_sha256 IS NOT NULL)
        ))
    )
) WITHOUT ROWID, STRICT;
CREATE INDEX artifact_ingestions_recovery
ON artifact_ingestions(state, created_at, artifact_id)
WHERE state IN (0, 1);

CREATE TRIGGER artifact_ingestions_validate_update
BEFORE UPDATE ON artifact_ingestions BEGIN
    SELECT CASE WHEN
        new.command_scope != old.command_scope OR
        new.idempotency_key != old.idempotency_key OR
        new.request_fingerprint != old.request_fingerprint OR
        new.artifact_id != old.artifact_id OR
        new.created_at != old.created_at OR
        new.revision != old.revision + 1 OR
        new.updated_at < old.updated_at OR
        NOT (
            (old.state = 0 AND new.state = 1
                           AND old.content_sha256 IS NULL
                           AND new.content_sha256 IS NOT NULL) OR
            (old.state = 0 AND new.state = 3
                           AND new.content_sha256 IS NULL) OR
            (old.state = 1 AND new.state IN (2, 3)
                           AND new.content_sha256 = old.content_sha256
                           AND new.content_media_type = old.content_media_type
                           AND new.content_byte_size = old.content_byte_size
                           AND new.content_created_at = old.content_created_at)
        )
    THEN RAISE(ABORT, 'invalid artifact ingestion transition') END;
END;
CREATE TRIGGER artifact_ingestions_validate_commit
BEFORE UPDATE OF state ON artifact_ingestions WHEN new.state = 2 BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1
        FROM artifacts artifact
        JOIN artifact_versions version
          ON version.artifact_id = artifact.id
         AND version.version = artifact.current_content_version
        WHERE artifact.id = new.artifact_id
          AND artifact.state = 1
          AND version.version = 1
          AND version.content_sha256 = new.content_sha256
          AND version.media_type = new.content_media_type
          AND version.byte_size = new.content_byte_size
          AND version.created_at = new.content_created_at
    ) THEN RAISE(ABORT, 'artifact ingestion commit is incomplete') END;
END;
CREATE TRIGGER artifact_ingestions_immutable_delete
BEFORE DELETE ON artifact_ingestions BEGIN
    SELECT RAISE(ABORT, 'artifact ingestion journal is immutable');
END;

CREATE TABLE artifact_open_commands (
    command_scope TEXT NOT NULL CHECK (command_scope = 'open_artifact'),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
    ),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    artifact_id TEXT NOT NULL,
    content_version INTEGER NOT NULL CHECK (content_version BETWEEN 1 AND 1000000),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 4),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 0 AND 2),
    active_slot INTEGER UNIQUE CHECK (active_slot IS NULL OR active_slot = 1),
    failure_code TEXT CHECK (
        failure_code IS NULL OR
        length(CAST(failure_code AS BLOB)) BETWEEN 1 AND 64
    ),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    PRIMARY KEY(command_scope, idempotency_key),
    FOREIGN KEY(artifact_id, content_version)
        REFERENCES artifact_versions(artifact_id, version),
    CHECK (
        (state = 0 AND revision = 0 AND active_slot = 1 AND failure_code IS NULL) OR
        (state = 1 AND revision = 1 AND active_slot = 1 AND failure_code IS NULL) OR
        (state = 2 AND revision = 2 AND active_slot IS NULL AND failure_code IS NULL) OR
        (state = 3 AND revision IN (1, 2) AND active_slot IS NULL
                   AND failure_code IS NOT NULL) OR
        (state = 4 AND revision = 2 AND active_slot IS NULL AND failure_code IS NULL)
    )
) WITHOUT ROWID, STRICT;
CREATE INDEX artifact_open_commands_recovery
ON artifact_open_commands(state, created_at, artifact_id)
WHERE state IN (0, 1);

CREATE TRIGGER artifact_open_commands_validate_insert
BEFORE INSERT ON artifact_open_commands BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM artifacts
        WHERE id = new.artifact_id
          AND state = 1
          AND current_content_version = new.content_version
    ) THEN RAISE(ABORT, 'artifact content is not currently available') END;
END;
CREATE TRIGGER artifact_open_commands_validate_update
BEFORE UPDATE ON artifact_open_commands BEGIN
    SELECT CASE WHEN
        new.command_scope != old.command_scope OR
        new.idempotency_key != old.idempotency_key OR
        new.request_fingerprint != old.request_fingerprint OR
        new.artifact_id != old.artifact_id OR
        new.content_version != old.content_version OR
        new.created_at != old.created_at OR
        new.revision != old.revision + 1 OR
        new.updated_at < old.updated_at OR
        NOT (
            (old.state = 0 AND new.state IN (1, 3)) OR
            (old.state = 1 AND new.state IN (2, 3, 4))
        )
    THEN RAISE(ABORT, 'invalid artifact open transition') END;
END;
CREATE TRIGGER artifact_open_commands_immutable_delete
BEFORE DELETE ON artifact_open_commands BEGIN
    SELECT RAISE(ABORT, 'artifact open journal is immutable');
END;

CREATE TRIGGER artifacts_search_ai AFTER INSERT ON artifacts WHEN new.state = 1 BEGIN
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    VALUES (new.id, new.project_id, 'artifact', new.name, '', new.updated_at);
END;
CREATE TRIGGER artifacts_search_au AFTER UPDATE ON artifacts BEGIN
    DELETE FROM search_documents WHERE id = new.id;
    INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
    SELECT new.id, new.project_id, 'artifact', new.name, '', new.updated_at
    WHERE new.state = 1;
END;

INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
SELECT artifacts.id, artifacts.project_id, 'artifact', artifacts.name, '', artifacts.updated_at
FROM artifacts
JOIN projects ON projects.id = artifacts.project_id
LEFT JOIN threads
  ON threads.id = artifacts.thread_id AND threads.project_id = artifacts.project_id
WHERE artifacts.state = 1
  AND (artifacts.thread_id IS NULL OR threads.id IS NOT NULL);

INSERT INTO search_documents_fts(search_documents_fts) VALUES ('rebuild');
";

fn migrate_artifacts_v17(transaction: &Transaction<'_>) -> Result<(), SqlCipherStoreError> {
    validate_legacy_artifacts_v16(transaction)?;
    transaction.execute_batch(MIGRATION_17)?;
    Ok(())
}

// Epoch 15 makes content removal a durable, restartable operation. Public
// artifact metadata is tombstoned before any private bytes are deleted, while
// immutable version metadata remains available for recovery and audit. Quota
// accounting continues to include Retained and PurgePending versions and only
// releases bytes after the content adapter has confirmed an exact purge.
const MIGRATION_18: &str = r"
CREATE TABLE artifact_version_retention (
    artifact_id TEXT NOT NULL,
    content_version INTEGER NOT NULL CHECK (content_version BETWEEN 1 AND 1000000),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 2),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 0 AND 2),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    purged_at INTEGER CHECK (purged_at IS NULL OR purged_at >= created_at),
    PRIMARY KEY(artifact_id, content_version),
    FOREIGN KEY(artifact_id, content_version)
        REFERENCES artifact_versions(artifact_id, version),
    CHECK (
        (state = 0 AND revision = 0 AND updated_at = created_at AND purged_at IS NULL) OR
        (state = 1 AND revision = 1 AND purged_at IS NULL) OR
        (state = 2 AND revision = 2 AND purged_at = updated_at)
    )
) WITHOUT ROWID, STRICT;

INSERT INTO artifact_version_retention(
    artifact_id,content_version,state,revision,created_at,updated_at,purged_at
)
SELECT artifact_id,version,0,0,created_at,created_at,NULL
FROM artifact_versions;

CREATE INDEX artifact_version_retention_pending
ON artifact_version_retention(state, updated_at, artifact_id, content_version)
WHERE state = 1;

CREATE TRIGGER artifact_versions_create_retention
AFTER INSERT ON artifact_versions BEGIN
    INSERT INTO artifact_version_retention(
        artifact_id,content_version,state,revision,created_at,updated_at,purged_at
    ) VALUES (new.artifact_id,new.version,0,0,new.created_at,new.created_at,NULL);
END;

CREATE TRIGGER artifact_version_retention_validate_insert
BEFORE INSERT ON artifact_version_retention BEGIN
    SELECT CASE WHEN
        new.state != 0 OR new.revision != 0 OR new.purged_at IS NOT NULL OR
        NOT EXISTS (
            SELECT 1 FROM artifact_versions
            WHERE artifact_id = new.artifact_id
              AND version = new.content_version
              AND created_at = new.created_at
              AND new.updated_at = new.created_at
        )
    THEN RAISE(ABORT, 'invalid artifact version retention insert') END;
END;

CREATE TABLE artifact_removal_commands (
    command_scope TEXT NOT NULL CHECK (command_scope = 'remove_artifact'),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
    ),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    artifact_id TEXT NOT NULL UNIQUE CHECK (
        length(CAST(artifact_id AS BLOB)) BETWEEN 1 AND 128
    ),
    content_version INTEGER NOT NULL CHECK (content_version BETWEEN 1 AND 1000000),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 1),
    revision INTEGER NOT NULL CHECK (revision BETWEEN 0 AND 1),
    active_slot INTEGER UNIQUE CHECK (active_slot IS NULL OR active_slot = 1),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    PRIMARY KEY(command_scope, idempotency_key),
    FOREIGN KEY(artifact_id) REFERENCES artifacts(id),
    FOREIGN KEY(artifact_id, content_version)
        REFERENCES artifact_versions(artifact_id, version),
    CHECK (
        (state = 0 AND revision = 0 AND active_slot = 1 AND updated_at = created_at) OR
        (state = 1 AND revision = 1 AND active_slot IS NULL)
    )
) WITHOUT ROWID, STRICT;

CREATE INDEX artifact_removal_commands_recovery
ON artifact_removal_commands(state, created_at, artifact_id)
WHERE state = 0;

CREATE TRIGGER artifact_removal_commands_validate_insert
BEFORE INSERT ON artifact_removal_commands BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM artifacts
        WHERE id = new.artifact_id
          AND state = 1
          AND current_content_version = new.content_version
    ) THEN RAISE(ABORT, 'artifact content is not removable') END;
    SELECT CASE WHEN EXISTS (
        SELECT 1 FROM artifact_ingestions
        WHERE artifact_id = new.artifact_id AND state IN (0, 1)
    ) OR EXISTS (
        SELECT 1 FROM artifact_open_commands
        WHERE state IN (0, 1)
    ) THEN RAISE(ABORT, 'artifact operation is active') END;
    SELECT CASE WHEN EXISTS (
        SELECT 1
        FROM artifact_versions version
        LEFT JOIN artifact_version_retention retention
          ON retention.artifact_id = version.artifact_id
         AND retention.content_version = version.version
        WHERE version.artifact_id = new.artifact_id
          AND (retention.artifact_id IS NULL OR retention.state != 0)
    ) THEN RAISE(ABORT, 'artifact retention is incomplete') END;
END;

CREATE TRIGGER artifacts_validate_removal
BEFORE UPDATE ON artifacts WHEN old.state != 2 AND new.state = 2 BEGIN
    SELECT CASE WHEN
        old.state != 1 OR
        new.id != old.id OR
        new.project_id != old.project_id OR
        new.thread_id IS NOT old.thread_id OR
        new.name != old.name OR
        new.current_content_version IS NOT NULL OR
        new.revision != old.revision + 1 OR
        new.created_at != old.created_at OR
        new.updated_at < old.updated_at OR
        NOT EXISTS (
            SELECT 1 FROM artifact_removal_commands
            WHERE artifact_id = old.id
              AND content_version = old.current_content_version
              AND state = 0
              AND created_at = new.updated_at
        )
    THEN RAISE(ABORT, 'invalid artifact removal') END;
END;

CREATE TRIGGER artifacts_deleted_immutable
BEFORE UPDATE ON artifacts WHEN old.state = 2 BEGIN
    SELECT RAISE(ABORT, 'deleted artifact metadata is immutable');
END;

CREATE TRIGGER artifact_version_retention_validate_update
BEFORE UPDATE ON artifact_version_retention BEGIN
    SELECT CASE WHEN
        new.artifact_id != old.artifact_id OR
        new.content_version != old.content_version OR
        new.created_at != old.created_at OR
        new.revision != old.revision + 1 OR
        new.updated_at < old.updated_at OR
        NOT (
            (old.state = 0 AND new.state = 1 AND new.purged_at IS NULL) OR
            (old.state = 1 AND new.state = 2 AND new.purged_at = new.updated_at)
        )
    THEN RAISE(ABORT, 'invalid artifact version retention transition') END;
    SELECT CASE WHEN old.state = 0 AND NOT EXISTS (
        SELECT 1
        FROM artifact_removal_commands removal
        JOIN artifacts artifact ON artifact.id = removal.artifact_id
        WHERE removal.artifact_id = old.artifact_id
          AND removal.state = 0
          AND removal.created_at = new.updated_at
          AND artifact.state = 2
    ) THEN RAISE(ABORT, 'artifact version purge is not reserved') END;
END;

CREATE TRIGGER artifact_version_retention_immutable_delete
BEFORE DELETE ON artifact_version_retention BEGIN
    SELECT RAISE(ABORT, 'artifact version retention records are immutable');
END;

CREATE TRIGGER artifact_removal_commands_validate_update
BEFORE UPDATE ON artifact_removal_commands BEGIN
    SELECT CASE WHEN
        new.command_scope != old.command_scope OR
        new.idempotency_key != old.idempotency_key OR
        new.request_fingerprint != old.request_fingerprint OR
        new.artifact_id != old.artifact_id OR
        new.content_version != old.content_version OR
        new.created_at != old.created_at OR
        old.state != 0 OR new.state != 1 OR
        old.active_slot != 1 OR new.active_slot IS NOT NULL OR
        new.revision != old.revision + 1 OR
        new.updated_at < old.updated_at
    THEN RAISE(ABORT, 'invalid artifact removal transition') END;
END;

CREATE TRIGGER artifact_removal_commands_validate_commit
BEFORE UPDATE OF state ON artifact_removal_commands WHEN new.state = 1 BEGIN
    SELECT CASE WHEN EXISTS (
        SELECT 1
        FROM artifact_versions version
        LEFT JOIN artifact_version_retention retention
          ON retention.artifact_id = version.artifact_id
         AND retention.content_version = version.version
        WHERE version.artifact_id = new.artifact_id
          AND (retention.artifact_id IS NULL OR retention.state != 2)
    ) THEN RAISE(ABORT, 'artifact removal purge is incomplete') END;
END;

CREATE TRIGGER artifact_removal_commands_immutable_delete
BEFORE DELETE ON artifact_removal_commands BEGIN
    SELECT RAISE(ABORT, 'artifact removal journal is immutable');
END;
";

fn migrate_artifact_retention_v18(
    transaction: &Transaction<'_>,
) -> Result<(), SqlCipherStoreError> {
    validate_artifacts_v17_for_retention(transaction)?;
    transaction.execute_batch(MIGRATION_18)?;
    Ok(())
}

fn validate_artifacts_v17_for_retention(
    transaction: &Transaction<'_>,
) -> Result<(), SqlCipherStoreError> {
    let invalid: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1
             FROM artifact_versions version
             LEFT JOIN artifacts artifact ON artifact.id = version.artifact_id
             WHERE artifact.id IS NULL
             UNION ALL
             SELECT 1
             FROM artifacts artifact
             LEFT JOIN artifact_versions version
               ON version.artifact_id = artifact.id
              AND version.version = artifact.current_content_version
             WHERE artifact.state = 1 AND version.artifact_id IS NULL
             UNION ALL
             SELECT 1
             FROM artifact_open_commands command
             LEFT JOIN artifact_versions version
               ON version.artifact_id = command.artifact_id
              AND version.version = command.content_version
             WHERE version.artifact_id IS NULL
         )",
        [],
        |row| row.get(0),
    )?;
    if invalid {
        return Err(rusqlite::Error::InvalidQuery.into());
    }
    Ok(())
}

// Epoch 19 introduces only the durable, daemon-private scheduler kernel. It
// normalizes supported legacy schedule text, but creates no cursor, lease, or
// occurrence and therefore cannot enable execution. Every future scheduler
// mutation is fenced by the singleton lease and exact-command journals.
const MIGRATION_19: &str = r"
CREATE TRIGGER automation_history_validate_insert
BEFORE INSERT ON automation_history BEGIN
    SELECT CASE WHEN
        new.sequence != COALESCE((
            SELECT MAX(sequence) + 1 FROM automation_history
            WHERE automation_id = new.automation_id
        ), 1) OR
        new.status NOT BETWEEN 0 AND 3 OR
        length(CAST(new.summary AS BLOB)) > 1000
    THEN RAISE(ABORT, 'invalid automation history insertion') END;
END;
CREATE TRIGGER automation_history_immutable_update
BEFORE UPDATE ON automation_history BEGIN
    SELECT RAISE(ABORT, 'automation history is immutable');
END;
CREATE TRIGGER automation_history_immutable_delete
BEFORE DELETE ON automation_history BEGIN
    SELECT RAISE(ABORT, 'automation history is immutable');
END;

CREATE TABLE automation_scheduler_lease (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    owner_id TEXT NOT NULL CHECK (
        length(CAST(owner_id AS BLOB)) BETWEEN 1 AND 128
    ),
    fence INTEGER NOT NULL CHECK (fence > 0),
    acquired_at INTEGER NOT NULL CHECK (acquired_at >= 0),
    renewed_at INTEGER NOT NULL CHECK (renewed_at >= acquired_at),
    expires_at INTEGER NOT NULL CHECK (expires_at > renewed_at),
    CHECK (expires_at - renewed_at BETWEEN 1 AND 60000)
) STRICT;
CREATE TRIGGER automation_scheduler_lease_validate_insert
BEFORE INSERT ON automation_scheduler_lease BEGIN
    SELECT CASE WHEN new.singleton != 1 OR new.fence != 1 OR
        new.acquired_at != new.renewed_at
    THEN RAISE(ABORT, 'invalid initial automation scheduler lease') END;
END;
CREATE TRIGGER automation_scheduler_lease_validate_update
BEFORE UPDATE ON automation_scheduler_lease BEGIN
    SELECT CASE WHEN
        new.singleton != old.singleton OR NOT (
            (new.owner_id = old.owner_id
             AND new.fence = old.fence
             AND new.acquired_at = old.acquired_at
             AND new.renewed_at >= old.renewed_at
             AND new.renewed_at < old.expires_at) OR
            (new.fence > old.fence
             AND new.acquired_at >= old.expires_at
             AND new.renewed_at = new.acquired_at)
        )
    THEN RAISE(ABORT, 'invalid automation scheduler lease transition') END;
END;
CREATE TRIGGER automation_scheduler_lease_immutable_delete
BEFORE DELETE ON automation_scheduler_lease BEGIN
    SELECT RAISE(ABORT, 'automation scheduler lease cannot be deleted');
END;

CREATE TABLE automation_schedule_cursors (
    automation_id TEXT PRIMARY KEY REFERENCES automations(id) CHECK (
        length(CAST(automation_id AS BLOB)) BETWEEN 1 AND 128
    ),
    definition_revision INTEGER NOT NULL CHECK (definition_revision >= 0),
    schedule_fingerprint BLOB NOT NULL CHECK (length(schedule_fingerprint) = 32),
    calculator_version INTEGER NOT NULL CHECK (calculator_version = 1),
    evaluated_through INTEGER NOT NULL CHECK (evaluated_through >= 0),
    next_kind INTEGER CHECK (next_kind IN (0, 1)),
    next_year INTEGER,
    next_month INTEGER CHECK (next_month BETWEEN 1 AND 12),
    next_day INTEGER CHECK (next_day BETWEEN 1 AND 31),
    next_hour INTEGER CHECK (next_hour BETWEEN 0 AND 23),
    next_minute INTEGER CHECK (next_minute BETWEEN 0 AND 59),
    next_scheduled_for INTEGER CHECK (next_scheduled_for >= 0),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (
        updated_at >= created_at AND updated_at >= evaluated_through
    ),
    CHECK (revision != 0 OR updated_at = created_at),
    CHECK (
        (next_kind IS NULL
         AND next_year IS NULL AND next_month IS NULL AND next_day IS NULL
         AND next_hour IS NULL AND next_minute IS NULL
         AND next_scheduled_for IS NULL) OR
        (next_kind = 0
         AND next_year IS NOT NULL AND next_month IS NOT NULL AND next_day IS NOT NULL
         AND next_hour IS NOT NULL AND next_minute IS NOT NULL
         AND next_scheduled_for > evaluated_through) OR
        (next_kind = 1
         AND next_year IS NOT NULL AND next_month IS NOT NULL AND next_day IS NOT NULL
         AND next_hour IS NOT NULL AND next_minute IS NOT NULL
         AND next_scheduled_for IS NULL)
    )
) STRICT;
CREATE INDEX automation_schedule_cursors_due
ON automation_schedule_cursors(next_scheduled_for, automation_id)
WHERE next_kind = 0;
CREATE INDEX automation_schedule_cursors_gap
ON automation_schedule_cursors(automation_id)
WHERE next_kind = 1;
CREATE TRIGGER automation_schedule_cursors_validate_insert
BEFORE INSERT ON automation_schedule_cursors BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM automations automation
        JOIN projects project ON project.id = automation.project_id
        WHERE automation.id = new.automation_id
          AND automation.revision = new.definition_revision
          AND automation.state = 0
          AND project.state = 0
    ) THEN RAISE(ABORT, 'automation cursor definition is not enabled') END;
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM automation_schedule_evaluation_commands command
        WHERE command.automation_id = new.automation_id
          AND command.definition_revision = new.definition_revision
          AND command.schedule_fingerprint = new.schedule_fingerprint
          AND command.expected_cursor_revision IS NULL
          AND command.result_cursor_revision = new.revision
          AND command.evaluated_through = new.evaluated_through
          AND command.next_kind IS new.next_kind
          AND command.next_year IS new.next_year
          AND command.next_month IS new.next_month
          AND command.next_day IS new.next_day
          AND command.next_hour IS new.next_hour
          AND command.next_minute IS new.next_minute
          AND command.next_scheduled_for IS new.next_scheduled_for
          AND command.result_updated_at = new.updated_at
    ) THEN RAISE(ABORT, 'automation cursor initialization lacks evaluation evidence') END;
END;

CREATE TABLE automation_schedule_evaluation_commands (
    command_scope TEXT NOT NULL CHECK (command_scope = 'automation_scheduler_evaluate_v1'),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
    ),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    owner_id TEXT NOT NULL CHECK (
        length(CAST(owner_id AS BLOB)) BETWEEN 1 AND 128
    ),
    fence INTEGER NOT NULL CHECK (fence > 0),
    automation_id TEXT NOT NULL REFERENCES automations(id),
    definition_revision INTEGER NOT NULL CHECK (definition_revision >= 0),
    schedule_fingerprint BLOB NOT NULL CHECK (length(schedule_fingerprint) = 32),
    expected_cursor_revision INTEGER CHECK (
        expected_cursor_revision IS NULL OR expected_cursor_revision >= 0
    ),
    result_cursor_revision INTEGER NOT NULL CHECK (
        (expected_cursor_revision IS NULL AND result_cursor_revision = 0) OR
        result_cursor_revision = expected_cursor_revision + 1
    ),
    prior_evaluated_through INTEGER CHECK (
        prior_evaluated_through IS NULL OR prior_evaluated_through >= 0
    ),
    evaluated_through INTEGER NOT NULL CHECK (
        prior_evaluated_through IS NULL OR evaluated_through >= prior_evaluated_through
    ),
    next_kind INTEGER CHECK (next_kind IN (0, 1)),
    next_year INTEGER,
    next_month INTEGER CHECK (next_month BETWEEN 1 AND 12),
    next_day INTEGER CHECK (next_day BETWEEN 1 AND 31),
    next_hour INTEGER CHECK (next_hour BETWEEN 0 AND 23),
    next_minute INTEGER CHECK (next_minute BETWEEN 0 AND 59),
    next_scheduled_for INTEGER CHECK (next_scheduled_for >= 0),
    result_updated_at INTEGER NOT NULL CHECK (result_updated_at >= evaluated_through),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    PRIMARY KEY(command_scope, idempotency_key),
    UNIQUE(automation_id, result_cursor_revision),
    CHECK (result_updated_at = created_at),
    CHECK (
        (next_kind IS NULL
         AND next_year IS NULL AND next_month IS NULL AND next_day IS NULL
         AND next_hour IS NULL AND next_minute IS NULL
         AND next_scheduled_for IS NULL) OR
        (next_kind = 0
         AND next_year IS NOT NULL AND next_month IS NOT NULL AND next_day IS NOT NULL
         AND next_hour IS NOT NULL AND next_minute IS NOT NULL
         AND next_scheduled_for > evaluated_through) OR
        (next_kind = 1
         AND next_year IS NOT NULL AND next_month IS NOT NULL AND next_day IS NOT NULL
         AND next_hour IS NOT NULL AND next_minute IS NOT NULL
         AND next_scheduled_for IS NULL)
    )
) WITHOUT ROWID, STRICT;
CREATE TRIGGER automation_schedule_evaluation_commands_validate_insert
BEFORE INSERT ON automation_schedule_evaluation_commands BEGIN
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM automation_scheduler_lease lease
        WHERE lease.singleton = 1
          AND lease.owner_id = new.owner_id
          AND lease.fence = new.fence
          AND new.created_at >= lease.acquired_at
          AND new.created_at < lease.expires_at
    ) THEN RAISE(ABORT, 'automation evaluation has a stale scheduler fence') END;
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM automations automation
        JOIN projects project ON project.id = automation.project_id
        WHERE automation.id = new.automation_id
          AND automation.state = 0
          AND automation.revision = new.definition_revision
          AND project.state = 0
    ) THEN RAISE(ABORT, 'automation evaluation definition is not enabled') END;
    SELECT CASE WHEN NOT (
        (new.expected_cursor_revision IS NULL
         AND new.prior_evaluated_through IS NULL
         AND new.result_cursor_revision = 0
         AND NOT EXISTS (
             SELECT 1 FROM automation_schedule_cursors cursor
             WHERE cursor.automation_id = new.automation_id
         )) OR
        (new.expected_cursor_revision IS NOT NULL
         AND EXISTS (
             SELECT 1 FROM automation_schedule_cursors cursor
             WHERE cursor.automation_id = new.automation_id
               AND cursor.definition_revision = new.definition_revision
               AND cursor.schedule_fingerprint = new.schedule_fingerprint
               AND cursor.revision = new.expected_cursor_revision
               AND cursor.evaluated_through = new.prior_evaluated_through
               AND new.evaluated_through >= cursor.evaluated_through
               AND new.result_updated_at >= cursor.updated_at
         ))
    ) THEN RAISE(ABORT, 'automation evaluation does not match its cursor') END;
END;
CREATE TRIGGER automation_schedule_evaluation_commands_immutable_update
BEFORE UPDATE ON automation_schedule_evaluation_commands BEGIN
    SELECT RAISE(ABORT, 'automation schedule evaluation commands are immutable');
END;
CREATE TRIGGER automation_schedule_evaluation_commands_immutable_delete
BEFORE DELETE ON automation_schedule_evaluation_commands BEGIN
    SELECT RAISE(ABORT, 'automation schedule evaluation commands are immutable');
END;

CREATE TRIGGER automation_schedule_cursors_validate_update
BEFORE UPDATE ON automation_schedule_cursors BEGIN
    SELECT CASE WHEN
        new.automation_id != old.automation_id OR
        new.definition_revision != old.definition_revision OR
        new.schedule_fingerprint != old.schedule_fingerprint OR
        new.calculator_version != old.calculator_version OR
        new.created_at != old.created_at OR
        new.revision != old.revision + 1 OR
        new.evaluated_through < old.evaluated_through OR
        new.updated_at < old.updated_at OR
        NOT EXISTS (
            SELECT 1 FROM automation_schedule_evaluation_commands command
            WHERE command.automation_id = new.automation_id
              AND command.definition_revision = new.definition_revision
              AND command.schedule_fingerprint = new.schedule_fingerprint
              AND command.expected_cursor_revision = old.revision
              AND command.result_cursor_revision = new.revision
              AND command.prior_evaluated_through = old.evaluated_through
              AND command.evaluated_through = new.evaluated_through
              AND command.next_kind IS new.next_kind
              AND command.next_year IS new.next_year
              AND command.next_month IS new.next_month
              AND command.next_day IS new.next_day
              AND command.next_hour IS new.next_hour
              AND command.next_minute IS new.next_minute
              AND command.next_scheduled_for IS new.next_scheduled_for
              AND command.result_updated_at = new.updated_at
        )
    THEN RAISE(ABORT, 'invalid automation cursor transition') END;
END;
CREATE TRIGGER automation_schedule_cursors_immutable_delete
BEFORE DELETE ON automation_schedule_cursors BEGIN
    SELECT RAISE(ABORT, 'automation schedule cursors cannot be deleted');
END;

CREATE TABLE automation_occurrences (
    id TEXT PRIMARY KEY CHECK (length(CAST(id AS BLOB)) BETWEEN 1 AND 128),
    automation_id TEXT NOT NULL REFERENCES automations(id) CHECK (
        length(CAST(automation_id AS BLOB)) BETWEEN 1 AND 128
    ),
    evaluation_scope TEXT NOT NULL CHECK (
        evaluation_scope = 'automation_scheduler_evaluate_v1'
    ),
    evaluation_key TEXT NOT NULL CHECK (
        length(CAST(evaluation_key AS BLOB)) BETWEEN 1 AND 128
    ),
    definition_revision INTEGER NOT NULL CHECK (definition_revision >= 0),
    snapshot_project_id TEXT NOT NULL REFERENCES projects(id) CHECK (
        length(CAST(snapshot_project_id AS BLOB)) BETWEEN 1 AND 128
    ),
    snapshot_title TEXT NOT NULL CHECK (
        length(CAST(snapshot_title AS BLOB)) BETWEEN 1 AND 200
    ),
    snapshot_prompt TEXT NOT NULL CHECK (
        length(CAST(snapshot_prompt AS BLOB)) BETWEEN 1 AND 65536
    ),
    canonical_schedule TEXT NOT NULL CHECK (
        length(CAST(canonical_schedule AS BLOB)) BETWEEN 1 AND 256
    ),
    timezone TEXT NOT NULL CHECK (
        length(CAST(timezone AS BLOB)) BETWEEN 1 AND 128
    ),
    missed_run_policy INTEGER NOT NULL CHECK (missed_run_policy IN (0, 1)),
    overlap_policy INTEGER NOT NULL CHECK (overlap_policy IN (0, 1)),
    schedule_fingerprint BLOB NOT NULL CHECK (length(schedule_fingerprint) = 32),
    calculator_version INTEGER NOT NULL CHECK (calculator_version = 1),
    nominal_year INTEGER NOT NULL,
    nominal_month INTEGER NOT NULL CHECK (nominal_month BETWEEN 1 AND 12),
    nominal_day INTEGER NOT NULL CHECK (nominal_day BETWEEN 1 AND 31),
    nominal_hour INTEGER NOT NULL CHECK (nominal_hour BETWEEN 0 AND 23),
    nominal_minute INTEGER NOT NULL CHECK (nominal_minute BETWEEN 0 AND 59),
    scheduled_for INTEGER CHECK (scheduled_for IS NULL OR scheduled_for >= 0),
    occurrence_count INTEGER NOT NULL CHECK (occurrence_count BETWEEN 1 AND 370),
    initial_state INTEGER NOT NULL CHECK (initial_state IN (0, 1, 6, 7, 8)),
    initial_revision INTEGER NOT NULL CHECK (
        (initial_state IN (0, 8) AND initial_revision = 0) OR
        (initial_state IN (1, 6, 7) AND initial_revision = 1)
    ),
    state INTEGER NOT NULL CHECK (state BETWEEN 0 AND 10),
    claim_owner_id TEXT CHECK (
        claim_owner_id IS NULL OR length(CAST(claim_owner_id AS BLOB)) BETWEEN 1 AND 128
    ),
    claim_fence INTEGER CHECK (claim_fence IS NULL OR claim_fence > 0),
    claimed_at INTEGER CHECK (claimed_at IS NULL OR claimed_at >= 0),
    claim_expires_at INTEGER CHECK (claim_expires_at IS NULL OR claim_expires_at >= 0),
    run_id TEXT REFERENCES runs(id),
    claim_attempt_count INTEGER NOT NULL CHECK (claim_attempt_count BETWEEN 0 AND 16),
    revision INTEGER NOT NULL CHECK (revision >= 0),
    created_at INTEGER NOT NULL CHECK (created_at >= 0),
    updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
    FOREIGN KEY(evaluation_scope, evaluation_key)
        REFERENCES automation_schedule_evaluation_commands(command_scope, idempotency_key),
    UNIQUE(
        automation_id,definition_revision,nominal_year,nominal_month,nominal_day,
        nominal_hour,nominal_minute
    ),
    CHECK (
        (scheduled_for IS NULL AND state = 8 AND occurrence_count = 1) OR
        (scheduled_for IS NOT NULL AND state != 8)
    ),
    CHECK (
        (claim_owner_id IS NULL AND claim_fence IS NULL
         AND claimed_at IS NULL AND claim_expires_at IS NULL) OR
        (claim_owner_id IS NOT NULL AND claim_fence IS NOT NULL
         AND claimed_at IS NOT NULL AND claim_expires_at > claimed_at
         AND claim_expires_at - claimed_at BETWEEN 1 AND 60000)
    ),
    CHECK ((state IN (2, 3)) = (claim_owner_id IS NOT NULL)),
    CHECK (
        (state IN (3, 4, 5) AND run_id IS NOT NULL) OR
        (state IN (0, 1, 2, 6, 7, 8) AND run_id IS NULL) OR
        state IN (9, 10)
    ),
    CHECK (
        (state = 8 AND revision = 0 AND claim_attempt_count = 0) OR
        (state = 0) OR
        (state IN (1, 2, 6, 7, 9, 10) AND revision > 0) OR
        (state = 3 AND revision >= 2) OR
        (state IN (4, 5) AND revision >= 3)
    ),
    CHECK (
        state NOT IN (2, 3, 4, 5) OR claim_attempt_count > 0
    ),
    CHECK (
        state != 9 OR
        (run_id IS NOT NULL AND claim_attempt_count > 0) OR
        (run_id IS NULL AND claim_attempt_count = 16)
    )
) STRICT;
CREATE UNIQUE INDEX automation_occurrences_one_active
ON automation_occurrences(automation_id)
WHERE state IN (0, 2, 3);
CREATE UNIQUE INDEX automation_occurrences_one_queued
ON automation_occurrences(automation_id)
WHERE state = 1;
CREATE INDEX automation_occurrences_evaluation
ON automation_occurrences(evaluation_scope, evaluation_key, nominal_year, nominal_month,
                          nominal_day, nominal_hour, nominal_minute);
CREATE INDEX automation_occurrences_claim_recovery
ON automation_occurrences(claim_expires_at, id)
WHERE state IN (2, 3);
CREATE INDEX automation_occurrences_run
ON automation_occurrences(run_id)
WHERE run_id IS NOT NULL;

CREATE TRIGGER automation_occurrences_validate_insert
BEFORE INSERT ON automation_occurrences BEGIN
    SELECT CASE WHEN new.state != new.initial_state OR
        new.revision != new.initial_revision OR
        new.state NOT IN (0, 1, 6, 7, 8) THEN
        RAISE(ABORT, 'automation occurrence cannot start in this state') END;
    SELECT CASE WHEN NOT EXISTS (
        SELECT 1 FROM automation_schedule_evaluation_commands command
        WHERE command.command_scope = new.evaluation_scope
          AND command.idempotency_key = new.evaluation_key
          AND command.automation_id = new.automation_id
          AND command.definition_revision = new.definition_revision
          AND command.schedule_fingerprint = new.schedule_fingerprint
          AND command.created_at = new.created_at
    ) THEN RAISE(ABORT, 'automation occurrence is not bound to its evaluation') END;
END;
CREATE TRIGGER automation_occurrences_validate_update
BEFORE UPDATE ON automation_occurrences BEGIN
    SELECT CASE WHEN
        new.id != old.id OR
        new.automation_id != old.automation_id OR
        new.evaluation_scope != old.evaluation_scope OR
        new.evaluation_key != old.evaluation_key OR
        new.definition_revision != old.definition_revision OR
        new.snapshot_project_id != old.snapshot_project_id OR
        new.snapshot_title != old.snapshot_title OR
        new.snapshot_prompt != old.snapshot_prompt OR
        new.canonical_schedule != old.canonical_schedule OR
        new.timezone != old.timezone OR
        new.missed_run_policy != old.missed_run_policy OR
        new.overlap_policy != old.overlap_policy OR
        new.schedule_fingerprint != old.schedule_fingerprint OR
        new.calculator_version != old.calculator_version OR
        new.nominal_year != old.nominal_year OR
        new.nominal_month != old.nominal_month OR
        new.nominal_day != old.nominal_day OR
        new.nominal_hour != old.nominal_hour OR
        new.nominal_minute != old.nominal_minute OR
        new.scheduled_for IS NOT old.scheduled_for OR
        new.occurrence_count != old.occurrence_count OR
        new.initial_state != old.initial_state OR
        new.initial_revision != old.initial_revision OR
        new.created_at != old.created_at OR
        new.revision != old.revision + 1 OR
        new.updated_at < old.updated_at OR
        NOT (
            (old.state = 0 AND new.state IN (1, 2, 6, 7, 9, 10)) OR
            (old.state = 1 AND new.state IN (0, 7, 10)) OR
            (old.state = 2 AND new.state IN (0, 3, 10)) OR
            (old.state = 3 AND new.state IN (4, 5, 9, 10))
        ) OR
        NOT (
            (old.state = 0 AND new.state = 2
             AND new.claim_attempt_count = old.claim_attempt_count + 1) OR
            (NOT (old.state = 0 AND new.state = 2)
             AND new.claim_attempt_count = old.claim_attempt_count)
        ) OR
        NOT (
            (old.state = 2 AND new.state = 3
             AND old.run_id IS NULL AND new.run_id IS NOT NULL) OR
            (NOT (old.state = 2 AND new.state = 3)
             AND new.run_id IS old.run_id)
        )
    THEN RAISE(ABORT, 'invalid automation occurrence transition') END;
    SELECT CASE WHEN new.state = 2 AND NOT EXISTS (
        SELECT 1 FROM automation_scheduler_lease lease
        WHERE lease.singleton = 1
          AND lease.owner_id = new.claim_owner_id
          AND lease.fence = new.claim_fence
          AND new.claimed_at >= lease.acquired_at
          AND new.claimed_at < lease.expires_at
          AND new.claim_expires_at <= lease.expires_at
    ) THEN RAISE(ABORT, 'automation occurrence claim has a stale fence') END;
    SELECT CASE WHEN old.state = 2 AND new.state = 0
        AND new.updated_at < old.claim_expires_at
    THEN RAISE(ABORT, 'automation occurrence claim is not expired') END;
END;
CREATE TRIGGER automation_occurrences_immutable_delete
BEFORE DELETE ON automation_occurrences BEGIN
    SELECT RAISE(ABORT, 'automation occurrences are immutable journals');
END;

CREATE TABLE automation_occurrence_claim_attempts (
    occurrence_id TEXT NOT NULL REFERENCES automation_occurrences(id),
    sequence INTEGER NOT NULL CHECK (sequence BETWEEN 1 AND 16),
    command_scope TEXT NOT NULL CHECK (
        command_scope = 'automation_scheduler_claim_v1'
    ),
    idempotency_key TEXT NOT NULL CHECK (
        length(CAST(idempotency_key AS BLOB)) BETWEEN 1 AND 128
    ),
    request_fingerprint BLOB NOT NULL CHECK (length(request_fingerprint) = 32),
    owner_id TEXT NOT NULL CHECK (
        length(CAST(owner_id AS BLOB)) BETWEEN 1 AND 128
    ),
    fence INTEGER NOT NULL CHECK (fence > 0),
    claimed_at INTEGER NOT NULL CHECK (claimed_at >= 0),
    expires_at INTEGER NOT NULL CHECK (
        expires_at > claimed_at AND expires_at - claimed_at BETWEEN 1 AND 60000
    ),
    completed_at INTEGER CHECK (completed_at IS NULL OR completed_at >= claimed_at),
    outcome INTEGER CHECK (outcome IS NULL OR outcome BETWEEN 0 AND 3),
    result_occurrence_revision INTEGER NOT NULL CHECK (
        result_occurrence_revision > 0
    ),
    PRIMARY KEY(occurrence_id, sequence),
    UNIQUE(command_scope, idempotency_key),
    CHECK ((completed_at IS NULL) = (outcome IS NULL))
) WITHOUT ROWID, STRICT;
CREATE INDEX automation_occurrence_claim_attempts_recovery
ON automation_occurrence_claim_attempts(expires_at, occurrence_id, sequence)
WHERE completed_at IS NULL;
CREATE TRIGGER automation_occurrence_claim_attempts_validate_insert
BEFORE INSERT ON automation_occurrence_claim_attempts BEGIN
    SELECT CASE WHEN EXISTS (
        SELECT 1 FROM automation_occurrence_claim_attempts
        WHERE occurrence_id = new.occurrence_id AND completed_at IS NULL
    ) OR NOT EXISTS (
        SELECT 1 FROM automation_occurrences occurrence
        WHERE occurrence.id = new.occurrence_id
          AND occurrence.state = 2
          AND occurrence.claim_attempt_count = new.sequence
          AND occurrence.claim_owner_id = new.owner_id
          AND occurrence.claim_fence = new.fence
          AND occurrence.claimed_at = new.claimed_at
          AND occurrence.claim_expires_at = new.expires_at
          AND occurrence.revision = new.result_occurrence_revision
    ) OR new.sequence != COALESCE((
        SELECT MAX(sequence) + 1 FROM automation_occurrence_claim_attempts
        WHERE occurrence_id = new.occurrence_id
    ), 1)
    THEN RAISE(ABORT, 'invalid automation occurrence claim attempt') END;
END;
CREATE TRIGGER automation_occurrence_claim_attempts_validate_update
BEFORE UPDATE ON automation_occurrence_claim_attempts BEGIN
    SELECT CASE WHEN
        new.occurrence_id != old.occurrence_id OR
        new.sequence != old.sequence OR
        new.command_scope != old.command_scope OR
        new.idempotency_key != old.idempotency_key OR
        new.request_fingerprint != old.request_fingerprint OR
        new.owner_id != old.owner_id OR
        new.fence != old.fence OR
        new.claimed_at != old.claimed_at OR
        new.expires_at != old.expires_at OR
        new.result_occurrence_revision != old.result_occurrence_revision OR
        old.completed_at IS NOT NULL OR
        new.completed_at IS NULL OR
        NOT (
            (new.outcome = 0 AND new.completed_at >= old.expires_at AND EXISTS (
                SELECT 1 FROM automation_occurrences occurrence
                WHERE occurrence.id = new.occurrence_id AND occurrence.state = 0
            )) OR
            (new.outcome = 1 AND EXISTS (
                SELECT 1 FROM automation_occurrences occurrence
                WHERE occurrence.id = new.occurrence_id AND occurrence.state = 3
                  AND occurrence.run_id IS NOT NULL
            )) OR
            (new.outcome IN (2, 3) AND EXISTS (
                SELECT 1 FROM automation_occurrences occurrence
                WHERE occurrence.id = new.occurrence_id
                  AND occurrence.state IN (9, 10)
            ))
        )
    THEN RAISE(ABORT, 'invalid automation occurrence claim completion') END;
END;
CREATE TRIGGER automation_occurrence_claim_attempts_immutable_delete
BEFORE DELETE ON automation_occurrence_claim_attempts BEGIN
    SELECT RAISE(ABORT, 'automation occurrence claim attempts are immutable');
END;

CREATE TRIGGER automations_require_scheduler_rebase
BEFORE UPDATE ON automations WHEN EXISTS (
    SELECT 1 FROM automation_schedule_cursors cursor WHERE cursor.automation_id = old.id
) BEGIN
    SELECT RAISE(ABORT, 'automation with a scheduler cursor requires atomic rebase');
END;
";

fn migrate_automation_scheduler_v19(
    transaction: &Transaction<'_>,
) -> Result<(), SqlCipherStoreError> {
    let normalized = validate_automations_v18_for_scheduler(transaction)?;
    for (automation_id, schedule) in normalized {
        transaction.execute(
            "UPDATE automations SET schedule=?1 WHERE id=?2",
            params![schedule, automation_id],
        )?;
    }
    transaction.execute_batch(MIGRATION_19)?;
    Ok(())
}

fn validate_automations_v18_for_scheduler(
    transaction: &Transaction<'_>,
) -> Result<Vec<(String, String)>, SqlCipherStoreError> {
    let orphaned: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM automations automation
             LEFT JOIN projects project ON project.id=automation.project_id
             WHERE project.id IS NULL
             UNION ALL
             SELECT 1 FROM automation_history history
             LEFT JOIN automations automation ON automation.id=history.automation_id
             WHERE automation.id IS NULL
         )",
        [],
        |row| row.get(0),
    )?;
    if orphaned {
        return Err(rusqlite::Error::InvalidQuery.into());
    }

    let normalized = {
        let mut statement = transaction.prepare(
            "SELECT id,project_id,title,prompt,schedule,timezone,missed_run_policy,
                    overlap_policy,state,revision,created_at,updated_at
             FROM automations ORDER BY id",
        )?;
        let rows = statement.query_map([], mapping::automation_from_row)?;
        let mut normalized = Vec::new();
        for row in rows {
            let mut automation: Automation = row?;
            if automation.state == AutomationState::Enabled {
                return Err(rusqlite::Error::InvalidQuery.into());
            }
            let schedule = AutomationSchedule::parse_for_normalization(
                &automation.schedule,
                &automation.timezone,
            )
            .map_err(|_| rusqlite::Error::InvalidQuery)?
            .to_canonical_string();
            let automation_id = automation.id.to_string();
            automation.schedule.clone_from(&schedule);
            Automation::restore(automation).map_err(|_| rusqlite::Error::InvalidQuery)?;
            normalized.push((automation_id, schedule));
        }
        normalized
    };

    let mut statement = transaction.prepare(
        "SELECT automation_id,sequence,scheduled_for,recorded_at,status,summary
         FROM automation_history ORDER BY automation_id,sequence",
    )?;
    let rows = statement.query_map([], mapping::automation_history_from_row)?;
    let mut previous_id = None::<String>;
    let mut expected_sequence = 1_u64;
    for row in rows {
        let entry: AutomationHistoryEntry = row?;
        if previous_id.as_deref() != Some(entry.automation_id.as_str()) {
            previous_id = Some(entry.automation_id.to_string());
            expected_sequence = 1;
        }
        if entry.sequence != expected_sequence {
            return Err(rusqlite::Error::InvalidQuery.into());
        }
        AutomationHistoryEntry::restore(entry).map_err(|_| rusqlite::Error::InvalidQuery)?;
        expected_sequence = expected_sequence
            .checked_add(1)
            .ok_or(rusqlite::Error::InvalidQuery)?;
    }
    Ok(normalized)
}

fn validate_legacy_artifacts_v16(transaction: &Transaction<'_>) -> Result<(), SqlCipherStoreError> {
    let mut statement = transaction.prepare(
        "SELECT id,project_id,thread_id,name,state,revision,created_at,updated_at
         FROM artifacts",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
            row.get::<_, i64>(6)?,
            row.get::<_, i64>(7)?,
        ))
    })?;
    for row in rows {
        let (id, project_id, thread_id, name, state, revision, created_at, updated_at) = row?;
        let invalid_identifier = |value: &str| {
            value.is_empty() || value.len() > 128 || value.chars().any(char::is_control)
        };
        if invalid_identifier(&id)
            || invalid_identifier(&project_id)
            || thread_id.as_deref().is_some_and(invalid_identifier)
            || name.trim().is_empty()
            || name.len() > 200
            || name.chars().any(char::is_control)
            || !matches!(state, 0 | 1)
            || revision < 0
            || created_at < 0
            || updated_at < created_at
            || (revision == 0 && updated_at != created_at)
            || (state == 1 && revision == 0)
        {
            return Err(rusqlite::Error::InvalidQuery.into());
        }
    }
    Ok(())
}

fn migrate_conversation_turn_events(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    transaction.execute_batch(MIGRATION_12_TABLE)?;
    validate_conversation_event_backfill_source(transaction)?;
    insert_backfilled_conversation_lifecycle_events(transaction)?;
    insert_backfilled_completed_text_events(transaction)?;
    validate_backfilled_conversation_event_projection(transaction)?;
    transaction.execute_batch(MIGRATION_12_TRIGGERS)
}

fn validate_conversation_event_backfill_source(
    transaction: &Transaction<'_>,
) -> rusqlite::Result<()> {
    let invalid_backfill: bool = transaction.query_row(
        &format!(
            "SELECT EXISTS(
                 SELECT 1
                 FROM conversation_turns turns
                 LEFT JOIN side_effects effects ON effects.id=turns.effect_id
                 LEFT JOIN messages assistants ON assistants.id=turns.assistant_message_id
                 WHERE length(CAST(turns.id AS BLOB)) NOT BETWEEN 1 AND 128
                    OR (turns.state=0 AND (
                        turns.revision!=0 OR turns.updated_at!=turns.created_at
                    ))
                    OR (turns.state=1 AND (
                        turns.revision!=1 OR effects.id IS NULL
                        OR effects.created_at!=turns.updated_at
                    ))
                    OR (turns.state=2 AND (
                        turns.revision!=2 OR effects.id IS NULL
                        OR effects.created_at>turns.updated_at
                        OR assistants.id IS NULL
                        OR length(CAST(assistants.content AS BLOB)) NOT BETWEEN 1 AND {MAX_MESSAGE_BYTES}
                    ))
                    OR (turns.state IN (3,5) AND (
                        turns.revision!=2 OR effects.id IS NULL
                        OR effects.created_at>turns.updated_at
                    ))
                    OR (turns.state=4 AND turns.revision!=1)
             )"
        ),
        [],
        |row| row.get(0),
    )?;
    if invalid_backfill {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

fn insert_backfilled_conversation_lifecycle_events(
    transaction: &Transaction<'_>,
) -> rusqlite::Result<()> {
    transaction.execute(
        "INSERT INTO conversation_turn_events(
             turn_id,sequence,kind,from_state,to_state,start_utf8_offset,text
         )
         SELECT id,1,0,NULL,NULL,NULL,NULL FROM conversation_turns",
        [],
    )?;
    transaction.execute(
        "INSERT INTO conversation_turn_events(
             turn_id,sequence,kind,from_state,to_state,start_utf8_offset,text
         )
         SELECT id,2,1,0,1,NULL,NULL FROM conversation_turns
         WHERE state IN (1,2,3,5)",
        [],
    )?;
    transaction.execute(
        "INSERT INTO conversation_turn_events(
             turn_id,sequence,kind,from_state,to_state,start_utf8_offset,text
         )
         SELECT id,2,1,0,4,NULL,NULL FROM conversation_turns WHERE state=4",
        [],
    )?;
    transaction.execute(
        "INSERT INTO conversation_turn_events(
             turn_id,sequence,kind,from_state,to_state,start_utf8_offset,text
         )
         SELECT id,3,1,1,state,NULL,NULL FROM conversation_turns WHERE state IN (3,5)",
        [],
    )?;
    Ok(())
}

fn insert_backfilled_completed_text_events(transaction: &Transaction<'_>) -> rusqlite::Result<()> {
    let mut after_id = String::new();
    loop {
        let completed = transaction
            .query_row(
                "SELECT turns.id,assistants.content
                 FROM conversation_turns turns
                 JOIN messages assistants ON assistants.id=turns.assistant_message_id
                 WHERE turns.state=2 AND turns.id>?1
                 ORDER BY turns.id LIMIT 1",
                [&after_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        let Some((turn_id, text)) = completed else {
            break;
        };
        let mut sequence = 3_u64;
        let mut start = 0_usize;
        while start < text.len() {
            let mut end = start
                .saturating_add(MAX_CONVERSATION_TEXT_CHUNK_BYTES)
                .min(text.len());
            while !text.is_char_boundary(end) {
                end = end.saturating_sub(1);
            }
            if end == start {
                return Err(rusqlite::Error::InvalidQuery);
            }
            transaction.execute(
                "INSERT INTO conversation_turn_events(
                     turn_id,sequence,kind,from_state,to_state,start_utf8_offset,text
                 ) VALUES (?1,?2,2,NULL,NULL,?3,?4)",
                params![
                    turn_id,
                    i64::try_from(sequence).map_err(|_| rusqlite::Error::InvalidQuery)?,
                    i64::try_from(start).map_err(|_| rusqlite::Error::InvalidQuery)?,
                    &text[start..end],
                ],
            )?;
            sequence = sequence
                .checked_add(1)
                .ok_or(rusqlite::Error::InvalidQuery)?;
            start = end;
        }
        transaction.execute(
            "INSERT INTO conversation_turn_events(
                 turn_id,sequence,kind,from_state,to_state,start_utf8_offset,text
             ) VALUES (?1,?2,1,1,2,NULL,NULL)",
            params![
                turn_id,
                i64::try_from(sequence).map_err(|_| rusqlite::Error::InvalidQuery)?,
            ],
        )?;
        after_id = turn_id;
    }
    Ok(())
}

fn validate_backfilled_conversation_event_projection(
    transaction: &Transaction<'_>,
) -> rusqlite::Result<()> {
    let invalid_projection: bool = transaction.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM conversation_turns turns
             WHERE NOT EXISTS (
                 SELECT 1 FROM conversation_turn_events events
                 WHERE events.turn_id=turns.id AND events.sequence=1 AND events.kind=0
             ) OR (turns.state!=0 AND NOT EXISTS (
                 SELECT 1 FROM conversation_turn_events events
                 WHERE events.turn_id=turns.id AND events.kind=1
                   AND events.to_state=turns.state
             ))
         )",
        [],
        |row| row.get(0),
    )?;
    if invalid_projection {
        return Err(rusqlite::Error::InvalidQuery);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use grok_application::DatabaseKey;
    use rusqlite::{Connection, Transaction, params};

    use super::*;

    fn create_version_six_database(path: &Path, key: &DatabaseKey) -> Connection {
        let connection = Connection::open(path).expect("connection");
        let key_hex = hex::encode(key.expose_secret());
        connection
            .execute_batch(&format!(
                "PRAGMA key = \"x'{key_hex}'\";
                 PRAGMA foreign_keys = ON;
                 {MIGRATION_1} {MIGRATION_2} {MIGRATION_3}
                 {MIGRATION_4} {MIGRATION_5} {MIGRATION_6}
                 PRAGMA user_version = 6;"
            ))
            .expect("version six schema");
        connection
            .execute(
                "INSERT INTO schema_migrations(version, applied_at_unix_ms)
                 VALUES (1, 1), (2, 2), (3, 3), (4, 4), (5, 5), (6, 6)",
                [],
            )
            .expect("version six migration history");
        connection
    }

    fn create_version_seven_database(path: &Path, key: &DatabaseKey) -> Connection {
        let mut connection = create_version_six_database(path, key);
        let transaction = connection.transaction().expect("version seven transaction");
        transaction
            .execute_batch(MIGRATION_7)
            .expect("version seven schema");
        transaction
            .execute(
                "INSERT INTO schema_migrations(version, applied_at_unix_ms) VALUES (7, 7)",
                [],
            )
            .expect("version seven migration history");
        transaction
            .execute_batch("PRAGMA user_version = 7;")
            .expect("version seven marker");
        transaction.commit().expect("commit version seven");
        connection
    }

    fn create_version_eight_database(path: &Path, key: &DatabaseKey) -> Connection {
        let mut connection = create_version_seven_database(path, key);
        let transaction = connection.transaction().expect("version eight transaction");
        transaction
            .execute_batch(MIGRATION_8)
            .expect("version eight schema");
        transaction
            .execute(
                "INSERT INTO schema_migrations(version, applied_at_unix_ms) VALUES (8, 8)",
                [],
            )
            .expect("version eight migration history");
        transaction
            .execute_batch("PRAGMA user_version = 8;")
            .expect("version eight marker");
        transaction.commit().expect("commit version eight");
        connection
    }

    fn create_version_nine_database(path: &Path, key: &DatabaseKey) -> Connection {
        let mut connection = create_version_eight_database(path, key);
        let transaction = connection.transaction().expect("version nine transaction");
        transaction
            .execute_batch(MIGRATION_9)
            .expect("version nine schema");
        transaction
            .execute(
                "INSERT INTO schema_migrations(version, applied_at_unix_ms) VALUES (9, 9)",
                [],
            )
            .expect("version nine migration history");
        transaction
            .execute_batch("PRAGMA user_version = 9;")
            .expect("version nine marker");
        transaction.commit().expect("commit version nine");
        connection
    }

    fn create_version_ten_database(path: &Path, key: &DatabaseKey) -> Connection {
        let mut connection = create_version_nine_database(path, key);
        let transaction = connection.transaction().expect("version ten transaction");
        transaction
            .execute_batch(MIGRATION_10)
            .expect("version ten schema");
        transaction
            .execute(
                "INSERT INTO schema_migrations(version, applied_at_unix_ms) VALUES (10, 10)",
                [],
            )
            .expect("version ten migration history");
        transaction
            .execute_batch("PRAGMA user_version = 10;")
            .expect("version ten marker");
        transaction.commit().expect("commit version ten");
        connection
    }

    fn create_version_eleven_database(path: &Path, key: &DatabaseKey) -> Connection {
        let mut connection = create_version_ten_database(path, key);
        let transaction = connection
            .transaction()
            .expect("version eleven transaction");
        transaction
            .execute_batch(MIGRATION_11)
            .expect("version eleven schema");
        transaction
            .execute(
                "INSERT INTO schema_migrations(version, applied_at_unix_ms) VALUES (11, 11)",
                [],
            )
            .expect("version eleven migration history");
        transaction
            .execute_batch("PRAGMA user_version = 11;")
            .expect("version eleven marker");
        transaction.commit().expect("commit version eleven");
        connection
    }

    fn migrate_fixture_to_version_twelve(connection: &mut Connection) {
        let transaction = connection
            .transaction()
            .expect("version twelve transaction");
        migrate_conversation_turn_events(&transaction).expect("version twelve schema");
        transaction
            .execute(
                "INSERT INTO schema_migrations(version, applied_at_unix_ms) VALUES (12, 12)",
                [],
            )
            .expect("version twelve migration history");
        transaction
            .execute_batch("PRAGMA user_version = 12;")
            .expect("version twelve marker");
        transaction.commit().expect("commit version twelve");
    }

    fn migrate_fixture_to_version_thirteen(connection: &mut Connection) {
        let transaction = connection
            .transaction()
            .expect("version thirteen transaction");
        transaction
            .execute_batch(MIGRATION_13)
            .expect("version thirteen schema");
        transaction
            .execute(
                "INSERT INTO schema_migrations(version, applied_at_unix_ms) VALUES (13, 13)",
                [],
            )
            .expect("version thirteen migration history");
        transaction
            .execute_batch("PRAGMA user_version = 13;")
            .expect("version thirteen marker");
        transaction.commit().expect("commit version thirteen");
    }

    fn insert_version_eleven_completed_turn(connection: &Connection, assistant_text: &str) {
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('event-project','Events','',0,0,1,1);
                 INSERT INTO threads(
                     id,project_id,title,state,revision,created_at,updated_at
                 ) VALUES ('event-thread','event-project','Events',0,0,1,1);
                 INSERT INTO messages(
                     id,thread_id,sequence,role,content,state,revision,created_at,updated_at
                 ) VALUES ('event-user','event-thread',1,1,'Prompt',0,0,1,1);
                 INSERT INTO runs(
                     id,project_id,thread_id,state,revision,created_at,updated_at
                 ) VALUES ('event-run','event-project','event-thread',5,3,1,3);
                 INSERT INTO side_effects(
                     id,run_id,kind,target,idempotency,state,revision,created_at,updated_at
                 ) VALUES (
                     'event-effect','event-run',2,
                     'official xAI Responses API model grok-4.3',1,2,2,2,3
                 );",
            )
            .expect("version eleven completed links");
        connection
            .execute(
                "INSERT INTO messages(
                     id,thread_id,sequence,role,content,state,revision,created_at,updated_at
                 ) VALUES ('event-assistant','event-thread',2,2,?1,0,0,3,3)",
                [assistant_text],
            )
            .expect("version eleven assistant");
        connection
            .execute(
                "INSERT INTO conversation_turns(
                     id,idempotency_key,request_fingerprint,provider_request_fingerprint,
                     project_id,thread_id,user_message_id,run_id,model_id,state,effect_id,
                     assistant_message_id,failure_kind,failure_message,failure_retryable,
                     provider_response_id,citations_json,input_tokens,output_tokens,
                     cost_in_usd_ticks,zero_data_retention,revision,created_at,updated_at
                 ) VALUES (
                     'event-turn','event-command',zeroblob(32),zeroblob(32),
                     'event-project','event-thread','event-user','event-run','grok-4.3',2,
                     'event-effect','event-assistant',NULL,NULL,NULL,'event-response','[]',
                     0,0,0,1,2,1,3
                 )",
                [],
            )
            .expect("version eleven completed turn");
        connection
            .execute(
                "INSERT INTO conversation_turn_context(
                     turn_id,sequence,message_id,role,content,revision,created_at,updated_at
                 ) VALUES ('event-turn',1,'event-user',1,'Prompt',0,1,1)",
                [],
            )
            .expect("version eleven context");
    }

    fn migration_history_count(connection: &Connection, version: u32) -> u32 {
        connection
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=?1",
                [version],
                |row| row.get(0),
            )
            .expect("migration history")
    }

    fn downgrade_scheduler_schema_to_v18(connection: &Connection) {
        connection
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 DROP TRIGGER IF EXISTS automations_require_scheduler_rebase;
                 DROP TRIGGER IF EXISTS automation_occurrence_claim_attempts_immutable_delete;
                 DROP TRIGGER IF EXISTS automation_occurrence_claim_attempts_validate_update;
                 DROP TRIGGER IF EXISTS automation_occurrence_claim_attempts_validate_insert;
                 DROP TRIGGER IF EXISTS automation_occurrences_immutable_delete;
                 DROP TRIGGER IF EXISTS automation_occurrences_validate_update;
                 DROP TRIGGER IF EXISTS automation_occurrences_validate_insert;
                 DROP TRIGGER IF EXISTS automation_schedule_cursors_immutable_delete;
                 DROP TRIGGER IF EXISTS automation_schedule_cursors_validate_update;
                 DROP TRIGGER IF EXISTS automation_schedule_evaluation_commands_immutable_delete;
                 DROP TRIGGER IF EXISTS automation_schedule_evaluation_commands_immutable_update;
                 DROP TRIGGER IF EXISTS automation_schedule_evaluation_commands_validate_insert;
                 DROP TRIGGER IF EXISTS automation_schedule_cursors_validate_insert;
                 DROP TRIGGER IF EXISTS automation_scheduler_lease_immutable_delete;
                 DROP TRIGGER IF EXISTS automation_scheduler_lease_validate_update;
                 DROP TRIGGER IF EXISTS automation_scheduler_lease_validate_insert;
                 DROP TRIGGER IF EXISTS automation_history_immutable_delete;
                 DROP TRIGGER IF EXISTS automation_history_immutable_update;
                 DROP TRIGGER IF EXISTS automation_history_validate_insert;
                 DROP TABLE IF EXISTS automation_occurrence_claim_attempts;
                 DROP TABLE IF EXISTS automation_occurrences;
                 DROP TABLE IF EXISTS automation_schedule_evaluation_commands;
                 DROP TABLE IF EXISTS automation_schedule_cursors;
                 DROP TABLE IF EXISTS automation_scheduler_lease;
                 DELETE FROM schema_migrations WHERE version=19;
                 PRAGMA user_version=18;
                 PRAGMA foreign_keys=ON;",
            )
            .expect("downgrade scheduler schema to version eighteen");
    }

    fn downgrade_empty_artifact_schema_to_v16(connection: &Connection) {
        downgrade_scheduler_schema_to_v18(connection);
        connection
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 DROP TRIGGER artifact_removal_commands_immutable_delete;
                 DROP TRIGGER artifact_removal_commands_validate_commit;
                 DROP TRIGGER artifact_removal_commands_validate_update;
                 DROP TRIGGER artifact_version_retention_immutable_delete;
                 DROP TRIGGER artifact_version_retention_validate_update;
                 DROP TRIGGER artifacts_deleted_immutable;
                 DROP TRIGGER artifacts_validate_removal;
                 DROP TRIGGER artifact_removal_commands_validate_insert;
                 DROP TRIGGER artifact_version_retention_validate_insert;
                 DROP TRIGGER artifact_versions_create_retention;
                 DROP TRIGGER artifacts_search_ai;
                 DROP TRIGGER artifacts_search_au;
                 DROP INDEX artifacts_project_recent;
                 DROP TABLE artifact_removal_commands;
                 DROP TABLE artifact_version_retention;
                 DROP TABLE artifact_open_commands;
                 DROP TABLE artifact_ingestions;
                 DROP TABLE artifacts;
                 DROP TABLE artifact_versions;
                 CREATE TABLE artifacts (
                     id TEXT PRIMARY KEY,
                     project_id TEXT NOT NULL REFERENCES projects(id),
                     thread_id TEXT,
                     name TEXT NOT NULL,
                     relative_path TEXT NOT NULL,
                     media_type TEXT NOT NULL,
                     byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
                     state INTEGER NOT NULL,
                     revision INTEGER NOT NULL CHECK (revision >= 0),
                     created_at INTEGER NOT NULL CHECK (created_at >= 0),
                     updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
                     FOREIGN KEY(thread_id, project_id) REFERENCES threads(id, project_id)
                 ) STRICT;
                 CREATE INDEX artifacts_project_recent
                 ON artifacts(project_id, updated_at DESC, id);
                 CREATE TRIGGER artifacts_search_ai
                 AFTER INSERT ON artifacts WHEN new.state=0 BEGIN
                     INSERT INTO search_documents(id,project_id,kind,title,body,updated_at)
                     VALUES (new.id,new.project_id,'artifact',new.name,'',new.updated_at);
                 END;
                 CREATE TRIGGER artifacts_search_au AFTER UPDATE ON artifacts BEGIN
                     DELETE FROM search_documents WHERE id=new.id;
                     INSERT INTO search_documents(id,project_id,kind,title,body,updated_at)
                     SELECT new.id,new.project_id,'artifact',new.name,'',new.updated_at
                     WHERE new.state=0;
                 END;
                 DELETE FROM schema_migrations WHERE version IN (17,18);
                 PRAGMA user_version=16;
                 PRAGMA foreign_keys=ON;",
            )
            .expect("downgrade empty artifact schema to version sixteen");
    }

    fn downgrade_artifact_retention_schema_to_v17(connection: &Connection) {
        downgrade_scheduler_schema_to_v18(connection);
        connection
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 DROP TRIGGER artifact_removal_commands_immutable_delete;
                 DROP TRIGGER artifact_removal_commands_validate_commit;
                 DROP TRIGGER artifact_removal_commands_validate_update;
                 DROP TRIGGER artifact_version_retention_immutable_delete;
                 DROP TRIGGER artifact_version_retention_validate_update;
                 DROP TRIGGER artifacts_deleted_immutable;
                 DROP TRIGGER artifacts_validate_removal;
                 DROP TRIGGER artifact_removal_commands_validate_insert;
                 DROP TRIGGER artifact_version_retention_validate_insert;
                 DROP TRIGGER artifact_versions_create_retention;
                 DROP TABLE artifact_removal_commands;
                 DROP TABLE artifact_version_retention;
                 DELETE FROM schema_migrations WHERE version=18;
                 PRAGMA user_version=17;
                 PRAGMA foreign_keys=ON;",
            )
            .expect("downgrade artifact retention schema to version seventeen");
    }

    fn epoch_validation_trigger_count(connection: &Connection) -> u32 {
        connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='trigger'
                   AND name='privileged_operation_attempts_validate_epoch_insert'",
                [],
                |row| row.get(0),
            )
            .expect("epoch validation trigger count")
    }

    fn assert_v11_rejects_invalid_epoch_evidence(connection: &mut Connection) {
        // Bypass table CHECK constraints so each failure specifically proves that
        // the forward-only v11 trigger protects attempts inserted after upgrade.
        connection
            .execute_batch("PRAGMA ignore_check_constraints = ON;")
            .expect("isolate migration eleven trigger");
        for (operation_id, transport_id, broker_boot_id, guest_boot_id, deadline) in [
            (
                "zero-broker-operation",
                "zero-broker-transport",
                &[0_u8; 16][..],
                &[3_u8; 16][..],
                201,
            ),
            (
                "zero-guest-operation",
                "zero-guest-transport",
                &[2_u8; 16][..],
                &[0_u8; 16][..],
                201,
            ),
            (
                "stable-id-operation",
                "stable-id-operation",
                &[2_u8; 16][..],
                &[3_u8; 16][..],
                201,
            ),
            (
                "zero-duration-operation",
                "zero-duration-transport",
                &[2_u8; 16][..],
                &[3_u8; 16][..],
                101,
            ),
        ] {
            insert_prepared(connection, operation_id, 0).expect("prepared operation");
            let transaction = connection
                .transaction()
                .expect("invalid attempt transaction");
            stage_first_dispatch(&transaction, operation_id).expect("dispatch state");
            assert!(
                insert_dispatching_attempt(
                    &transaction,
                    operation_id,
                    transport_id,
                    &[1_u8; 32],
                    broker_boot_id,
                    guest_boot_id,
                    deadline,
                )
                .is_err(),
                "migration eleven accepted invalid evidence for {operation_id}"
            );
            transaction.rollback().expect("rollback invalid attempt");
        }
    }

    fn open_test_database(marker: u8) -> (tempfile::TempDir, Connection) {
        let directory = tempfile::tempdir().expect("tempdir");
        let key = DatabaseKey::from_slice(&[marker; 32]).expect("key");
        let connection = open_encrypted(&directory.path().join("state.db"), &key).expect("open");
        (directory, connection)
    }

    fn target_columns(
        kind: i64,
    ) -> (
        i64,
        Option<&'static str>,
        Option<&'static str>,
        Option<&'static str>,
        Option<i64>,
    ) {
        match kind {
            0 => (0, None, None, None, None),
            1 => (1, None, None, None, None),
            2 => (1, Some("integration-main"), None, None, None),
            3 => (
                1,
                Some("integration-main"),
                Some("instance-main"),
                None,
                None,
            ),
            4 => (0, Some("integration-main"), None, None, None),
            5 => (
                1,
                Some("integration-main"),
                Some("instance-main"),
                Some("application-main"),
                Some(1),
            ),
            _ => panic!("unsupported test operation kind"),
        }
    }

    fn insert_prepared_with_identity(
        connection: &mut Connection,
        id: &str,
        kind: i64,
        authority_grant_id: &str,
        idempotency_key: &str,
        links: (Option<&str>, Option<&str>, Option<&str>, Option<&str>),
    ) -> rusqlite::Result<()> {
        let transaction = connection.transaction()?;
        insert_prepared_record(
            &transaction,
            id,
            kind,
            authority_grant_id,
            idempotency_key,
            links,
        )?;
        transaction.commit()
    }

    fn insert_prepared_record(
        connection: &Connection,
        id: &str,
        kind: i64,
        authority_grant_id: &str,
        idempotency_key: &str,
        links: (Option<&str>, Option<&str>, Option<&str>, Option<&str>),
    ) -> rusqlite::Result<()> {
        let (retry_class, integration_id, instance_id, application_id, observation_revision) =
            target_columns(kind);
        connection.execute(
            "INSERT INTO privileged_operations(
                 id, operation_kind, retry_class, target_vm_id,
                 target_integration_id, target_instance_id, target_application_id,
                 target_observation_revision, payload_digest, retained_payload_digest,
                 authority_grant_id,
                 authority_expires_at, idempotency_key, request_digest,
                 run_id, effect_id, approval_id, supersedes_id, state,
                 attempt_count, revision, created_at, updated_at
             ) VALUES (
                 ?1, ?2, ?3, 'vm-primary', ?4, ?5, ?6, ?7, ?8, ?8, ?9,
                 1000, ?10, ?11, ?12, ?13, ?14, ?15, 0, 0, 0, 100, 100
             )",
            params![
                id,
                kind,
                retry_class,
                integration_id,
                instance_id,
                application_id,
                observation_revision,
                &[11_u8; 32][..],
                authority_grant_id,
                idempotency_key,
                &[12_u8; 32][..],
                links.0,
                links.1,
                links.2,
                links.3,
            ],
        )?;
        connection.execute(
            "INSERT INTO privileged_operation_payloads(
                 operation_id, payload_digest, payload, created_at
             ) VALUES (?1, ?2, ?3, 100)",
            params![id, &[11_u8; 32][..], b"{}".as_slice()],
        )?;
        Ok(())
    }

    fn insert_prepared(connection: &mut Connection, id: &str, kind: i64) -> rusqlite::Result<()> {
        insert_prepared_with_identity(
            connection,
            id,
            kind,
            &format!("authority-grant-{id}"),
            &format!("idempotency-key-{id}"),
            (None, None, None, None),
        )
    }

    fn dispatch_first_attempt(
        connection: &mut Connection,
        id: &str,
        transport_operation_id: &str,
    ) -> rusqlite::Result<()> {
        let transaction = connection.transaction()?;
        stage_first_dispatch(&transaction, id)?;
        insert_dispatching_attempt(
            &transaction,
            id,
            transport_operation_id,
            &[21_u8; 32],
            &[22_u8; 16],
            &[23_u8; 16],
            201,
        )?;
        transaction.commit()
    }

    fn stage_first_dispatch(transaction: &Transaction<'_>, id: &str) -> rusqlite::Result<()> {
        transaction.execute(
            "UPDATE privileged_operations
             SET state=1, attempt_count=1, last_attempt_sequence=1,
                 last_attempt_certainty=0, revision=1, updated_at=101
             WHERE id=?1",
            [id],
        )?;
        Ok(())
    }

    fn insert_dispatching_attempt(
        transaction: &Transaction<'_>,
        id: &str,
        transport_operation_id: &str,
        wire_digest: &[u8],
        broker_boot_id: &[u8],
        guest_boot_id: &[u8],
        deadline_unix_ms: i64,
    ) -> rusqlite::Result<()> {
        transaction.execute(
            "INSERT INTO privileged_operation_attempts(
                 operation_id, sequence, transport_operation_id, wire_digest,
                 broker_boot_id, guest_boot_id, started_at, deadline_unix_ms,
                 outcome_certainty
             ) VALUES (?1, 1, ?2, ?3, ?4, ?5, 101, ?6, 0)",
            params![
                id,
                transport_operation_id,
                wire_digest,
                broker_boot_id,
                guest_boot_id,
                deadline_unix_ms,
            ],
        )?;
        Ok(())
    }

    fn complete_first_attempt(
        connection: &mut Connection,
        id: &str,
        state: i64,
        certainty: i64,
        result_digest: Option<&[u8]>,
        failure_code: Option<&str>,
    ) -> rusqlite::Result<()> {
        let has_terminal_result = matches!(state, 3 | 4);
        let retained_payload_digest = (!has_terminal_result).then_some(&[11_u8; 32][..]);
        let terminal_digest = has_terminal_result.then_some(&[31_u8; 32][..]);
        let terminal_payload = has_terminal_result.then_some(b"terminal-result".as_slice());
        let transaction = connection.transaction()?;
        transaction.execute(
            "UPDATE privileged_operations
             SET state=?2, last_attempt_certainty=?3, revision=2, updated_at=102,
                 retained_payload_digest=?4,
                 terminal_result_digest=?5, terminal_result_payload=?6
             WHERE id=?1",
            params![
                id,
                state,
                certainty,
                retained_payload_digest,
                terminal_digest,
                terminal_payload,
            ],
        )?;
        transaction.execute(
            "UPDATE privileged_operation_attempts
             SET completed_at=102, outcome_certainty=?2,
                 result_digest=?3, failure_code=?4
             WHERE operation_id=?1 AND sequence=1",
            params![id, certainty, result_digest, failure_code],
        )?;
        transaction.commit()
    }

    fn prepare_uncertain_operation(
        connection: &mut Connection,
        id: &str,
        transport_operation_id: &str,
    ) {
        insert_prepared(connection, id, 1).expect("operation");
        dispatch_first_attempt(connection, id, transport_operation_id).expect("dispatch");
        complete_first_attempt(connection, id, 5, 4, None, Some("outcome_unknown"))
            .expect("uncertain outcome");
    }

    fn stage_review(transaction: &Transaction<'_>, id: &str) -> rusqlite::Result<()> {
        transaction.execute(
            "UPDATE privileged_operations
             SET state=6, review_disposition=2, retained_payload_digest=NULL,
                 revision=3, updated_at=103
             WHERE id=?1",
            [id],
        )?;
        Ok(())
    }

    fn assert_terminal_tombstone(connection: &Connection, id: &str) {
        let tombstone: (u32, Option<Vec<u8>>, Vec<u8>, Vec<u8>) = connection
            .query_row(
                "SELECT terminal_result_pruned, terminal_result_payload,
                        terminal_result_digest, payload_digest
                 FROM privileged_operations WHERE id=?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("terminal tombstone");
        assert_eq!(tombstone.0, 1);
        assert_eq!(tombstone.1, None);
        assert_eq!(tombstone.2.len(), 32);
        assert_eq!(tombstone.3.len(), 32);
    }

    #[test]
    fn migration_enables_wal_and_fts5_indexing() {
        let directory = tempfile::tempdir().expect("tempdir");
        let key = DatabaseKey::from_slice(&[5; 32]).expect("key");
        let connection = open_encrypted(&directory.path().join("state.db"), &key).expect("open");
        let journal: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("journal mode");
        assert_eq!(journal, "wal");
        connection
            .execute(
                "INSERT INTO search_documents(id, project_id, kind, title, body, updated_at)
                 VALUES ('doc-1', 'project-1', 'message', 'Launch notes', 'Mars research', 1)",
                [],
            )
            .expect("index document");
        let matched: String = connection
            .query_row(
                "SELECT title FROM search_documents_fts
                 WHERE search_documents_fts MATCH 'Mars'",
                [],
                |row| row.get(0),
            )
            .expect("search");
        assert_eq!(matched, "Launch notes");
    }

    #[test]
    fn version_two_database_migrates_forward_without_retaining_orphaned_search_data() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[6; 32]).expect("key");
        let connection = Connection::open(&path).expect("connection");
        let key_hex = hex::encode(key.expose_secret());
        connection
            .execute_batch(&format!(
                "PRAGMA key = \"x'{key_hex}'\"; {MIGRATION_1} {MIGRATION_2}
                 PRAGMA user_version = 2;"
            ))
            .expect("version two schema");
        connection
            .execute(
                "INSERT INTO schema_migrations(version,applied_at_unix_ms) VALUES (1,1),(2,2)",
                [],
            )
            .expect("migration history");
        connection
            .execute(
                "INSERT INTO search_documents(id,project_id,kind,title,body,updated_at)
                 VALUES ('legacy','project-1','message','Legacy','preserved search',1)",
                [],
            )
            .expect("legacy search");
        drop(connection);

        let migrated = open_encrypted(&path, &key).expect("migrated");
        let version: u32 = migrated
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("version");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        let legacy_rows: u32 = migrated
            .query_row(
                "SELECT count(*) FROM search_documents WHERE id='legacy'",
                [],
                |row| row.get(0),
            )
            .expect("orphaned derived document count");
        assert_eq!(legacy_rows, 0);
        let legacy_matches: u32 = migrated
            .query_row(
                "SELECT count(*) FROM search_documents_fts
                 WHERE search_documents_fts MATCH 'preserved'",
                [],
                |row| row.get(0),
            )
            .expect("orphaned FTS match count");
        assert_eq!(legacy_matches, 0);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn version_six_database_migrates_to_latest_without_losing_prior_rows() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[7; 32]).expect("key");
        let connection = create_version_six_database(&path, &key);
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id, name, description, state, revision, created_at, updated_at
                 ) VALUES ('legacy-project', 'Legacy project', '', 0, 0, 10, 10);
                 INSERT INTO threads(
                     id, project_id, title, state, revision, created_at, updated_at
                 ) VALUES ('legacy-thread', 'legacy-project', 'Legacy thread', 0, 0, 10, 10);
                 INSERT INTO messages(
                     id, thread_id, sequence, role, content, state, revision,
                     created_at, updated_at
                 ) VALUES (
                     'legacy-message', 'legacy-thread', 1, 0, 'Preserved body',
                     0, 0, 10, 10
                 );
                 INSERT INTO runs(
                     id, project_id, thread_id, state, revision, created_at, updated_at
                 ) VALUES ('legacy-run', 'legacy-project', 'legacy-thread', 0, 0, 10, 10);
                 INSERT INTO approvals(
                     id, run_id, action, target, data_summary, risk, scope,
                     status, revision, created_at, expires_at
                 ) VALUES (
                     'legacy-approval', 'legacy-run', 'filesystem.write', 'report.md',
                     'report', 1, 0, 0, 0, 10, 1000
                 );
                 INSERT INTO side_effects(
                     id, run_id, kind, target, idempotency, state, revision,
                     created_at, updated_at
                 ) VALUES (
                     'legacy-effect', 'legacy-run', 0, 'report.md', 1, 0, 0, 10, 10
                 );
                 INSERT INTO credential_commands(
                     scope, idempotency_key, request_fingerprint, completed,
                     xai_api_key_configured, xai_capabilities_resolved
                 ) VALUES (
                     'credential', 'legacy-credential-key', zeroblob(32), 1, 1, 1
                 );
                 INSERT INTO conversation_turns(
                     id, idempotency_key, request_fingerprint, project_id, thread_id,
                     user_message_id, run_id, model_id, state, revision,
                     created_at, updated_at
                 ) VALUES (
                     'legacy-turn', 'legacy-turn-key', zeroblob(32), 'legacy-project',
                     'legacy-thread', 'legacy-message', 'legacy-run', 'grok-test',
                     0, 0, 10, 10
                 );",
            )
            .expect("representative version six rows");
        drop(connection);

        let migrated = open_encrypted(&path, &key).expect("migrate to latest version");
        let version: u32 = migrated
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        for table in [
            "projects",
            "threads",
            "messages",
            "runs",
            "approvals",
            "side_effects",
            "credential_commands",
            "conversation_turns",
        ] {
            let count: u32 = migrated
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("preserved row count");
            assert_eq!(count, 1, "unexpected row count for {table}");
        }
        let migration_seven: u32 = migrated
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=7",
                [],
                |row| row.get(0),
            )
            .expect("migration history");
        assert_eq!(migration_seven, 1);
        let migration_eight: u32 = migrated
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=8",
                [],
                |row| row.get(0),
            )
            .expect("preference migration history");
        assert_eq!(migration_eight, 1);
        let migration_nine: u32 = migrated
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=9",
                [],
                |row| row.get(0),
            )
            .expect("model preference migration history");
        assert_eq!(migration_nine, 1);
        let migration_ten: u32 = migrated
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=10",
                [],
                |row| row.get(0),
            )
            .expect("search cache repair migration history");
        assert_eq!(migration_ten, 1);
        let default_close_behavior: bool = migrated
            .query_row(
                "SELECT keep_running_in_notification_area FROM desktop_preferences WHERE singleton=1",
                [],
                |row| row.get(0),
            )
            .expect("default desktop preference");
        assert!(default_close_behavior);
        let foreign_key_violations: u32 = migrated
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .expect("foreign key check");
        assert_eq!(foreign_key_violations, 0);
    }

    #[test]
    fn version_seven_database_migrates_to_latest_with_default_preferences() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[17; 32]).expect("key");
        let connection = create_version_seven_database(&path, &key);
        connection
            .execute(
                "INSERT INTO projects(
                     id, name, description, state, revision, created_at, updated_at
                 ) VALUES ('preserved-project', 'Preserved', '', 0, 0, 10, 10)",
                [],
            )
            .expect("representative version seven row");
        drop(connection);

        let migrated = open_encrypted(&path, &key).expect("migrate to latest version");
        let version: u32 = migrated
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        let project_name: String = migrated
            .query_row(
                "SELECT name FROM projects WHERE id='preserved-project'",
                [],
                |row| row.get(0),
            )
            .expect("preserved project");
        assert_eq!(project_name, "Preserved");
        let preference: (bool, i64, i64) = migrated
            .query_row(
                "SELECT keep_running_in_notification_area,revision,updated_at
                 FROM desktop_preferences WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("default desktop preference");
        assert_eq!(preference, (true, 0, 0));
        let migration_eight: u32 = migrated
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=8",
                [],
                |row| row.get(0),
            )
            .expect("migration eight history");
        assert_eq!(migration_eight, 1);
        let model: (String, i64, i64) = migrated
            .query_row(
                "SELECT selected_model_id,revision,updated_at
                 FROM chat_model_preferences WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("default chat model preference");
        assert_eq!(model, ("grok-4.3".into(), 0, 0));
    }

    #[test]
    fn migration_eight_rolls_back_schema_and_version_together() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[18; 32]).expect("key");
        let connection = create_version_seven_database(&path, &key);
        connection
            .execute_batch("CREATE TABLE desktop_preferences(conflict TEXT) STRICT;")
            .expect("migration conflict fixture");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());

        let connection = Connection::open(&path).expect("inspect failed migration");
        let key_hex = hex::encode(key.expose_secret());
        connection
            .execute_batch(&format!("PRAGMA key = \"x'{key_hex}'\";"))
            .expect("unlock failed migration");
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 7);
        let history: u32 = connection
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=8",
                [],
                |row| row.get(0),
            )
            .expect("migration history");
        assert_eq!(history, 0);
        let command_table: u32 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='table' AND name='desktop_preference_commands'",
                [],
                |row| row.get(0),
            )
            .expect("partial table check");
        assert_eq!(command_table, 0);
    }

    #[test]
    fn version_eight_database_migrates_to_latest_with_default_chat_model() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[19; 32]).expect("key");
        let connection = create_version_eight_database(&path, &key);
        connection
            .execute(
                "UPDATE desktop_preferences
                 SET keep_running_in_notification_area=0,revision=1,updated_at=10
                 WHERE singleton=1",
                [],
            )
            .expect("representative version eight preference");
        drop(connection);

        let migrated = open_encrypted(&path, &key).expect("migrate to latest version");
        let version: u32 = migrated
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        let close_behavior: (bool, i64) = migrated
            .query_row(
                "SELECT keep_running_in_notification_area,revision
                 FROM desktop_preferences WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("preserved desktop preference");
        assert_eq!(close_behavior, (false, 1));
        let model: (String, i64, i64) = migrated
            .query_row(
                "SELECT selected_model_id,revision,updated_at
                 FROM chat_model_preferences WHERE singleton=1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("default chat model");
        assert_eq!(model, ("grok-4.3".into(), 0, 0));
    }

    #[test]
    fn migration_nine_rolls_back_schema_and_version_together() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[20; 32]).expect("key");
        let connection = create_version_eight_database(&path, &key);
        connection
            .execute_batch("CREATE TABLE chat_model_preferences(conflict TEXT) STRICT;")
            .expect("migration conflict fixture");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());

        let connection = Connection::open(&path).expect("inspect failed migration");
        let key_hex = hex::encode(key.expose_secret());
        connection
            .execute_batch(&format!("PRAGMA key = \"x'{key_hex}'\";"))
            .expect("unlock failed migration");
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 8);
        let history: u32 = connection
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=9",
                [],
                |row| row.get(0),
            )
            .expect("migration history");
        assert_eq!(history, 0);
        let command_table: u32 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='table' AND name='chat_model_preference_commands'",
                [],
                |row| row.get(0),
            )
            .expect("partial table check");
        assert_eq!(command_table, 0);
    }

    #[test]
    fn version_nine_database_rebuilds_search_cache_from_canonical_entities() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[21; 32]).expect("key");
        let connection = create_version_nine_database(&path, &key);
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id, name, description, state, revision, created_at, updated_at
                 ) VALUES (
                     'canonical-project', 'Canonical title', 'canonical searchable body',
                     0, 0, 10, 10
                 );
                 UPDATE search_documents
                 SET project_id='forged-project', title='Forged title',
                     body='forged searchable body', updated_at=999
                 WHERE id='canonical-project';
                 INSERT INTO search_documents(
                     id, project_id, kind, title, body, updated_at
                 ) VALUES (
                     'orphan-document', 'forged-project', 'message', 'Orphan',
                     'orphan searchable body', 999
                 );",
            )
            .expect("damaged version nine search cache");
        drop(connection);

        let migrated = open_encrypted(&path, &key).expect("repair search cache");
        let version: u32 = migrated
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        let document: (String, String, String, String, i64) = migrated
            .query_row(
                "SELECT project_id,kind,title,body,updated_at
                 FROM search_documents WHERE id='canonical-project'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("rebuilt canonical document");
        assert_eq!(
            document,
            (
                "canonical-project".into(),
                "project".into(),
                "Canonical title".into(),
                "canonical searchable body".into(),
                10,
            )
        );
        let orphan_count: u32 = migrated
            .query_row(
                "SELECT count(*) FROM search_documents WHERE id='orphan-document'",
                [],
                |row| row.get(0),
            )
            .expect("orphan count");
        assert_eq!(orphan_count, 0);
        let canonical_matches: u32 = migrated
            .query_row(
                "SELECT count(*) FROM search_documents_fts
                 WHERE search_documents_fts MATCH 'canonical'",
                [],
                |row| row.get(0),
            )
            .expect("canonical FTS match");
        assert_eq!(canonical_matches, 1);
        let forged_matches: u32 = migrated
            .query_row(
                "SELECT count(*) FROM search_documents_fts
                 WHERE search_documents_fts MATCH 'forged OR orphan'",
                [],
                |row| row.get(0),
            )
            .expect("forged FTS match");
        assert_eq!(forged_matches, 0);
    }

    #[test]
    fn migration_ten_rolls_back_and_restarts_as_one_transaction() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[22; 32]).expect("key");
        let connection = create_version_nine_database(&path, &key);
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id, name, description, state, revision, created_at, updated_at
                 ) VALUES ('restart-project', 'Restart', 'repair', 0, 0, 10, 10);
                 CREATE TRIGGER reject_search_rebuild
                 BEFORE INSERT ON search_documents BEGIN
                     SELECT RAISE(ABORT, 'injected search rebuild failure');
                 END;",
            )
            .expect("migration failure fixture");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());

        let connection = Connection::open(&path).expect("inspect failed migration");
        let key_hex = hex::encode(key.expose_secret());
        connection
            .execute_batch(&format!("PRAGMA key = \"x'{key_hex}'\";"))
            .expect("unlock failed migration");
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 9);
        let history: u32 = connection
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=10",
                [],
                |row| row.get(0),
            )
            .expect("migration history");
        assert_eq!(history, 0);
        let rolled_back_document: u32 = connection
            .query_row(
                "SELECT count(*) FROM search_documents WHERE id='restart-project'",
                [],
                |row| row.get(0),
            )
            .expect("rolled back cache");
        assert_eq!(rolled_back_document, 1);
        connection
            .execute_batch("DROP TRIGGER reject_search_rebuild;")
            .expect("remove injected failure");
        drop(connection);

        let restarted = open_encrypted(&path, &key).expect("restart migration");
        let version: u32 = restarted
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("restarted schema version");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        let history: u32 = restarted
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=10",
                [],
                |row| row.get(0),
            )
            .expect("restarted migration history");
        assert_eq!(history, 1);
    }

    #[test]
    fn version_ten_upgrades_restartably_and_v11_trigger_rejects_invalid_epoch_evidence() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[23; 32]).expect("key");
        let mut connection = create_version_ten_database(&path, &key);
        insert_prepared(&mut connection, "preserved-attempt-operation", 0)
            .expect("prepared version ten operation");
        dispatch_first_attempt(
            &mut connection,
            "preserved-attempt-operation",
            "preserved-transport-operation",
        )
        .expect("valid version ten attempt");
        assert_eq!(epoch_validation_trigger_count(&connection), 0);
        drop(connection);

        let mut migrated = open_encrypted(&path, &key).expect("migrate version ten to eleven");
        let version: u32 = migrated
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        assert_eq!(migration_history_count(&migrated, 11), 1);
        assert_eq!(epoch_validation_trigger_count(&migrated), 1);
        let preserved_attempts: u32 = migrated
            .query_row(
                "SELECT count(*) FROM privileged_operation_attempts
                 WHERE operation_id='preserved-attempt-operation'",
                [],
                |row| row.get(0),
            )
            .expect("preserved attempt count");
        assert_eq!(preserved_attempts, 1);
        assert_v11_rejects_invalid_epoch_evidence(&mut migrated);
        drop(migrated);

        let restarted = open_encrypted(&path, &key).expect("restart upgraded database");
        assert_eq!(migration_history_count(&restarted, 11), 1);
    }

    #[test]
    fn version_eleven_backfills_completed_turn_events_restartably_at_utf8_boundaries() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("conversation-events.db");
        let key = DatabaseKey::from_slice(&[24; 32]).expect("key");
        let text = format!(
            "{}é-tail",
            "x".repeat(MAX_CONVERSATION_TEXT_CHUNK_BYTES - 1)
        );
        let connection = create_version_eleven_database(&path, &key);
        insert_version_eleven_completed_turn(&connection, &text);
        drop(connection);

        let migrated = open_encrypted(&path, &key).expect("migrate conversation events");
        assert_eq!(migration_history_count(&migrated, 12), 1);
        let cancellation_primary_key = migrated
            .prepare(
                "SELECT name,pk FROM pragma_table_info('conversation_turn_cancel_commands')
                 WHERE pk > 0 ORDER BY pk",
            )
            .expect("cancellation command schema query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("cancellation command schema rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect cancellation command schema");
        assert_eq!(
            cancellation_primary_key,
            vec![("command_scope".into(), 1), ("idempotency_key".into(), 2)]
        );
        let rows = migrated
            .prepare(
                "SELECT sequence,kind,from_state,to_state,start_utf8_offset,text
                 FROM conversation_turn_events WHERE turn_id='event-turn'
                 ORDER BY sequence",
            )
            .expect("event query")
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<i64>>(2)?,
                    row.get::<_, Option<i64>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            })
            .expect("event rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect event rows");
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0], (1, 0, None, None, None, None));
        assert_eq!(rows[1], (2, 1, Some(0), Some(1), None, None));
        assert_eq!(rows[2].0, 3);
        assert_eq!(rows[2].1, 2);
        assert_eq!(rows[2].4, Some(0));
        assert_eq!(
            rows[2].5.as_deref(),
            Some(&text[..MAX_CONVERSATION_TEXT_CHUNK_BYTES - 1])
        );
        assert_eq!(rows[3].0, 4);
        assert_eq!(rows[3].1, 2);
        assert_eq!(
            rows[3].4,
            Some(i64::try_from(MAX_CONVERSATION_TEXT_CHUNK_BYTES - 1).expect("offset"))
        );
        assert_eq!(
            rows[3].5.as_deref(),
            Some(&text[MAX_CONVERSATION_TEXT_CHUNK_BYTES - 1..])
        );
        assert_eq!(rows[4], (5, 1, Some(1), Some(2), None, None));
        drop(migrated);

        let restarted = open_encrypted(&path, &key).expect("restart migrated schema");
        assert_eq!(migration_history_count(&restarted, 12), 1);
        let cancellation_scope_column: u32 = restarted
            .query_row(
                "SELECT count(*) FROM pragma_table_info('conversation_turn_cancel_commands')
                 WHERE name='command_scope' AND \"notnull\"=1 AND pk=1",
                [],
                |row| row.get(0),
            )
            .expect("restarted cancellation scope schema");
        assert_eq!(cancellation_scope_column, 1);
        let reconstructed: String = restarted
            .query_row(
                "SELECT group_concat(text, '') FROM (
                     SELECT text FROM conversation_turn_events
                     WHERE turn_id='event-turn' AND kind=2 ORDER BY sequence
                 )",
                [],
                |row| row.get(0),
            )
            .expect("reconstructed text");
        assert_eq!(reconstructed, text);
    }

    #[test]
    fn version_twelve_backfill_rolls_back_on_oversized_legacy_text() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("conversation-events-oversized.db");
        let key = DatabaseKey::from_slice(&[25; 32]).expect("key");
        let connection = create_version_eleven_database(&path, &key);
        insert_version_eleven_completed_turn(&connection, &"x".repeat(MAX_MESSAGE_BYTES + 1));
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());
        let connection = Connection::open(&path).expect("inspect failed migration");
        connection
            .execute_batch(&format!(
                "PRAGMA key = \"x'{}'\";",
                hex::encode(key.expose_secret())
            ))
            .expect("unlock failed migration");
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 11);
        assert_eq!(migration_history_count(&connection, 12), 0);
        let event_table_count: u32 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='table' AND name='conversation_turn_events'",
                [],
                |row| row.get(0),
            )
            .expect("event table count");
        assert_eq!(event_table_count, 0);
        connection
            .execute(
                "UPDATE messages SET content='repaired response'
                 WHERE id='event-assistant'",
                [],
            )
            .expect("repair oversized legacy text");
        drop(connection);

        let restarted = open_encrypted(&path, &key).expect("restart repaired migration");
        assert_eq!(migration_history_count(&restarted, 12), 1);
        let text_event_count: u32 = restarted
            .query_row(
                "SELECT count(*) FROM conversation_turn_events
                 WHERE turn_id='event-turn' AND kind=2",
                [],
                |row| row.get(0),
            )
            .expect("backfilled text event count");
        assert_eq!(text_event_count, 1);
    }

    #[test]
    fn version_twelve_backfills_thread_identity_and_legacy_lineage_restartably() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("conversation-lineage.db");
        let key = DatabaseKey::from_slice(&[26; 32]).expect("key");
        let mut connection = create_version_eleven_database(&path, &key);
        insert_version_eleven_completed_turn(&connection, "legacy answer");
        migrate_fixture_to_version_twelve(&mut connection);
        drop(connection);

        let migrated = open_encrypted(&path, &key).expect("migrate conversation lineage");
        assert_eq!(migration_history_count(&migrated, 13), 1);
        let lineage: (i64, Option<String>, Option<String>, i64) = migrated
            .query_row(
                "SELECT origin,source_turn_id,credential_binding_id,retry_depth
                 FROM conversation_turn_lineage WHERE turn_id='event-turn'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("backfilled lineage");
        assert_eq!(lineage, (0, None, None, 0));
        let legacy_identity: (i64, Option<String>) = migrated
            .query_row(
                "SELECT source,credential_binding_id
                 FROM conversation_thread_identity WHERE thread_id='event-thread'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("backfilled legacy thread identity");
        assert_eq!(legacy_identity, (0, None));
        let legacy_context: String = migrated
            .query_row(
                "SELECT content FROM conversation_turn_context
                 WHERE turn_id='event-turn' AND sequence=1",
                [],
                |row| row.get(0),
            )
            .expect("read migrated legacy context");
        assert_eq!(legacy_context, "Prompt");
        assert!(
            migrated
                .execute(
                    "UPDATE conversation_thread_identity
                     SET credential_binding_id='forged-legacy-binding'
                     WHERE thread_id='event-thread'",
                    [],
                )
                .is_err()
        );
        assert!(
            migrated
                .execute(
                    "UPDATE conversation_turn_lineage SET retry_depth=1
                     WHERE turn_id='event-turn'",
                    [],
                )
                .is_err()
        );
        assert!(
            migrated
                .execute(
                    "DELETE FROM conversation_turn_lineage WHERE turn_id='event-turn'",
                    [],
                )
                .is_err()
        );
        drop(migrated);

        let restarted = open_encrypted(&path, &key).expect("restart lineage schema");
        assert_eq!(migration_history_count(&restarted, 13), 1);
        let lineage_count: u32 = restarted
            .query_row(
                "SELECT count(*) FROM conversation_turn_lineage",
                [],
                |row| row.get(0),
            )
            .expect("lineage count");
        assert_eq!(lineage_count, 1);
        let restarted_identity: (i64, Option<String>) = restarted
            .query_row(
                "SELECT source,credential_binding_id
                 FROM conversation_thread_identity WHERE thread_id='event-thread'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("restarted legacy identity");
        assert_eq!(restarted_identity, (0, None));
        let restarted_context: String = restarted
            .query_row(
                "SELECT content FROM conversation_turn_context
                 WHERE turn_id='event-turn' AND sequence=1",
                [],
                |row| row.get(0),
            )
            .expect("read restarted legacy context");
        assert_eq!(restarted_context, "Prompt");
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn artifact_path_privacy_and_content_authority_migrations_restart_after_rollback() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory
            .path()
            .join("artifact-search-privacy-migration.db");
        let key = DatabaseKey::from_slice(&[31; 32]).expect("key");
        let connection = open_encrypted(&path, &key).expect("create current schema");
        downgrade_scheduler_schema_to_v18(&connection);
        connection
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 DROP TRIGGER artifact_removal_commands_immutable_delete;
                 DROP TRIGGER artifact_removal_commands_validate_commit;
                 DROP TRIGGER artifact_removal_commands_validate_update;
                 DROP TRIGGER artifact_version_retention_immutable_delete;
                 DROP TRIGGER artifact_version_retention_validate_update;
                 DROP TRIGGER artifacts_deleted_immutable;
                 DROP TRIGGER artifacts_validate_removal;
                 DROP TRIGGER artifact_removal_commands_validate_insert;
                 DROP TRIGGER artifact_version_retention_validate_insert;
                 DROP TRIGGER artifact_versions_create_retention;
                 DROP TRIGGER artifacts_search_ai;
                 DROP TRIGGER artifacts_search_au;
                 DROP INDEX artifacts_project_recent;
                 DROP TABLE artifact_removal_commands;
                 DROP TABLE artifact_version_retention;
                 DROP TABLE artifact_open_commands;
                 DROP TABLE artifact_ingestions;
                 DROP TABLE artifacts;
                 DROP TABLE artifact_versions;
                 CREATE TABLE artifacts (
                     id TEXT PRIMARY KEY,
                     project_id TEXT NOT NULL REFERENCES projects(id),
                     thread_id TEXT,
                     name TEXT NOT NULL,
                     relative_path TEXT NOT NULL,
                     media_type TEXT NOT NULL,
                     byte_size INTEGER NOT NULL CHECK (byte_size >= 0),
                     state INTEGER NOT NULL,
                     revision INTEGER NOT NULL CHECK (revision >= 0),
                     created_at INTEGER NOT NULL CHECK (created_at >= 0),
                     updated_at INTEGER NOT NULL CHECK (updated_at >= created_at),
                     FOREIGN KEY(thread_id, project_id) REFERENCES threads(id, project_id)
                 ) STRICT;
                 CREATE INDEX artifacts_project_recent
                 ON artifacts(project_id, updated_at DESC, id);
                 CREATE TRIGGER artifacts_search_ai
                 AFTER INSERT ON artifacts WHEN new.state=0 BEGIN
                     INSERT INTO search_documents(id,project_id,kind,title,body,updated_at)
                     VALUES (new.id,new.project_id,'artifact',new.name,'',new.updated_at);
                 END;
                 CREATE TRIGGER artifacts_search_au AFTER UPDATE ON artifacts BEGIN
                     DELETE FROM search_documents WHERE id=new.id;
                     INSERT INTO search_documents(id,project_id,kind,title,body,updated_at)
                     SELECT new.id,new.project_id,'artifact',new.name,'',new.updated_at
                     WHERE new.state=0;
                 END;
                 DELETE FROM schema_migrations WHERE version IN (17,18);
                 PRAGMA user_version=16;
                 PRAGMA foreign_keys=ON;

                 INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('artifact-project','Artifact project','',0,0,1,1);
                 INSERT INTO artifacts(
                     id,project_id,thread_id,name,relative_path,media_type,byte_size,
                     state,revision,created_at,updated_at
                 ) VALUES (
                     'artifact-private','artifact-project',NULL,'Visible report',
                     'privatecachetoken/report.txt','text/plain',12,0,0,1,1
                 );

                 DROP TRIGGER artifacts_search_ai;
                 DROP TRIGGER artifacts_search_au;
                 CREATE TRIGGER artifacts_search_ai
                 AFTER INSERT ON artifacts WHEN new.state=0 BEGIN
                     INSERT INTO search_documents(id,project_id,kind,title,body,updated_at)
                     VALUES (
                         new.id,new.project_id,'artifact',new.name,new.relative_path,new.updated_at
                     );
                 END;
                 CREATE TRIGGER artifacts_search_au AFTER UPDATE ON artifacts BEGIN
                     DELETE FROM search_documents WHERE id=new.id;
                     INSERT INTO search_documents(id,project_id,kind,title,body,updated_at)
                     SELECT new.id,new.project_id,'artifact',new.name,new.relative_path,new.updated_at
                     WHERE new.state=0;
                 END;
                 UPDATE search_documents
                 SET body='privatecachetoken/report.txt'
                 WHERE id='artifact-private';

                 DROP TRIGGER search_documents_au;
                 UPDATE search_documents SET body='' WHERE id='artifact-private';
                 CREATE TRIGGER search_documents_au AFTER UPDATE ON search_documents BEGIN
                     INSERT INTO search_documents_fts(
                         search_documents_fts,rowid,title,body
                     ) VALUES ('delete',old.rowid,old.title,old.body);
                     INSERT INTO search_documents_fts(rowid,title,body)
                     VALUES (new.rowid,new.title,new.body);
                 END;

                 DELETE FROM schema_migrations WHERE version IN (16,17,18);
                 PRAGMA user_version=15;
                 CREATE TRIGGER block_artifact_search_rebuild
                 BEFORE INSERT ON search_documents WHEN new.kind='artifact' BEGIN
                     SELECT RAISE(ABORT, 'block artifact search rebuild');
                 END;",
            )
            .expect("construct schema fifteen artifact search fixture");
        let stale_path_hits: u32 = connection
            .query_row(
                "SELECT count(*) FROM search_documents_fts
                 WHERE search_documents_fts MATCH 'privatecachetoken'",
                [],
                |row| row.get(0),
            )
            .expect("desynchronized legacy path posting");
        assert_eq!(stale_path_hits, 1);
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());
        let rolled_back = Connection::open(&path).expect("inspect rolled-back migration");
        apply_encryption_key(&rolled_back, &key).expect("unlock rolled-back migration");
        let version: u32 = rolled_back
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 15);
        assert_eq!(migration_history_count(&rolled_back, 16), 0);
        let retained_body: String = rolled_back
            .query_row(
                "SELECT body FROM search_documents WHERE id='artifact-private'",
                [],
                |row| row.get(0),
            )
            .expect("legacy body survives rollback");
        assert!(retained_body.is_empty());
        let retained_stale_path_hits: u32 = rolled_back
            .query_row(
                "SELECT count(*) FROM search_documents_fts
                 WHERE search_documents_fts MATCH 'privatecachetoken'",
                [],
                |row| row.get(0),
            )
            .expect("legacy path posting survives rollback");
        assert_eq!(retained_stale_path_hits, 1);
        let legacy_trigger: String = rolled_back
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='trigger' AND name='artifacts_search_ai'",
                [],
                |row| row.get(0),
            )
            .expect("legacy trigger survives rollback");
        assert!(legacy_trigger.contains("new.relative_path"));
        rolled_back
            .execute_batch("DROP TRIGGER block_artifact_search_rebuild;")
            .expect("remove migration blocker");
        drop(rolled_back);

        let migrated = open_encrypted(&path, &key).expect("restart schema sixteen migration");
        assert_eq!(migration_history_count(&migrated, 16), 1);
        let version: u32 = migrated
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("migrated version");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        let private_hits: u32 = migrated
            .query_row(
                "SELECT count(*) FROM search_documents_fts
                 WHERE search_documents_fts MATCH 'privatecachetoken'",
                [],
                |row| row.get(0),
            )
            .expect("private path query");
        assert_eq!(private_hits, 0);
        let filename_hits: u32 = migrated
            .query_row(
                "SELECT count(*) FROM search_documents_fts
                 WHERE search_documents_fts MATCH 'visible'",
                [],
                |row| row.get(0),
            )
            .expect("artifact name query");
        assert_eq!(filename_hits, 0);
        let migrated_documents: u32 = migrated
            .query_row(
                "SELECT count(*) FROM search_documents WHERE id='artifact-private'",
                [],
                |row| row.get(0),
            )
            .expect("migrated artifact search document count");
        assert_eq!(migrated_documents, 0);
        let migrated_artifact: (i64, Option<i64>) = migrated
            .query_row(
                "SELECT state,current_content_version FROM artifacts
                 WHERE id='artifact-private'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("migrated unavailable artifact");
        assert_eq!(migrated_artifact, (0, None));
        for trigger in ["artifacts_search_ai", "artifacts_search_au"] {
            let definition: String = migrated
                .query_row(
                    "SELECT sql FROM sqlite_master WHERE type='trigger' AND name=?1",
                    [trigger],
                    |row| row.get(0),
                )
                .expect("migrated trigger");
            assert!(!definition.contains("relative_path"));
        }
        drop(migrated);

        let restarted = open_encrypted(&path, &key).expect("reopen migrated schema");
        assert_eq!(migration_history_count(&restarted, 16), 1);
        assert_eq!(migration_history_count(&restarted, 17), 1);
        let restarted_documents: u32 = restarted
            .query_row(
                "SELECT count(*) FROM search_documents WHERE id='artifact-private'",
                [],
                |row| row.get(0),
            )
            .expect("restarted artifact search document count");
        assert_eq!(restarted_documents, 0);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn version_seventeen_discards_unqualified_content_and_restarts_after_rollback() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("artifact-content-migration.db");
        let key = DatabaseKey::from_slice(&[32; 32]).expect("key");
        let connection = open_encrypted(&path, &key).expect("create current schema");
        downgrade_empty_artifact_schema_to_v16(&connection);
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('content-project','Content project','',0,0,1,1);
                 INSERT INTO artifacts(
                     id,project_id,thread_id,name,relative_path,media_type,byte_size,
                     state,revision,created_at,updated_at
                 ) VALUES
                 ('legacy-available','content-project',NULL,'Available legacy.txt',
                  'untrusted/available.txt','text/plain',41,0,3,2,9),
                 ('legacy-deleted','content-project',NULL,'Deleted legacy.txt',
                  'untrusted/deleted.txt','text/plain',17,1,1,3,8);
                 INSERT INTO workspace_commands(
                     scope,idempotency_key,request_fingerprint,entity_id
                 ) VALUES (
                     'create_artifact','legacy-artifact-command',zeroblob(32),'legacy-available'
                 );
                 CREATE TABLE artifact_versions(blocker INTEGER) STRICT;",
            )
            .expect("construct schema sixteen artifact fixture");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());
        let rolled_back = Connection::open(&path).expect("inspect rolled-back migration");
        apply_encryption_key(&rolled_back, &key).expect("unlock rolled-back migration");
        let version: u32 = rolled_back
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 16);
        assert_eq!(migration_history_count(&rolled_back, 17), 0);
        let legacy_path: String = rolled_back
            .query_row(
                "SELECT relative_path FROM artifacts WHERE id='legacy-available'",
                [],
                |row| row.get(0),
            )
            .expect("legacy path survives rolled-back transaction");
        assert_eq!(legacy_path, "untrusted/available.txt");
        rolled_back
            .execute_batch("DROP TABLE artifact_versions;")
            .expect("remove migration blocker");
        drop(rolled_back);

        let migrated = open_encrypted(&path, &key).expect("restart schema seventeen migration");
        assert_eq!(migration_history_count(&migrated, 17), 1);
        let columns = migrated
            .prepare("SELECT name FROM pragma_table_info('artifacts') ORDER BY cid")
            .expect("artifact columns")
            .query_map([], |row| row.get::<_, String>(0))
            .expect("artifact column rows")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect artifact columns");
        assert_eq!(
            columns,
            [
                "id",
                "project_id",
                "thread_id",
                "name",
                "current_content_version",
                "state",
                "revision",
                "created_at",
                "updated_at",
            ]
        );
        let unavailable: (i64, Option<i64>, i64, i64, i64) = migrated
            .query_row(
                "SELECT state,current_content_version,revision,created_at,updated_at
                 FROM artifacts WHERE id='legacy-available'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("migrated unavailable artifact");
        assert_eq!(unavailable, (0, None, 0, 2, 2));
        let deleted: (i64, Option<i64>, i64, i64) = migrated
            .query_row(
                "SELECT state,current_content_version,revision,updated_at
                 FROM artifacts WHERE id='legacy-deleted'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("migrated deleted artifact");
        assert_eq!(deleted, (2, None, 1, 8));
        let version_rows: u32 = migrated
            .query_row("SELECT count(*) FROM artifact_versions", [], |row| {
                row.get(0)
            })
            .expect("version rows");
        assert_eq!(version_rows, 0);
        let version_parent: String = migrated
            .query_row(
                "SELECT \"table\" FROM pragma_foreign_key_list('artifact_versions')
                 WHERE \"from\"='artifact_id'",
                [],
                |row| row.get(0),
            )
            .expect("artifact version parent table");
        assert_eq!(version_parent, "artifacts");
        let foreign_key_violations: u32 = migrated
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .expect("foreign key check");
        assert_eq!(foreign_key_violations, 0);
        let artifact_documents: u32 = migrated
            .query_row(
                "SELECT count(*) FROM search_documents WHERE kind='artifact'",
                [],
                |row| row.get(0),
            )
            .expect("artifact search documents");
        assert_eq!(artifact_documents, 0);
        migrated
            .execute_batch(
                "BEGIN IMMEDIATE;
                 INSERT INTO artifacts(
                     id,project_id,thread_id,name,current_content_version,state,revision,
                     created_at,updated_at
                 ) VALUES ('immutable-artifact','content-project',NULL,'Immutable.txt',1,1,1,10,10);
                 INSERT INTO artifact_versions(
                     artifact_id,version,content_sha256,media_type,byte_size,created_at
                 ) VALUES ('immutable-artifact',1,zeroblob(32),'text/plain',5,10);
                 COMMIT;",
            )
            .expect("seed immutable artifact version");
        assert!(
            migrated
                .execute(
                    "UPDATE artifact_versions SET media_type='application/octet-stream'
                     WHERE artifact_id='immutable-artifact' AND version=1",
                    [],
                )
                .is_err(),
            "artifact versions must reject updates"
        );
        assert!(
            migrated
                .execute(
                    "DELETE FROM artifact_versions
                     WHERE artifact_id='immutable-artifact' AND version=1",
                    [],
                )
                .is_err(),
            "artifact versions must reject deletes"
        );
        let legacy_commands: u32 = migrated
            .query_row(
                "SELECT count(*) FROM workspace_commands
                 WHERE scope IN ('create_artifact','update_artifact','delete_artifact')",
                [],
                |row| row.get(0),
            )
            .expect("legacy artifact command count");
        assert_eq!(legacy_commands, 0);
        for table in ["artifact_ingestions", "artifact_open_commands"] {
            let exists: u32 = migrated
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |row| row.get(0),
                )
                .expect("journal table");
            assert_eq!(exists, 1, "missing {table}");
        }
        drop(migrated);

        let restarted = open_encrypted(&path, &key).expect("restart migrated database");
        assert_eq!(migration_history_count(&restarted, 17), 1);
    }

    #[test]
    fn version_seventeen_rejects_corrupt_legacy_lifecycle_before_rebuilding() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("artifact-content-corruption.db");
        let key = DatabaseKey::from_slice(&[33; 32]).expect("key");
        let connection = open_encrypted(&path, &key).expect("create current schema");
        downgrade_empty_artifact_schema_to_v16(&connection);
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('corrupt-project','Corrupt project','',0,0,1,1);
                 INSERT INTO artifacts(
                     id,project_id,thread_id,name,relative_path,media_type,byte_size,
                     state,revision,created_at,updated_at
                 ) VALUES (
                     'corrupt-artifact','corrupt-project',NULL,'Corrupt.txt',
                     'legacy/corrupt.txt','text/plain',1,9,0,1,1
                 );",
            )
            .expect("construct corrupt artifact fixture");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());
        let rolled_back = Connection::open(&path).expect("inspect rejected migration");
        apply_encryption_key(&rolled_back, &key).expect("unlock rejected migration");
        let version: u32 = rolled_back
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 16);
        assert_eq!(migration_history_count(&rolled_back, 17), 0);
        let relative_path_column: u32 = rolled_back
            .query_row(
                "SELECT count(*) FROM pragma_table_info('artifacts')
                 WHERE name='relative_path'",
                [],
                |row| row.get(0),
            )
            .expect("legacy column count");
        assert_eq!(relative_path_column, 1);
        rolled_back
            .execute(
                "UPDATE artifacts SET state=0 WHERE id='corrupt-artifact'",
                [],
            )
            .expect("repair corrupt lifecycle");
        drop(rolled_back);

        let migrated = open_encrypted(&path, &key).expect("migrate repaired artifact");
        assert_eq!(migration_history_count(&migrated, 17), 1);
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn version_eighteen_backfills_retention_and_restarts_after_rollback() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("artifact-retention-migration.db");
        let key = DatabaseKey::from_slice(&[34; 32]).expect("key");
        let connection = open_encrypted(&path, &key).expect("create current schema");
        downgrade_artifact_retention_schema_to_v17(&connection);
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('retention-project','Retention project','',0,0,1,1);
                 BEGIN IMMEDIATE;
                 INSERT INTO artifacts(
                     id,project_id,thread_id,name,current_content_version,state,revision,
                     created_at,updated_at
                 ) VALUES (
                     'retention-artifact','retention-project',NULL,'Retained.txt',1,1,1,10,10
                 );
                 INSERT INTO artifact_versions(
                     artifact_id,version,content_sha256,media_type,byte_size,created_at
                 ) VALUES ('retention-artifact',1,zeroblob(32),'text/plain',41,10);
                 COMMIT;
                 CREATE TABLE artifact_version_retention(blocker INTEGER) STRICT;",
            )
            .expect("construct version seventeen retention fixture and blocker");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());
        let rolled_back = Connection::open(&path).expect("inspect rolled-back migration");
        apply_encryption_key(&rolled_back, &key).expect("unlock rolled-back migration");
        let version: u32 = rolled_back
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 17);
        assert_eq!(migration_history_count(&rolled_back, 18), 0);
        let version_bytes: i64 = rolled_back
            .query_row(
                "SELECT byte_size FROM artifact_versions
                 WHERE artifact_id='retention-artifact' AND version=1",
                [],
                |row| row.get(0),
            )
            .expect("legacy immutable version survives rollback");
        assert_eq!(version_bytes, 41);
        let removal_table: u32 = rolled_back
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='table' AND name='artifact_removal_commands'",
                [],
                |row| row.get(0),
            )
            .expect("rolled-back removal table count");
        assert_eq!(removal_table, 0);
        rolled_back
            .execute_batch("DROP TABLE artifact_version_retention;")
            .expect("remove migration blocker");
        drop(rolled_back);

        let migrated = open_encrypted(&path, &key).expect("restart schema eighteen migration");
        assert_eq!(migration_history_count(&migrated, 18), 1);
        let retention: (i64, i64, i64, i64, Option<i64>) = migrated
            .query_row(
                "SELECT state,revision,created_at,updated_at,purged_at
                 FROM artifact_version_retention
                 WHERE artifact_id='retention-artifact' AND content_version=1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("backfilled retention record");
        assert_eq!(retention, (0, 0, 10, 10, None));
        assert!(
            migrated
                .execute(
                    "DELETE FROM artifact_version_retention
                     WHERE artifact_id='retention-artifact' AND content_version=1",
                    [],
                )
                .is_err(),
            "retention records must be immutable"
        );
        assert!(
            migrated
                .execute(
                    "UPDATE artifact_version_retention
                     SET state=2,revision=2,updated_at=11,purged_at=11
                     WHERE artifact_id='retention-artifact' AND content_version=1",
                    [],
                )
                .is_err(),
            "retention transitions must not skip durable purge intent"
        );
        drop(migrated);

        let restarted = open_encrypted(&path, &key).expect("reopen migrated retention schema");
        assert_eq!(migration_history_count(&restarted, 18), 1);
        let foreign_key_violations: u32 = restarted
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
                row.get(0)
            })
            .expect("foreign key check");
        assert_eq!(foreign_key_violations, 0);
    }

    #[test]
    fn version_eighteen_rejects_orphan_version_before_retention_backfill() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("artifact-retention-corruption.db");
        let key = DatabaseKey::from_slice(&[35; 32]).expect("key");
        let connection = open_encrypted(&path, &key).expect("create current schema");
        downgrade_artifact_retention_schema_to_v17(&connection);
        connection
            .execute_batch(
                "PRAGMA foreign_keys=OFF;
                 INSERT INTO artifact_versions(
                     artifact_id,version,content_sha256,media_type,byte_size,created_at
                 ) VALUES ('orphan-artifact',1,zeroblob(32),'text/plain',1,1);
                 PRAGMA foreign_keys=ON;",
            )
            .expect("construct corrupt version seventeen fixture");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());
        let rolled_back = Connection::open(&path).expect("inspect rejected migration");
        apply_encryption_key(&rolled_back, &key).expect("unlock rejected migration");
        let version: u32 = rolled_back
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("rejected schema version");
        assert_eq!(version, 17);
        assert_eq!(migration_history_count(&rolled_back, 18), 0);
        let retention_table: u32 = rolled_back
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='table' AND name='artifact_version_retention'",
                [],
                |row| row.get(0),
            )
            .expect("retention table count");
        assert_eq!(retention_table, 0);
        let orphan_count: u32 = rolled_back
            .query_row(
                "SELECT count(*) FROM artifact_versions
                 WHERE artifact_id='orphan-artifact'",
                [],
                |row| row.get(0),
            )
            .expect("orphan version survives rejected migration");
        assert_eq!(orphan_count, 1);
    }

    #[test]
    fn version_fourteen_rolls_back_then_continues_without_rewriting_schema_thirteen_rows() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("conversation-fork-migration.db");
        let key = DatabaseKey::from_slice(&[28; 32]).expect("key");
        let mut connection = create_version_eleven_database(&path, &key);
        insert_version_eleven_completed_turn(&connection, "legacy fork source");
        migrate_fixture_to_version_twelve(&mut connection);
        migrate_fixture_to_version_thirteen(&mut connection);
        connection
            .execute_batch("CREATE TABLE conversation_thread_forks(blocker INTEGER) STRICT;")
            .expect("install migration blocker");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());
        let connection = Connection::open(&path).expect("inspect rolled-back migration");
        connection
            .execute_batch(&format!(
                "PRAGMA key = \"x'{}'\";",
                hex::encode(key.expose_secret())
            ))
            .expect("unlock rolled-back migration");
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 13);
        assert_eq!(migration_history_count(&connection, 14), 0);
        let lineage: (i64, Option<String>, Option<String>, i64) = connection
            .query_row(
                "SELECT origin,source_turn_id,credential_binding_id,retry_depth
                 FROM conversation_turn_lineage WHERE turn_id='event-turn'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("schema thirteen lineage after rollback");
        assert_eq!(lineage, (0, None, None, 0));
        let immutability_trigger: u32 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='trigger'
                   AND name='conversation_turn_lineage_immutable_update'",
                [],
                |row| row.get(0),
            )
            .expect("schema thirteen trigger after rollback");
        assert_eq!(immutability_trigger, 1);
        connection
            .execute_batch("DROP TABLE conversation_thread_forks;")
            .expect("remove migration blocker");
        drop(connection);

        let restarted = open_encrypted(&path, &key).expect("restart schema fourteen migration");
        assert_eq!(migration_history_count(&restarted, 14), 1);
        assert_eq!(migration_history_count(&restarted, 15), 1);
        let version: u32 = restarted
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("restarted version");
        assert_eq!(version, LATEST_SCHEMA_VERSION);
        let preserved: (i64, Option<String>, Option<String>, i64) = restarted
            .query_row(
                "SELECT origin,source_turn_id,credential_binding_id,retry_depth
                 FROM conversation_turn_lineage WHERE turn_id='event-turn'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("preserved schema thirteen lineage");
        assert_eq!(preserved, lineage);
        for table in [
            "conversation_thread_forks",
            "conversation_message_derivations",
            "conversation_inherited_assistant_outcomes",
            "conversation_fork_commands",
            "conversation_fork_deliveries",
            "conversation_fork_delivery_aliases",
            "conversation_fork_delivery_ack_commands",
        ] {
            let rows: u32 = restarted
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("empty schema fourteen side table");
            assert_eq!(rows, 0, "legacy rows leaked into {table}");
        }
    }

    #[test]
    fn new_thread_identity_binds_once_and_survives_restart() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("new-thread-identity.db");
        let key = DatabaseKey::from_slice(&[27; 32]).expect("key");
        let connection = open_encrypted(&path, &key).expect("latest schema");
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('identity-project','Identity','',0,0,1,1);
                 INSERT INTO threads(
                     id,project_id,title,state,revision,created_at,updated_at
                 ) VALUES (
                     'empty-thread','identity-project','Empty',0,0,1,1
                 );",
            )
            .expect("insert post-migration thread");
        let empty_binding: Option<String> = connection
            .query_row(
                "SELECT credential_binding_id FROM conversation_thread_identity
                 WHERE thread_id='empty-thread'",
                [],
                |row| row.get(0),
            )
            .expect("new thread identity");
        assert_eq!(empty_binding, None);
        assert_eq!(
            connection
                .execute(
                    "UPDATE conversation_thread_identity
                     SET credential_binding_id='first-local-generation'
                     WHERE thread_id='empty-thread'",
                    [],
                )
                .expect("bind empty thread once"),
            1
        );
        assert!(
            connection
                .execute(
                    "UPDATE conversation_thread_identity
                     SET credential_binding_id='replacement-local-generation'
                     WHERE thread_id='empty-thread'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM conversation_thread_identity
                     WHERE thread_id='empty-thread'",
                    [],
                )
                .is_err()
        );
        drop(connection);

        let restarted = open_encrypted(&path, &key).expect("restart identity schema");
        let identity: (i64, Option<String>) = restarted
            .query_row(
                "SELECT source,credential_binding_id
                 FROM conversation_thread_identity WHERE thread_id='empty-thread'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("restarted thread identity");
        assert_eq!(identity, (0, Some("first-local-generation".into())));
    }

    #[test]
    fn migration_seven_rolls_back_schema_and_version_together() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.db");
        let key = DatabaseKey::from_slice(&[8; 32]).expect("key");
        let connection = create_version_six_database(&path, &key);
        connection
            .execute_batch("CREATE TABLE privileged_operations(conflict TEXT) STRICT;")
            .expect("migration conflict fixture");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());

        let connection = Connection::open(&path).expect("inspect failed migration");
        let key_hex = hex::encode(key.expose_secret());
        connection
            .execute_batch(&format!("PRAGMA key = \"x'{key_hex}'\";"))
            .expect("unlock database");
        let version: u32 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 6);
        let history: u32 = connection
            .query_row(
                "SELECT count(*) FROM schema_migrations WHERE version=7",
                [],
                |row| row.get(0),
            )
            .expect("migration history");
        assert_eq!(history, 0);
        let partially_created_index: u32 = connection
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='index' AND name='side_effects_identity_run'",
                [],
                |row| row.get(0),
            )
            .expect("rolled back index");
        assert_eq!(partially_created_index, 0);
    }

    #[test]
    fn privileged_operation_kinds_targets_retry_policy_and_identity_are_closed() {
        let (_directory, mut connection) = open_test_database(9);
        for kind in 0..=5 {
            insert_prepared(&mut connection, &format!("operation-{kind}"), kind)
                .expect("valid operation shape");
        }

        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations SET retry_class=1 WHERE id='operation-0'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET target_integration_id='not-allowed'
                     WHERE id='operation-0'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET target_observation_revision=0
                     WHERE id='operation-5'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations SET target_vm_id='unsafe/path'
                     WHERE id='operation-0'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations SET request_digest=zeroblob(31)
                     WHERE id='operation-0'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations SET supersedes_id=id
                     WHERE id='operation-0'",
                    [],
                )
                .is_err()
        );
        assert!(
            insert_prepared_with_identity(
                &mut connection,
                "duplicate-operation",
                0,
                "authority-grant-operation-0",
                "idempotency-key-operation-0",
                (None, None, None, None),
            )
            .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET state=2, attempt_count=1, last_attempt_sequence=1,
                         last_attempt_certainty=4, revision=1, updated_at=101
                     WHERE id='operation-1'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn privileged_states_require_their_attempt_result_and_retention_records() {
        let (_directory, mut connection) = open_test_database(14);
        insert_prepared(&mut connection, "state-operation", 0).expect("operation");

        assert!(
            connection
                .execute(
                    "INSERT INTO privileged_operations(
                         id, operation_kind, retry_class, target_vm_id,
                         payload_digest, retained_payload_digest, authority_grant_id,
                         authority_expires_at, idempotency_key, request_digest,
                         state, attempt_count, revision, created_at, updated_at
                     ) VALUES (
                         'direct-cancelled', 0, 0, 'vm-primary', zeroblob(32), NULL,
                         'authority-direct-cancelled', 1000,
                         'idempotency-direct-cancelled', zeroblob(32),
                         7, 0, 1, 100, 101
                     )",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET state=3, revision=1, updated_at=101
                     WHERE id='state-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET state=7, revision=1, updated_at=101
                     WHERE id='state-operation'",
                    [],
                )
                .is_err()
        );
        connection
            .execute(
                "UPDATE privileged_operations
                 SET state=7, retained_payload_digest=NULL, revision=1, updated_at=101
                 WHERE id='state-operation'",
                [],
            )
            .expect("consistent cancellation");
        connection
            .execute(
                "DELETE FROM privileged_operation_payloads
                 WHERE operation_id='state-operation'",
                [],
            )
            .expect("cancelled payload cleanup");
        assert!(
            connection
                .execute(
                    "DELETE FROM privileged_operations WHERE id='state-operation'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn privileged_intent_identity_authority_target_and_digest_are_immutable() {
        let (_directory, mut connection) = open_test_database(19);
        for id in ["target-operation", "key-operation", "digest-operation"] {
            insert_prepared(&mut connection, id, 0).expect("operation");
        }

        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET state=7, retained_payload_digest=NULL, revision=1, updated_at=101,
                         target_vm_id='vm-secondary'
                     WHERE id='target-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET state=7, retained_payload_digest=NULL, revision=1, updated_at=101,
                         idempotency_key='different-idempotency-key'
                     WHERE id='key-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET state=7, retained_payload_digest=NULL, revision=1, updated_at=101,
                         payload_digest=zeroblob(32), authority_expires_at=2000
                     WHERE id='digest-operation'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn privileged_supersession_requires_an_existing_reviewed_operation() {
        let (_directory, mut connection) = open_test_database(20);
        insert_prepared(&mut connection, "unreviewed-operation", 1).expect("operation");
        assert!(
            insert_prepared_with_identity(
                &mut connection,
                "invalid-replacement",
                1,
                "authority-invalid-replacement",
                "idempotency-invalid-replacement",
                (None, None, None, Some("unreviewed-operation")),
            )
            .is_err()
        );
    }

    #[test]
    fn privileged_links_require_a_run_and_match_the_owned_effect_and_approval() {
        let (_directory, mut connection) = open_test_database(10);
        connection
            .execute_batch(
                "INSERT INTO runs(
                     id, project_id, thread_id, state, revision, created_at, updated_at
                 ) VALUES
                     ('run-one', 'project', 'thread', 0, 0, 1, 1),
                     ('run-two', 'project', 'thread', 0, 0, 1, 1);
                 INSERT INTO side_effects(
                     id, run_id, kind, target, idempotency, state, revision,
                     created_at, updated_at
                 ) VALUES ('effect-one', 'run-one', 0, 'target', 1, 0, 0, 1, 1);
                 INSERT INTO approvals(
                     id, run_id, action, target, data_summary, risk, scope,
                     status, revision, created_at, expires_at
                 ) VALUES (
                     'approval-one', 'run-one', 'action', 'target', 'summary',
                     1, 0, 0, 0, 1, 100
                 );",
            )
            .expect("link owners");
        assert!(
            insert_prepared_with_identity(
                &mut connection,
                "missing-run-operation",
                1,
                "authority-missing-run",
                "idempotency-missing-run",
                (None, Some("effect-one"), None, None),
            )
            .is_err()
        );
        assert!(
            insert_prepared_with_identity(
                &mut connection,
                "wrong-run-operation",
                1,
                "authority-wrong-run",
                "idempotency-wrong-run",
                (Some("run-two"), Some("effect-one"), None, None),
            )
            .is_err()
        );
        insert_prepared_with_identity(
            &mut connection,
            "linked-operation",
            1,
            "authority-linked-operation",
            "idempotency-linked-operation",
            (
                Some("run-one"),
                Some("effect-one"),
                Some("approval-one"),
                None,
            ),
        )
        .expect("owned links");
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET state=7, retained_payload_digest=NULL, revision=1,
                         updated_at=101, approval_id=NULL
                     WHERE id='linked-operation'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn privileged_request_payload_is_bounded_immutable_and_retained_until_terminal() {
        let (_directory, mut connection) = open_test_database(11);
        insert_prepared(&mut connection, "payload-operation", 0).expect("operation");

        assert!(
            connection
                .execute(
                    "UPDATE privileged_operation_payloads SET payload=zeroblob(8388609)
                     WHERE operation_id='payload-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operation_payloads SET payload=x'00'
                     WHERE operation_id='payload-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operation_payloads SET payload=x'7b2278223a317d'
                     WHERE operation_id='payload-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operation_payloads SET payload_digest=zeroblob(31)
                     WHERE operation_id='payload-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "DELETE FROM privileged_operation_payloads
                     WHERE operation_id='payload-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET terminal_result_digest=zeroblob(32),
                         terminal_result_payload=x'7b7d', revision=1, updated_at=101
                     WHERE id='payload-operation'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn privileged_terminal_payloads_prune_to_permanent_digest_tombstones() {
        let (_directory, mut connection) = open_test_database(15);
        insert_prepared(&mut connection, "terminal-operation", 0).expect("operation");
        dispatch_first_attempt(
            &mut connection,
            "terminal-operation",
            "transport-terminal-0001",
        )
        .expect("dispatch");
        complete_first_attempt(
            &mut connection,
            "terminal-operation",
            3,
            2,
            Some(&[41_u8; 32]),
            None,
        )
        .expect("known success");
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations SET terminal_result_payload=x'00'
                     WHERE id='terminal-operation'",
                    [],
                )
                .is_err()
        );
        connection
            .execute(
                "DELETE FROM privileged_operation_payloads
                 WHERE operation_id='terminal-operation'",
                [],
            )
            .expect("prune terminal request payload");
        assert!(
            connection
                .execute(
                    "INSERT INTO privileged_operation_payloads(
                         operation_id, payload_digest, payload, created_at
                     ) VALUES (
                         'terminal-operation', ?1, '{}', 100
                     )",
                    [&[11_u8; 32][..]],
                )
                .is_err()
        );
        connection
            .execute(
                "UPDATE privileged_operations
                 SET terminal_result_payload=NULL, terminal_result_pruned=1,
                     revision=3, updated_at=103
                 WHERE id='terminal-operation'",
                [],
            )
            .expect("retain a digest tombstone");
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET terminal_result_payload=x'726573746f726564',
                         terminal_result_pruned=0, revision=4, updated_at=104
                     WHERE id='terminal-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET terminal_result_digest=zeroblob(32),
                         revision=4, updated_at=104
                     WHERE id='terminal-operation'",
                    [],
                )
                .is_err()
        );
        assert_terminal_tombstone(&connection, "terminal-operation");
        assert!(
            connection
                .execute(
                    "DELETE FROM privileged_operations WHERE id='terminal-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            insert_prepared_with_identity(
                &mut connection,
                "replacement-with-old-key",
                0,
                "authority-grant-terminal-operation",
                "idempotency-key-terminal-operation",
                (None, None, None, None),
            )
            .is_err()
        );
    }

    #[test]
    fn privileged_attempt_completion_requires_consistent_immutable_evidence() {
        let (_directory, mut connection) = open_test_database(12);
        insert_prepared(&mut connection, "attempt-operation-one", 0).expect("operation one");
        dispatch_first_attempt(
            &mut connection,
            "attempt-operation-one",
            "transport-shared-0001",
        )
        .expect("first dispatch");

        assert!(
            connection
                .execute(
                    "UPDATE privileged_operation_attempts SET outcome_certainty=2
                     WHERE operation_id='attempt-operation-one' AND sequence=1",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operation_attempts SET broker_boot_id=zeroblob(15)
                     WHERE operation_id='attempt-operation-one' AND sequence=1",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operation_attempts SET wire_digest=zeroblob(31)
                     WHERE operation_id='attempt-operation-one' AND sequence=1",
                    [],
                )
                .is_err()
        );

        complete_first_attempt(
            &mut connection,
            "attempt-operation-one",
            3,
            2,
            Some(&[42_u8; 32]),
            None,
        )
        .expect("valid known success");
    }

    #[test]
    fn privileged_attempt_deadlines_and_transport_ids_are_bounded_and_unique() {
        let (_directory, mut connection) = open_test_database(16);
        insert_prepared(&mut connection, "attempt-operation-one", 0).expect("operation one");
        insert_prepared(&mut connection, "attempt-operation-two", 0).expect("operation two");
        dispatch_first_attempt(
            &mut connection,
            "attempt-operation-one",
            "transport-shared-0001",
        )
        .expect("first dispatch");

        let transaction = connection.transaction().expect("transaction");
        stage_first_dispatch(&transaction, "attempt-operation-two").expect("second dispatch state");
        assert!(
            insert_dispatching_attempt(
                &transaction,
                "attempt-operation-two",
                "transport-shared-0001",
                &[1_u8; 32],
                &[2_u8; 16],
                &[3_u8; 16],
                201,
            )
            .is_err()
        );
        transaction
            .rollback()
            .expect("rollback duplicate transport");

        let transaction = connection.transaction().expect("transaction");
        stage_first_dispatch(&transaction, "attempt-operation-two").expect("second dispatch state");
        assert!(
            insert_dispatching_attempt(
                &transaction,
                "attempt-operation-two",
                "transport-second-0001",
                &[1_u8; 32],
                &[2_u8; 15],
                &[3_u8; 16],
                201,
            )
            .is_err()
        );
        transaction.rollback().expect("rollback invalid epoch");

        let transaction = connection.transaction().expect("transaction");
        stage_first_dispatch(&transaction, "attempt-operation-two").expect("second dispatch state");
        assert!(
            insert_dispatching_attempt(
                &transaction,
                "attempt-operation-two",
                "transport-second-0002",
                &[1_u8; 32],
                &[2_u8; 16],
                &[3_u8; 16],
                30102,
            )
            .is_err()
        );
        transaction.rollback().expect("rollback excessive deadline");

        let transaction = connection.transaction().expect("transaction");
        stage_first_dispatch(&transaction, "attempt-operation-two").expect("second dispatch state");
        assert!(
            insert_dispatching_attempt(
                &transaction,
                "attempt-operation-two",
                "short",
                &[1_u8; 32],
                &[2_u8; 16],
                &[3_u8; 16],
                201,
            )
            .is_err()
        );
        transaction.rollback().expect("rollback short transport id");
    }

    #[test]
    fn privileged_attempt_epoch_evidence_is_nonzero_and_distinct_from_the_operation() {
        let (_directory, mut connection) = open_test_database(17);
        insert_prepared(&mut connection, "attempt-operation-two", 0).expect("operation two");
        for (transport, broker_boot, guest_boot, deadline) in [
            (
                "transport-zero-broker",
                &[0_u8; 16][..],
                &[3_u8; 16][..],
                201,
            ),
            (
                "transport-zero-guest",
                &[2_u8; 16][..],
                &[0_u8; 16][..],
                201,
            ),
            (
                "attempt-operation-two",
                &[2_u8; 16][..],
                &[3_u8; 16][..],
                201,
            ),
            (
                "transport-zero-duration",
                &[2_u8; 16][..],
                &[3_u8; 16][..],
                101,
            ),
        ] {
            let transaction = connection.transaction().expect("transaction");
            stage_first_dispatch(&transaction, "attempt-operation-two")
                .expect("second dispatch state");
            assert!(
                insert_dispatching_attempt(
                    &transaction,
                    "attempt-operation-two",
                    transport,
                    &[1_u8; 32],
                    broker_boot,
                    guest_boot,
                    deadline,
                )
                .is_err()
            );
            transaction
                .rollback()
                .expect("rollback invalid epoch evidence");
        }
    }

    #[test]
    fn privileged_review_requires_uncertain_non_idempotent_history() {
        let (_directory, mut connection) = open_test_database(13);
        insert_prepared(&mut connection, "premature-review-operation", 1).expect("operation");
        assert!(
            connection
                .execute(
                    "INSERT INTO privileged_operation_reviews(
                         operation_id, disposition, operation_revision, reviewed_at,
                         actor_id, rationale
                     ) VALUES (
                         'premature-review-operation', 2, 1, 101,
                         'local-user', 'Not reviewed'
                     )",
                    [],
                )
                .is_err()
        );

        prepare_uncertain_operation(&mut connection, "review-operation", "transport-review-0001");
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET state=1, attempt_count=2, last_attempt_sequence=2,
                         last_attempt_certainty=0, revision=3, updated_at=103
                     WHERE id='review-operation'",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operations
                     SET state=6, review_disposition=2, retained_payload_digest=NULL,
                         revision=3, updated_at=103
                     WHERE id='review-operation'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn privileged_review_rejects_self_replacement_and_oversized_rationale() {
        let (_directory, mut connection) = open_test_database(17);
        prepare_uncertain_operation(&mut connection, "review-operation", "transport-review-0002");
        let transaction = connection.transaction().expect("transaction");
        stage_review(&transaction, "review-operation").expect("review state");
        assert!(
            transaction
                .execute(
                    "INSERT INTO privileged_operation_reviews(
                         operation_id, disposition, operation_revision, reviewed_at,
                         actor_id, rationale, replacement_operation_id
                     ) VALUES (
                         'review-operation', 2, 3, 103, 'local-user',
                         'Invalid self replacement', 'review-operation'
                     )",
                    [],
                )
                .is_err()
        );
        transaction.rollback().expect("rollback self replacement");

        let transaction = connection.transaction().expect("transaction");
        stage_review(&transaction, "review-operation").expect("review state");
        assert!(
            transaction
                .execute(
                    "INSERT INTO privileged_operation_reviews(
                         operation_id, disposition, operation_revision, reviewed_at,
                         actor_id, rationale
                     ) VALUES ('review-operation', 2, 3, 103, 'unsafe/actor', 'Reviewed')",
                    [],
                )
                .is_err()
        );
        transaction.rollback().expect("rollback unsafe actor");

        let transaction = connection.transaction().expect("transaction");
        stage_review(&transaction, "review-operation").expect("review state");
        assert!(
            transaction
                .execute(
                    "INSERT INTO privileged_operation_reviews(
                         operation_id, disposition, operation_revision, reviewed_at,
                         actor_id, rationale
                     ) VALUES ('review-operation', 2, 3, 103, 'local-user', ?1)",
                    ["x".repeat(4097)],
                )
                .is_err()
        );
        transaction.rollback().expect("rollback oversized review");
    }

    #[test]
    fn privileged_review_replacement_has_backlinked_lineage_and_prunable_payload() {
        let (_directory, mut connection) = open_test_database(18);
        prepare_uncertain_operation(&mut connection, "review-operation", "transport-review-0003");
        insert_prepared(&mut connection, "unrelated-operation", 1).expect("unrelated operation");

        let transaction = connection.transaction().expect("transaction");
        stage_review(&transaction, "review-operation").expect("review state");
        assert!(
            transaction
                .execute(
                    "INSERT INTO privileged_operation_reviews(
                         operation_id, disposition, operation_revision, reviewed_at,
                         actor_id, rationale, replacement_operation_id
                     ) VALUES (
                         'review-operation', 2, 3, 103, 'local-user',
                         'Unrelated replacement', 'unrelated-operation'
                     )",
                    [],
                )
                .is_err()
        );
        transaction
            .rollback()
            .expect("rollback unrelated replacement");

        let transaction = connection.transaction().expect("transaction");
        stage_review(&transaction, "review-operation").expect("review state");
        insert_prepared_record(
            &transaction,
            "replacement-operation",
            1,
            "authority-replacement-operation",
            "idempotency-replacement-operation",
            (None, None, None, Some("review-operation")),
        )
        .expect("backlinked replacement");
        transaction
            .execute(
                "INSERT INTO privileged_operation_reviews(
                     operation_id, disposition, operation_revision, reviewed_at,
                     actor_id, rationale, replacement_operation_id
                 ) VALUES (
                     'review-operation', 2, 3, 103, 'local-user',
                     'User abandoned recovery', 'replacement-operation'
                 )",
                [],
            )
            .expect("review record");
        transaction.commit().expect("commit review");
        connection
            .execute(
                "DELETE FROM privileged_operation_payloads
                 WHERE operation_id='review-operation'",
                [],
            )
            .expect("prune reviewed request payload");
        assert!(
            connection
                .execute(
                    "UPDATE privileged_operation_reviews SET rationale='changed'
                     WHERE operation_id='review-operation'",
                    [],
                )
                .is_err()
        );
    }

    #[test]
    fn version_nineteen_normalizes_supported_schedules_without_starting_scheduler_state() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("automation-scheduler-migration.db");
        let key = DatabaseKey::from_slice(&[41; 32]).expect("key");
        let connection = open_encrypted(&path, &key).expect("create current schema");
        downgrade_scheduler_schema_to_v18(&connection);
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('scheduler-project','Scheduler project','',0,0,1,1);
                 INSERT INTO automations(
                     id,project_id,title,prompt,schedule,timezone,missed_run_policy,
                     overlap_policy,state,revision,created_at,updated_at
                 ) VALUES
                 ('json-automation','scheduler-project','JSON automation','Prompt',
                  '{\"frequency\":\"daily\",\"localTime\":\"09:05\",\"timeZoneIana\":\"UTC\"}',
                  'UTC',0,0,1,0,1,1),
                 ('cron-automation','scheduler-project','Cron automation','Prompt',
                  '30 8 * * 1-5','Europe/Paris',1,1,1,0,2,2);
                 INSERT INTO automation_history(
                     automation_id,sequence,scheduled_for,recorded_at,status,summary
                 ) VALUES ('json-automation',1,3,4,2,'Skipped before scheduler migration');",
            )
            .expect("version eighteen automation fixture");
        drop(connection);

        let migrated = open_encrypted(&path, &key).expect("migrate scheduler schema");
        let schedules = migrated
            .prepare("SELECT id,schedule FROM automations ORDER BY id")
            .expect("schedule query")
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .expect("schedule rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("normalized schedules");
        assert_eq!(
            schedules,
            vec![
                ("cron-automation".into(), "v1;weekdays;08:30".into()),
                ("json-automation".into(), "v1;daily;09:05".into()),
            ]
        );
        for table in [
            "automation_scheduler_lease",
            "automation_schedule_cursors",
            "automation_schedule_evaluation_commands",
            "automation_occurrences",
            "automation_occurrence_claim_attempts",
        ] {
            let count: u32 = migrated
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .expect("empty scheduler journal");
            assert_eq!(count, 0, "migration populated {table}");
        }
        assert_eq!(migration_history_count(&migrated, 19), 1);
        drop(migrated);

        let restarted = open_encrypted(&path, &key).expect("restart migrated scheduler schema");
        assert_eq!(migration_history_count(&restarted, 19), 1);
    }

    #[test]
    fn version_nineteen_restarts_after_schema_collision_without_partial_rewrite() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("automation-scheduler-restart.db");
        let key = DatabaseKey::from_slice(&[45; 32]).expect("key");
        let connection = open_encrypted(&path, &key).expect("create current schema");
        downgrade_scheduler_schema_to_v18(&connection);
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('scheduler-project','Scheduler project','',0,0,1,1);
                 INSERT INTO automations(
                     id,project_id,title,prompt,schedule,timezone,missed_run_policy,
                     overlap_policy,state,revision,created_at,updated_at
                 ) VALUES (
                     'restart-automation','scheduler-project','Restart automation','Prompt',
                     '5 9 * * *','UTC',0,0,1,0,1,1
                 );
                 CREATE TABLE automation_scheduler_lease(blocker INTEGER) STRICT;",
            )
            .expect("version eighteen collision fixture");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());
        let rolled_back = Connection::open(&path).expect("inspect rolled-back migration");
        apply_encryption_key(&rolled_back, &key).expect("unlock rolled-back migration");
        let version: u32 = rolled_back
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("rolled-back version");
        assert_eq!(version, 18);
        assert_eq!(migration_history_count(&rolled_back, 19), 0);
        let schedule: String = rolled_back
            .query_row(
                "SELECT schedule FROM automations WHERE id='restart-automation'",
                [],
                |row| row.get(0),
            )
            .expect("unmodified legacy schedule");
        assert_eq!(schedule, "5 9 * * *");
        rolled_back
            .execute_batch("DROP TABLE automation_scheduler_lease;")
            .expect("remove collision blocker");
        drop(rolled_back);

        let restarted = open_encrypted(&path, &key).expect("restart scheduler migration");
        let normalized: String = restarted
            .query_row(
                "SELECT schedule FROM automations WHERE id='restart-automation'",
                [],
                |row| row.get(0),
            )
            .expect("normalized schedule");
        assert_eq!(normalized, "v1;daily;09:05");
        assert_eq!(migration_history_count(&restarted, 19), 1);
    }

    #[test]
    fn version_nineteen_rejects_enabled_legacy_definition_atomically() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("automation-scheduler-rejection.db");
        let key = DatabaseKey::from_slice(&[42; 32]).expect("key");
        let connection = open_encrypted(&path, &key).expect("create current schema");
        downgrade_scheduler_schema_to_v18(&connection);
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('scheduler-project','Scheduler project','',0,0,1,1);
                 INSERT INTO automations(
                     id,project_id,title,prompt,schedule,timezone,missed_run_policy,
                     overlap_policy,state,revision,created_at,updated_at
                 ) VALUES (
                     'enabled-legacy','scheduler-project','Enabled legacy','Prompt',
                     '5 9 * * *','UTC',0,0,0,0,1,1
                 );",
            )
            .expect("enabled version eighteen fixture");
        drop(connection);

        assert!(open_encrypted(&path, &key).is_err());
        let rejected = Connection::open(&path).expect("inspect rejected migration");
        apply_encryption_key(&rejected, &key).expect("unlock rejected migration");
        let version: u32 = rejected
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("schema version");
        assert_eq!(version, 18);
        assert_eq!(migration_history_count(&rejected, 19), 0);
        let schedule: String = rejected
            .query_row(
                "SELECT schedule FROM automations WHERE id='enabled-legacy'",
                [],
                |row| row.get(0),
            )
            .expect("legacy schedule");
        assert_eq!(schedule, "5 9 * * *");
        let scheduler_tables: u32 = rejected
            .query_row(
                "SELECT count(*) FROM sqlite_master
                 WHERE type='table' AND name='automation_scheduler_lease'",
                [],
                |row| row.get(0),
            )
            .expect("scheduler table count");
        assert_eq!(scheduler_tables, 0);
    }

    #[test]
    fn version_nineteen_corrupt_legacy_rows_roll_back_without_partial_normalization() {
        let cases = [
            (
                "schedule",
                "UPDATE automations SET schedule='unsupported schedule' WHERE id='z-corrupt'",
            ),
            (
                "timezone",
                "UPDATE automations SET timezone='Mars/Olympus' WHERE id='z-corrupt'",
            ),
            (
                "missed-policy",
                "UPDATE automations SET missed_run_policy=9 WHERE id='z-corrupt'",
            ),
            (
                "overlap-policy",
                "UPDATE automations SET overlap_policy=9 WHERE id='z-corrupt'",
            ),
            (
                "state",
                "UPDATE automations SET state=9 WHERE id='z-corrupt'",
            ),
            (
                "text-bounds",
                "UPDATE automations SET title='' WHERE id='z-corrupt'",
            ),
            (
                "gapped-history",
                "INSERT INTO automation_history(
                     automation_id,sequence,scheduled_for,recorded_at,status,summary
                 ) VALUES ('z-corrupt',2,2,3,0,'gapped')",
            ),
        ];
        for (case, corruption) in cases {
            let directory = tempfile::tempdir().expect("tempdir");
            let path = directory.path().join(format!("scheduler-{case}.db"));
            let key = DatabaseKey::from_slice(&[44; 32]).expect("key");
            let connection = open_encrypted(&path, &key).expect("create current schema");
            downgrade_scheduler_schema_to_v18(&connection);
            connection
                .execute_batch(
                    "INSERT INTO projects(
                         id,name,description,state,revision,created_at,updated_at
                     ) VALUES ('scheduler-project','Scheduler project','',0,0,1,1);
                     INSERT INTO automations(
                         id,project_id,title,prompt,schedule,timezone,missed_run_policy,
                         overlap_policy,state,revision,created_at,updated_at
                     ) VALUES
                     ('a-control','scheduler-project','Control','Prompt','5 9 * * *','UTC',
                      0,0,1,0,1,1),
                     ('z-corrupt','scheduler-project','Corrupt','Prompt','5 9 * * *','UTC',
                      0,0,1,0,1,1);",
                )
                .expect("version eighteen corruption fixture");
            connection
                .execute_batch(corruption)
                .expect("inject representable corruption");
            drop(connection);

            assert!(open_encrypted(&path, &key).is_err(), "accepted {case}");
            let rejected = Connection::open(&path).expect("inspect rejected migration");
            apply_encryption_key(&rejected, &key).expect("unlock rejected migration");
            let version: u32 = rejected
                .query_row("PRAGMA user_version", [], |row| row.get(0))
                .expect("schema version");
            assert_eq!(version, 18, "advanced schema for {case}");
            assert_eq!(
                migration_history_count(&rejected, 19),
                0,
                "history for {case}"
            );
            let control_schedule: String = rejected
                .query_row(
                    "SELECT schedule FROM automations WHERE id='a-control'",
                    [],
                    |row| row.get(0),
                )
                .expect("control schedule");
            assert_eq!(control_schedule, "5 9 * * *", "partial rewrite for {case}");
            let scheduler_tables: u32 = rejected
                .query_row(
                    "SELECT count(*) FROM sqlite_master
                     WHERE type='table' AND name='automation_scheduler_lease'",
                    [],
                    |row| row.get(0),
                )
                .expect("scheduler table count");
            assert_eq!(scheduler_tables, 0, "partial scheduler schema for {case}");
        }
    }

    #[test]
    fn version_nineteen_lease_and_history_guards_fail_closed() {
        let (_directory, connection) = open_test_database(43);
        assert!(
            connection
                .execute(
                    "INSERT INTO automation_scheduler_lease(
                         singleton,owner_id,fence,acquired_at,renewed_at,expires_at
                     ) VALUES (1,'owner-a',2,10,10,20)",
                    [],
                )
                .is_err()
        );
        connection
            .execute(
                "INSERT INTO automation_scheduler_lease(
                     singleton,owner_id,fence,acquired_at,renewed_at,expires_at
                 ) VALUES (1,'owner-a',1,10,10,20)",
                [],
            )
            .expect("initial fenced lease");
        assert!(
            connection
                .execute(
                    "UPDATE automation_scheduler_lease
                     SET owner_id='owner-b',fence=2,acquired_at=19,renewed_at=19,expires_at=29
                     WHERE singleton=1",
                    [],
                )
                .is_err()
        );
        assert!(
            connection
                .execute("DELETE FROM automation_scheduler_lease", [])
                .is_err()
        );
        connection
            .execute_batch(
                "INSERT INTO projects(
                     id,name,description,state,revision,created_at,updated_at
                 ) VALUES ('history-project','History project','',0,0,1,1);
                 INSERT INTO automations(
                     id,project_id,title,prompt,schedule,timezone,missed_run_policy,
                     overlap_policy,state,revision,created_at,updated_at
                 ) VALUES (
                     'history-automation','history-project','History automation','Prompt',
                     'v1;daily;09:00','UTC',0,0,1,0,1,1
                 );",
            )
            .expect("history owner");
        assert!(
            connection
                .execute(
                    "INSERT INTO automation_history(
                         automation_id,sequence,scheduled_for,recorded_at,status,summary
                     ) VALUES ('history-automation',2,2,3,0,'out of order')",
                    [],
                )
                .is_err()
        );
        connection
            .execute(
                "INSERT INTO automation_history(
                     automation_id,sequence,scheduled_for,recorded_at,status,summary
                 ) VALUES ('history-automation',1,2,3,0,'recorded')",
                [],
            )
            .expect("contiguous history");
        assert!(
            connection
                .execute(
                    "UPDATE automation_history SET summary='changed'
                     WHERE automation_id='history-automation' AND sequence=1",
                    [],
                )
                .is_err()
        );
    }
}
