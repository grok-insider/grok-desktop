use async_trait::async_trait;
use grok_application::{
    MutationCommand, StoreError, WorkspaceSearchHit, WorkspaceSearchKind, WorkspaceStore,
};
use grok_domain::{
    Automation, AutomationHistoryEntry, AutomationId, ConversationThreadOrigin, Message, MessageId,
    Project, ProjectId, ProjectState, Thread, ThreadId,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};

use crate::{SqlCipherStore, mapping};

const PROJECT_COLUMNS: &str = "id, name, description, state, revision, created_at, updated_at";
const THREAD_COLUMNS: &str = "id,project_id,title,state,revision,created_at,updated_at,\
    (SELECT parent_thread_id FROM conversation_thread_forks \
        WHERE child_thread_id=threads.id),\
    (SELECT source_turn_id FROM conversation_thread_forks \
        WHERE child_thread_id=threads.id),\
    (SELECT source_message_id FROM conversation_thread_forks \
        WHERE child_thread_id=threads.id),\
    (SELECT kind FROM conversation_thread_forks WHERE child_thread_id=threads.id),\
    (SELECT root_thread_id FROM conversation_thread_forks \
        WHERE child_thread_id=threads.id),\
    (SELECT fork_depth FROM conversation_thread_forks WHERE child_thread_id=threads.id)";
const MESSAGE_COLUMNS: &str = "id,thread_id,sequence,role,content,state,revision,created_at,updated_at,\
    (SELECT kind FROM conversation_message_derivations \
        WHERE child_message_id=messages.id),\
    (SELECT source_message_id FROM conversation_message_derivations \
        WHERE child_message_id=messages.id),\
    (SELECT source_turn_id FROM conversation_message_derivations \
        WHERE child_message_id=messages.id),\
    (SELECT source_context_sequence FROM conversation_message_derivations \
        WHERE child_message_id=messages.id)";
const AUTOMATION_COLUMNS: &str = "id, project_id, title, prompt, schedule, timezone, \
                                  missed_run_policy, overlap_policy, state, revision, \
                                  created_at, updated_at";
const HISTORY_COLUMNS: &str =
    "automation_id, sequence, scheduled_for, recorded_at, status, summary";

#[async_trait]
#[allow(clippy::too_many_lines)]
impl WorkspaceStore for SqlCipherStore {
    async fn resolve_mutation(
        &self,
        scope: &str,
        command: &MutationCommand,
    ) -> Result<Option<String>, StoreError> {
        let scope = scope.to_owned();
        let command = command.clone();
        self.with_store(move |connection| resolve_command(connection, &scope, &command))
            .await
    }

    async fn create_project(
        &self,
        project: Project,
        command: &MutationCommand,
    ) -> Result<Project, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(id) = resolve_command(&transaction, "create_project", &command)? {
                return query_project(&transaction, &id);
            }
            transaction
                .execute(
                    "INSERT INTO projects(id,name,description,state,revision,created_at,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        project.id.as_str(),
                        project.name,
                        project.description,
                        mapping::project_state_to_i64(project.state),
                        number(project.revision)?,
                        number(project.created_at)?,
                        number(project.updated_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            record_command(&transaction, &command, project.id.as_str())?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(project)
        })
        .await
    }

    async fn get_project(&self, id: &ProjectId) -> Result<Project, StoreError> {
        let id = id.to_string();
        self.with_store(move |connection| query_project(connection, &id))
            .await
    }

    async fn save_project(
        &self,
        project: Project,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if resolve_command(&transaction, &command.scope, &command)?.is_some() {
                return Ok(());
            }
            ensure_next_revision(project.revision, expected_revision)?;
            let changed = transaction
                .execute(
                    "UPDATE projects SET name=?1,description=?2,state=?3,revision=?4,updated_at=?5
                     WHERE id=?6 AND revision=?7",
                    params![
                        project.name,
                        project.description,
                        mapping::project_state_to_i64(project.state),
                        number(project.revision)?,
                        number(project.updated_at)?,
                        project.id.as_str(),
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            record_command(&transaction, &command, project.id.as_str())?;
            transaction.commit().map_err(map_sqlite)
        })
        .await
    }

    async fn list_projects(
        &self,
        after: Option<&ProjectId>,
        limit: usize,
    ) -> Result<Vec<Project>, StoreError> {
        let after = after.map(ToString::to_string);
        self.with_store(move |connection| {
            let cursor = recent_cursor(connection, "projects", after.as_deref(), None)?;
            collect_recent(
                connection,
                &format!("SELECT {PROJECT_COLUMNS} FROM projects"),
                cursor,
                limit,
                mapping::project_from_row,
            )
        })
        .await
    }

    async fn create_thread(
        &self,
        thread: Thread,
        command: &MutationCommand,
    ) -> Result<Thread, StoreError> {
        if Thread::restore(thread.clone()).is_err()
            || !matches!(&thread.lineage.origin, ConversationThreadOrigin::Original)
        {
            return Err(StoreError::Conflict);
        }
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(id) = resolve_command(&transaction, "create_thread", &command)? {
                return query_thread(&transaction, &id);
            }
            ensure_project_active(&transaction, &thread.project_id)?;
            transaction
                .execute(
                    "INSERT INTO threads(id,project_id,title,state,revision,created_at,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        thread.id.as_str(),
                        thread.project_id.as_str(),
                        thread.title,
                        mapping::thread_state_to_i64(thread.state),
                        number(thread.revision)?,
                        number(thread.created_at)?,
                        number(thread.updated_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            record_command(&transaction, &command, thread.id.as_str())?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(thread)
        })
        .await
    }

    async fn get_thread(&self, id: &ThreadId) -> Result<Thread, StoreError> {
        let id = id.to_string();
        self.with_store(move |connection| query_thread(connection, &id))
            .await
    }

    async fn save_thread(
        &self,
        thread: Thread,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError> {
        if Thread::restore(thread.clone()).is_err() {
            return Err(StoreError::Conflict);
        }
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if resolve_command(&transaction, &command.scope, &command)?.is_some() {
                return Ok(());
            }
            if command.scope == "update_thread" {
                ensure_project_active(&transaction, &thread.project_id)?;
            }
            let current = query_thread(&transaction, thread.id.as_str())?;
            if current.project_id != thread.project_id || current.lineage != thread.lineage {
                return Err(StoreError::Conflict);
            }
            ensure_next_revision(thread.revision, expected_revision)?;
            let changed = transaction
                .execute(
                    "UPDATE threads SET title=?1,state=?2,revision=?3,updated_at=?4
                     WHERE id=?5 AND revision=?6",
                    params![
                        thread.title,
                        mapping::thread_state_to_i64(thread.state),
                        number(thread.revision)?,
                        number(thread.updated_at)?,
                        thread.id.as_str(),
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            record_command(&transaction, &command, thread.id.as_str())?;
            transaction.commit().map_err(map_sqlite)
        })
        .await
    }

    async fn list_threads(
        &self,
        project_id: &ProjectId,
        after: Option<&ThreadId>,
        limit: usize,
    ) -> Result<Vec<Thread>, StoreError> {
        let project_id = project_id.to_string();
        let after = after.map(ToString::to_string);
        self.with_store(move |connection| {
            ensure_exists(connection, "projects", &project_id)?;
            let cursor = recent_cursor(
                connection,
                "threads",
                after.as_deref(),
                Some(("project_id", &project_id)),
            )?;
            collect_recent_scoped(
                connection,
                &format!("SELECT {THREAD_COLUMNS} FROM threads"),
                "project_id",
                &project_id,
                cursor,
                limit,
                mapping::thread_from_row,
            )
        })
        .await
    }

    async fn create_message(
        &self,
        mut message: Message,
        command: &MutationCommand,
    ) -> Result<Message, StoreError> {
        if !message.derivation.is_original() {
            return Err(StoreError::Conflict);
        }
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(id) = resolve_command(&transaction, "create_message", &command)? {
                return query_message(&transaction, &id);
            }
            let active: Option<(i64, i64)> = transaction
                .query_row(
                    "SELECT threads.state, projects.state FROM threads
                     JOIN projects ON projects.id=threads.project_id WHERE threads.id=?1",
                    [message.thread_id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(map_sqlite)?;
            match active {
                None => return Err(StoreError::NotFound),
                Some((0, 0)) => {}
                Some(_) => return Err(StoreError::Conflict),
            }
            let active_turn: bool = transaction
                .query_row(
                    "SELECT 1 FROM conversation_turns WHERE thread_id=?1 AND state IN (0,1)",
                    [message.thread_id.as_str()],
                    |_| Ok(true),
                )
                .optional()
                .map_err(map_sqlite)?
                .unwrap_or(false);
            if active_turn {
                return Err(StoreError::Conflict);
            }
            message.sequence = transaction
                .query_row(
                    "SELECT COALESCE(MAX(sequence),0)+1 FROM messages WHERE thread_id=?1",
                    [message.thread_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(map_sqlite)?
                .try_into()
                .map_err(|_| StoreError::Internal("message sequence exhausted".into()))?;
            transaction
                .execute(
                    "INSERT INTO messages(id,thread_id,sequence,role,content,state,revision,
                                          created_at,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                    params![
                        message.id.as_str(),
                        message.thread_id.as_str(),
                        number(message.sequence)?,
                        mapping::message_role_to_i64(message.role),
                        message.content,
                        mapping::message_state_to_i64(message.state),
                        number(message.revision)?,
                        number(message.created_at)?,
                        number(message.updated_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            record_command(&transaction, &command, message.id.as_str())?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(message)
        })
        .await
    }

    async fn get_message(&self, id: &MessageId) -> Result<Message, StoreError> {
        let id = id.to_string();
        self.with_store(move |connection| query_message(connection, &id))
            .await
    }

    async fn save_message(
        &self,
        message: Message,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError> {
        if !message.derivation.is_original() {
            return Err(StoreError::Conflict);
        }
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if resolve_command(&transaction, &command.scope, &command)?.is_some() {
                return Ok(());
            }
            let referenced: bool = transaction
                .query_row(
                    "SELECT 1 FROM conversation_turns
                     WHERE user_message_id=?1 OR assistant_message_id=?1
                     UNION ALL
                     SELECT 1 FROM conversation_turn_context WHERE message_id=?1
                     LIMIT 1",
                    [message.id.as_str()],
                    |_| Ok(true),
                )
                .optional()
                .map_err(map_sqlite)?
                .unwrap_or(false);
            if referenced {
                return Err(StoreError::Conflict);
            }
            let current = query_message(&transaction, message.id.as_str())?;
            if current.thread_id != message.thread_id || current.derivation != message.derivation {
                return Err(StoreError::Conflict);
            }
            if command.scope == "update_message" {
                let active: Option<(i64, i64)> = transaction
                    .query_row(
                        "SELECT threads.state,projects.state FROM threads
                         JOIN projects ON projects.id=threads.project_id WHERE threads.id=?1",
                        [message.thread_id.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()
                    .map_err(map_sqlite)?;
                if active != Some((0, 0)) {
                    return Err(StoreError::Conflict);
                }
            }
            ensure_next_revision(message.revision, expected_revision)?;
            let changed = transaction
                .execute(
                    "UPDATE messages SET content=?1,state=?2,revision=?3,updated_at=?4
                     WHERE id=?5 AND revision=?6",
                    params![
                        message.content,
                        mapping::message_state_to_i64(message.state),
                        number(message.revision)?,
                        number(message.updated_at)?,
                        message.id.as_str(),
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            record_command(&transaction, &command, message.id.as_str())?;
            transaction.commit().map_err(map_sqlite)
        })
        .await
    }

    async fn list_messages(
        &self,
        thread_id: &ThreadId,
        after: Option<&MessageId>,
        limit: usize,
    ) -> Result<Vec<Message>, StoreError> {
        let thread_id = thread_id.to_string();
        let after = after.map(ToString::to_string);
        self.with_store(move |connection| {
            ensure_exists(connection, "threads", &thread_id)?;
            let after_sequence = match after {
                Some(id) => Some(
                    connection
                        .query_row(
                            "SELECT sequence FROM messages WHERE id=?1 AND thread_id=?2",
                            params![id, thread_id],
                            |row| row.get::<_, i64>(0),
                        )
                        .map_err(map_sqlite)?,
                ),
                None => None,
            };
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {MESSAGE_COLUMNS} FROM messages
                     WHERE thread_id=?1 AND (?2 IS NULL OR sequence>?2)
                     ORDER BY sequence LIMIT ?3"
                ))
                .map_err(map_sqlite)?;
            collect_rows(statement.query_map(
                params![thread_id, after_sequence, sql_limit(limit)],
                mapping::message_from_row,
            ))
        })
        .await
    }

    async fn create_automation(
        &self,
        automation: Automation,
        command: &MutationCommand,
    ) -> Result<Automation, StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(id) = resolve_command(&transaction, "create_automation", &command)? {
                return query_automation(&transaction, &id);
            }
            ensure_project_active(&transaction, &automation.project_id)?;
            transaction
                .execute(
                    "INSERT INTO automations(id,project_id,title,prompt,schedule,timezone,
                                             missed_run_policy,overlap_policy,state,revision,
                                             created_at,updated_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                    params![
                        automation.id.as_str(),
                        automation.project_id.as_str(),
                        automation.title,
                        automation.prompt,
                        automation.schedule,
                        automation.timezone,
                        mapping::missed_run_policy_to_i64(automation.missed_run_policy),
                        mapping::overlap_policy_to_i64(automation.overlap_policy),
                        mapping::automation_state_to_i64(automation.state),
                        number(automation.revision)?,
                        number(automation.created_at)?,
                        number(automation.updated_at)?,
                    ],
                )
                .map_err(map_sqlite)?;
            record_command(&transaction, &command, automation.id.as_str())?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(automation)
        })
        .await
    }

    async fn get_automation(&self, id: &AutomationId) -> Result<Automation, StoreError> {
        let id = id.to_string();
        self.with_store(move |connection| query_automation(connection, &id))
            .await
    }

    async fn save_automation(
        &self,
        automation: Automation,
        expected_revision: u64,
        command: &MutationCommand,
    ) -> Result<(), StoreError> {
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if resolve_command(&transaction, &command.scope, &command)?.is_some() {
                return Ok(());
            }
            if command.scope == "update_automation" {
                ensure_project_active(&transaction, &automation.project_id)?;
            }
            ensure_next_revision(automation.revision, expected_revision)?;
            let changed = transaction
                .execute(
                    "UPDATE automations SET title=?1,prompt=?2,schedule=?3,timezone=?4,
                                            missed_run_policy=?5,overlap_policy=?6,state=?7,
                                            revision=?8,updated_at=?9
                     WHERE id=?10 AND revision=?11",
                    params![
                        automation.title,
                        automation.prompt,
                        automation.schedule,
                        automation.timezone,
                        mapping::missed_run_policy_to_i64(automation.missed_run_policy),
                        mapping::overlap_policy_to_i64(automation.overlap_policy),
                        mapping::automation_state_to_i64(automation.state),
                        number(automation.revision)?,
                        number(automation.updated_at)?,
                        automation.id.as_str(),
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            ensure_changed(changed)?;
            record_command(&transaction, &command, automation.id.as_str())?;
            transaction.commit().map_err(map_sqlite)
        })
        .await
    }

    async fn list_automations(
        &self,
        project_id: &ProjectId,
        after: Option<&AutomationId>,
        limit: usize,
    ) -> Result<Vec<Automation>, StoreError> {
        let project_id = project_id.to_string();
        let after = after.map(ToString::to_string);
        self.with_store(move |connection| {
            ensure_exists(connection, "projects", &project_id)?;
            let cursor = recent_cursor(
                connection,
                "automations",
                after.as_deref(),
                Some(("project_id", &project_id)),
            )?;
            collect_recent_scoped(
                connection,
                &format!("SELECT {AUTOMATION_COLUMNS} FROM automations"),
                "project_id",
                &project_id,
                cursor,
                limit,
                mapping::automation_from_row,
            )
        })
        .await
    }

    async fn record_automation_history(
        &self,
        mut entry: AutomationHistoryEntry,
    ) -> Result<AutomationHistoryEntry, StoreError> {
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            let existing = transaction
                .query_row(
                    &format!(
                        "SELECT {HISTORY_COLUMNS} FROM automation_history
                         WHERE automation_id=?1 AND scheduled_for=?2"
                    ),
                    params![entry.automation_id.as_str(), number(entry.scheduled_for)?],
                    mapping::automation_history_from_row,
                )
                .optional()
                .map_err(map_sqlite)?;
            if let Some(existing) = existing {
                if existing.automation_id == entry.automation_id
                    && existing.scheduled_for == entry.scheduled_for
                    && existing.recorded_at == entry.recorded_at
                    && existing.status == entry.status
                    && existing.summary == entry.summary
                {
                    return Ok(existing);
                }
                return Err(StoreError::Conflict);
            }
            ensure_exists(&transaction, "automations", entry.automation_id.as_str())?;
            entry.sequence = transaction
                .query_row(
                    "SELECT COALESCE(MAX(sequence),0)+1 FROM automation_history
                     WHERE automation_id=?1",
                    [entry.automation_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(map_sqlite)?
                .try_into()
                .map_err(|_| StoreError::Internal("automation history exhausted".into()))?;
            transaction
                .execute(
                    "INSERT INTO automation_history(automation_id,sequence,scheduled_for,
                                                     recorded_at,status,summary)
                     VALUES (?1,?2,?3,?4,?5,?6)",
                    params![
                        entry.automation_id.as_str(),
                        number(entry.sequence)?,
                        number(entry.scheduled_for)?,
                        number(entry.recorded_at)?,
                        mapping::automation_history_status_to_i64(entry.status),
                        entry.summary,
                    ],
                )
                .map_err(map_sqlite)?;
            transaction.commit().map_err(map_sqlite)?;
            Ok(entry)
        })
        .await
    }

    async fn automation_history(
        &self,
        automation_id: &AutomationId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<Vec<AutomationHistoryEntry>, StoreError> {
        let automation_id = automation_id.to_string();
        self.with_store(move |connection| {
            ensure_exists(connection, "automations", &automation_id)?;
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {HISTORY_COLUMNS} FROM automation_history
                     WHERE automation_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3"
                ))
                .map_err(map_sqlite)?;
            collect_rows(statement.query_map(
                params![automation_id, number(after_sequence)?, sql_limit(limit)],
                mapping::automation_history_from_row,
            ))
        })
        .await
    }

    async fn search(
        &self,
        project_id: Option<&ProjectId>,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<WorkspaceSearchHit>, StoreError> {
        let project_id = project_id.map(ToString::to_string);
        let fts_query = fts_query(query);
        let artifact_fts_query = format!("title : ({fts_query})");
        self.with_store(move |connection| {
            let mut statement = connection
                .prepare(
                    "WITH canonical AS (
                         SELECT documents.rowid AS document_rowid,
                                documents.id,
                                documents.kind,
                                documents.project_id AS indexed_project_id,
                                documents.title AS indexed_title,
                                documents.body AS indexed_body,
                                documents.updated_at AS indexed_updated_at,
                                CASE documents.kind
                                  WHEN 'project' THEN projects.id
                                  WHEN 'thread' THEN threads.project_id
                                  WHEN 'message' THEN message_threads.project_id
                                  WHEN 'artifact' THEN artifacts.project_id
                                  WHEN 'automation' THEN automations.project_id
                                END AS project_id,
                                CASE documents.kind
                                  WHEN 'thread' THEN threads.id
                                  WHEN 'message' THEN messages.thread_id
                                  WHEN 'artifact' THEN artifacts.thread_id
                                END AS thread_id,
                                CASE documents.kind
                                  WHEN 'project' THEN projects.name
                                  WHEN 'thread' THEN threads.title
                                  WHEN 'message' THEN message_threads.title
                                  WHEN 'artifact' THEN artifacts.name
                                  WHEN 'automation' THEN automations.title
                                END AS title,
                                CASE documents.kind
                                  WHEN 'project' THEN projects.description
                                  WHEN 'thread' THEN ''
                                  WHEN 'message' THEN messages.content
                                  WHEN 'artifact' THEN ''
                                  WHEN 'automation' THEN automations.prompt
                                END AS body,
                                CASE documents.kind
                                  WHEN 'project' THEN projects.updated_at
                                  WHEN 'thread' THEN threads.updated_at
                                  WHEN 'message' THEN messages.updated_at
                                  WHEN 'artifact' THEN artifacts.updated_at
                                  WHEN 'automation' THEN automations.updated_at
                                END AS updated_at
                         FROM search_documents AS documents
                         LEFT JOIN projects
                           ON documents.kind='project' AND projects.id=documents.id
                         LEFT JOIN threads
                           ON documents.kind='thread' AND threads.id=documents.id
                         LEFT JOIN projects AS thread_projects
                           ON thread_projects.id=threads.project_id
                         LEFT JOIN messages
                           ON documents.kind='message' AND messages.id=documents.id
                              AND messages.state=0
                         LEFT JOIN threads AS message_threads
                           ON message_threads.id=messages.thread_id
                         LEFT JOIN projects AS message_projects
                           ON message_projects.id=message_threads.project_id
                         LEFT JOIN artifacts
                           ON documents.kind='artifact' AND artifacts.id=documents.id
                              AND artifacts.state=1
                         LEFT JOIN projects AS artifact_projects
                           ON artifact_projects.id=artifacts.project_id
                         LEFT JOIN threads AS artifact_threads
                           ON artifact_threads.id=artifacts.thread_id
                              AND artifact_threads.project_id=artifacts.project_id
                         LEFT JOIN automations
                           ON documents.kind='automation' AND automations.id=documents.id
                         LEFT JOIN projects AS automation_projects
                           ON automation_projects.id=automations.project_id
                         WHERE (documents.kind='project' AND projects.id IS NOT NULL)
                            OR (documents.kind='thread' AND threads.id IS NOT NULL
                                AND thread_projects.id IS NOT NULL)
                            OR (documents.kind='message' AND messages.id IS NOT NULL
                                AND message_projects.id IS NOT NULL)
                            OR (documents.kind='artifact' AND artifacts.id IS NOT NULL
                                AND artifact_projects.id IS NOT NULL
                                AND (artifacts.thread_id IS NULL
                                     OR artifact_threads.id IS NOT NULL))
                            OR (documents.kind='automation' AND automations.id IS NOT NULL
                                AND automation_projects.id IS NOT NULL)
                     ), canonical_integrity AS (
                         SELECT * FROM canonical
                         WHERE indexed_project_id=project_id
                           AND indexed_title=title
                           AND indexed_body=body
                           AND indexed_updated_at=updated_at
                     ), matched AS (
                         SELECT canonical.*, bm25(search_documents_fts) AS rank
                         FROM search_documents_fts
                         JOIN canonical_integrity AS canonical
                           ON canonical.document_rowid=search_documents_fts.rowid
                         WHERE search_documents_fts MATCH ?1
                           AND canonical.kind!='artifact'
                         UNION ALL
                         SELECT canonical.*, bm25(search_documents_fts) AS rank
                         FROM search_documents_fts
                         JOIN canonical_integrity AS canonical
                           ON canonical.document_rowid=search_documents_fts.rowid
                         WHERE search_documents_fts MATCH ?2
                           AND canonical.kind='artifact'
                     )
                     SELECT id,project_id,kind,title,substr(body,1,512),updated_at,thread_id
                     FROM matched
                     WHERE ?3 IS NULL OR project_id=?3
                     ORDER BY rank,updated_at DESC,id
                     LIMIT ?4 OFFSET ?5",
                )
                .map_err(map_sqlite)?;
            collect_rows(statement.query_map(
                params![
                    fts_query,
                    artifact_fts_query,
                    project_id,
                    sql_limit(limit),
                    sql_limit(offset)
                ],
                search_hit_from_row,
            ))
        })
        .await
    }
}

fn resolve_command(
    connection: &Connection,
    scope: &str,
    command: &MutationCommand,
) -> Result<Option<String>, StoreError> {
    if scope != command.scope {
        return Err(StoreError::Conflict);
    }
    let existing = connection
        .query_row(
            "SELECT request_fingerprint,entity_id FROM workspace_commands
             WHERE scope=?1 AND idempotency_key=?2",
            params![scope, command.key],
            |row| Ok((row.get::<_, Vec<u8>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?;
    match existing {
        Some((fingerprint, entity_id)) if fingerprint == command.fingerprint => Ok(Some(entity_id)),
        Some(_) => Err(StoreError::Conflict),
        None => Ok(None),
    }
}

fn record_command(
    transaction: &Transaction<'_>,
    command: &MutationCommand,
    entity_id: &str,
) -> Result<(), StoreError> {
    transaction
        .execute(
            "INSERT INTO workspace_commands(scope,idempotency_key,request_fingerprint,entity_id)
             VALUES (?1,?2,?3,?4)",
            params![
                command.scope,
                command.key,
                command.fingerprint.as_slice(),
                entity_id
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn query_project(connection: &Connection, id: &str) -> Result<Project, StoreError> {
    connection
        .query_row(
            &format!("SELECT {PROJECT_COLUMNS} FROM projects WHERE id=?1"),
            [id],
            mapping::project_from_row,
        )
        .map_err(map_sqlite)
}

fn query_thread(connection: &Connection, id: &str) -> Result<Thread, StoreError> {
    connection
        .query_row(
            &format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id=?1"),
            [id],
            mapping::thread_from_row,
        )
        .map_err(map_sqlite)
}

fn query_message(connection: &Connection, id: &str) -> Result<Message, StoreError> {
    connection
        .query_row(
            &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id=?1"),
            [id],
            mapping::message_from_row,
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
}

fn ensure_project_active(
    connection: &Connection,
    project_id: &ProjectId,
) -> Result<(), StoreError> {
    let state = connection
        .query_row(
            "SELECT state FROM projects WHERE id=?1",
            [project_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    match state {
        None => Err(StoreError::NotFound),
        Some(state) if state == mapping::project_state_to_i64(ProjectState::Active) => Ok(()),
        Some(_) => Err(StoreError::Conflict),
    }
}

fn ensure_exists(connection: &Connection, table: &str, id: &str) -> Result<(), StoreError> {
    let sql = format!("SELECT 1 FROM {table} WHERE id=?1");
    let exists = connection
        .query_row(&sql, [id], |_| Ok(true))
        .optional()
        .map_err(map_sqlite)?
        .unwrap_or(false);
    if !exists {
        return Err(StoreError::NotFound);
    }
    Ok(())
}

fn recent_cursor(
    connection: &Connection,
    table: &str,
    id: Option<&str>,
    scope: Option<(&str, &str)>,
) -> Result<Option<(i64, String)>, StoreError> {
    let Some(id) = id else { return Ok(None) };
    let cursor = if let Some((column, value)) = scope {
        connection
            .query_row(
                &format!("SELECT updated_at,id FROM {table} WHERE id=?1 AND {column}=?2"),
                params![id, value],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    } else {
        connection
            .query_row(
                &format!("SELECT updated_at,id FROM {table} WHERE id=?1"),
                [id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
    }
    .map_err(map_sqlite)?;
    cursor.ok_or(StoreError::NotFound).map(Some)
}

fn collect_recent<T>(
    connection: &Connection,
    base: &str,
    cursor: Option<(i64, String)>,
    limit: usize,
    mapper: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, StoreError> {
    let mut statement = connection
        .prepare(&format!(
            "{base} WHERE (?1 IS NULL OR updated_at<?1 OR (updated_at=?1 AND id>?2))
             ORDER BY updated_at DESC,id LIMIT ?3"
        ))
        .map_err(map_sqlite)?;
    let (updated_at, id) = cursor.map_or((None, None), |(time, id)| (Some(time), Some(id)));
    collect_rows(statement.query_map(params![updated_at, id, sql_limit(limit)], mapper))
}

#[allow(clippy::too_many_arguments)]
fn collect_recent_scoped<T>(
    connection: &Connection,
    base: &str,
    scope_column: &str,
    scope: &str,
    cursor: Option<(i64, String)>,
    limit: usize,
    mapper: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> Result<Vec<T>, StoreError> {
    let mut statement = connection
        .prepare(&format!(
            "{base} WHERE {scope_column}=?1
             AND (?2 IS NULL OR updated_at<?2 OR (updated_at=?2 AND id>?3))
             ORDER BY updated_at DESC,id LIMIT ?4"
        ))
        .map_err(map_sqlite)?;
    let (updated_at, id) = cursor.map_or((None, None), |(time, id)| (Some(time), Some(id)));
    collect_rows(statement.query_map(params![scope, updated_at, id, sql_limit(limit)], mapper))
}

fn collect_rows<T>(
    rows: rusqlite::Result<rusqlite::MappedRows<'_, impl FnMut(&Row<'_>) -> rusqlite::Result<T>>>,
) -> Result<Vec<T>, StoreError> {
    rows.map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)
}

fn search_hit_from_row(row: &Row<'_>) -> rusqlite::Result<WorkspaceSearchHit> {
    let kind: String = row.get(2)?;
    let kind = match kind.as_str() {
        "project" => WorkspaceSearchKind::Project,
        "thread" => WorkspaceSearchKind::Thread,
        "message" => WorkspaceSearchKind::Message,
        "artifact" => WorkspaceSearchKind::Artifact,
        "automation" => WorkspaceSearchKind::Automation,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let snippet: String = row.get(4)?;
    let boundary = snippet.floor_char_boundary(snippet.len().min(512));
    Ok(WorkspaceSearchHit {
        id: row.get(0)?,
        project_id: ProjectId::new(row.get::<_, String>(1)?)
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        thread_id: row
            .get::<_, Option<String>>(6)?
            .map(ThreadId::new)
            .transpose()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
        kind,
        title: row.get(3)?,
        snippet: snippet[..boundary].to_owned(),
        updated_at: row
            .get::<_, i64>(5)?
            .try_into()
            .map_err(|_| rusqlite::Error::InvalidQuery)?,
    })
}

fn fts_query(query: &str) -> String {
    query
        .split_whitespace()
        .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

fn begin(connection: &mut Connection) -> Result<Transaction<'_>, StoreError> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite)
}

fn ensure_next_revision(revision: u64, expected: u64) -> Result<(), StoreError> {
    if revision != expected.checked_add(1).ok_or(StoreError::Conflict)? {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn ensure_changed(changed: usize) -> Result<(), StoreError> {
    if changed != 1 {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn number(value: u64) -> Result<i64, StoreError> {
    i64::try_from(value).map_err(|_| StoreError::Internal("numeric value out of range".into()))
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
