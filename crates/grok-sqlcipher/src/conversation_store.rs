use std::collections::HashSet;

use async_trait::async_trait;
use grok_application::{
    CancelConversationTurnCommit, ConversationForkCommandResolution, ConversationForkDelivery,
    ConversationForkDeliveryState, ConversationForkMetadata, ConversationForkPlan,
    ConversationForkReservation, ConversationForkSnapshot, ConversationInheritedAssistantOutcome,
    ConversationThreadCredentialBinding, ConversationThreadModelBinding, ConversationTurnEventPage,
    ConversationTurnReservation, ConversationTurnReservationSource, ConversationTurnSnapshot,
    ConversationTurnStore, MAX_CONVERSATION_CONTEXT_BYTES, MAX_CONVERSATION_CONTEXT_MESSAGES,
    MAX_CONVERSATION_EVENT_BATCH, MAX_CONVERSATION_FORK_DELIVERY_ALIASES,
    MAX_CONVERSATION_FORK_DIRECT_CHILDREN, MAX_CONVERSATION_FORK_FAMILY_THREADS,
    MAX_CONVERSATION_FORK_INHERITED_OUTCOMES, MAX_CONVERSATION_FORK_METADATA_BYTES,
    MutationCommand, NewRunEvent, ProviderStartCommit, StoreError, TerminalTurnCommit,
    UsageScope, UsageSummary, UsageWindow, conversation_fork_metadata_is_within_bounds,
    window_lower_bound,
};
use grok_domain::{
    ChatRail, ConversationCitation, ConversationFailure, ConversationFailureKind,
    ConversationForkKind, ConversationMessageDerivation, ConversationMessageDerivationKind,
    ConversationThreadOrigin, ConversationTurn, ConversationTurnEvent, ConversationTurnEventKind,
    ConversationTurnEventLog, ConversationTurnId, ConversationTurnLineage, ConversationTurnOrigin,
    ConversationTurnState, ConversationUsage, EffectId, EffectKind, EffectState, Idempotency,
    MAX_CONVERSATION_TEXT_CHUNK_BYTES, MAX_CONVERSATION_TEXT_EVENTS, MAX_MESSAGE_BYTES, Message,
    MessageId, MessageRole, MessageState, ProjectId, ProjectState, Run, RunEventKind, RunId,
    RunState, SideEffect, Thread, ThreadId, UnixMillis,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::{
    SqlCipherStore, mapping,
    store::{
        append_events, insert_effect, insert_run, map_sqlite, number, update_effect, update_run,
    },
};

const TURN_COLUMNS: &str = "id,idempotency_key,request_fingerprint,provider_request_fingerprint,\
    project_id,thread_id,user_message_id,run_id,model_id,state,effect_id,assistant_message_id,\
    failure_kind,failure_message,failure_retryable,provider_response_id,citations_json,\
    input_tokens,output_tokens,cost_in_usd_ticks,zero_data_retention,revision,created_at,updated_at";
const RUN_COLUMNS: &str = "id,project_id,thread_id,state,revision,created_at,updated_at";
const EFFECT_COLUMNS: &str =
    "id,run_id,kind,target,idempotency,state,revision,created_at,updated_at";
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
const TURN_EVENT_COLUMNS: &str = "sequence,turn_id,kind,from_state,to_state,start_utf8_offset,text";
const CONVERSATION_CANCEL_COMMAND_SCOPE: &str = "cancel_conversation_turn";
const CONVERSATION_RECONCILIATION_COMMAND_SCOPE: &str = "reconcile_conversation_dispatch_exit";
const CONVERSATION_COMMAND_SCOPE: &str = "execute_conversation_turn";
const CONVERSATION_RETRY_COMMAND_SCOPE: &str = "retry_conversation_turn";
const CONVERSATION_BRANCH_COMMAND_SCOPE: &str = "branch_conversation_thread";
const CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE: &str = "edit_and_branch_conversation_turn";
const CONVERSATION_REGENERATE_COMMAND_SCOPE: &str = "regenerate_conversation_turn";
const CONVERSATION_FORK_DELIVERY_ACK_COMMAND_SCOPE: &str = "acknowledge_conversation_fork_delivery";
const MAX_CONVERSATION_INHERITED_SOURCE_EDGES: usize = 65;
const MAX_CONVERSATION_SNAPSHOT_ANCESTRY: usize = 65;

#[derive(Debug, Serialize, Deserialize)]
struct StoredCitation {
    title: Option<String>,
    url: String,
}

#[async_trait]
impl ConversationTurnStore for SqlCipherStore {
    async fn reserve_turn(
        &self,
        turn: ConversationTurn,
        lineage: ConversationTurnLineage,
        source: ConversationTurnReservationSource,
        mut user_message: Message,
        run: Run,
        event: NewRunEvent,
        turn_event: ConversationTurnEventKind,
    ) -> Result<ConversationTurnReservation, StoreError> {
        if turn_event != ConversationTurnEventKind::Created
            || ConversationTurnLineage::restore(lineage.clone(), &turn.id).is_err()
            || !reservation_source_matches_lineage(&source, &lineage)
        {
            return Err(StoreError::Conflict);
        }
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(existing) = query_turn_by_key(&transaction, &turn.idempotency_key)? {
                if existing.request_fingerprint != turn.request_fingerprint {
                    return Err(StoreError::Conflict);
                }
                let snapshot = query_snapshot(&transaction, &existing)?;
                if snapshot.lineage != lineage {
                    return Err(StoreError::Conflict);
                }
                let context = query_context(&transaction, &existing.id)?;
                validate_turn_context(&context, &snapshot)?;
                commit(transaction)?;
                return Ok(ConversationTurnReservation {
                    snapshot,
                    context,
                    created: false,
                });
            }
            validate_reservation_links(
                &transaction,
                &turn,
                &user_message,
                &run,
                &event,
                &turn_event,
            )?;
            let (sequenced_user, context) = match source {
                ConversationTurnReservationSource::CurrentThread => {
                    bind_or_validate_thread_identity(
                        &transaction,
                        &turn.thread_id,
                        &lineage,
                        &turn.model_id,
                    )?;
                    user_message.sequence = next_message_sequence(&transaction, &turn.thread_id)?;
                    let mut context = query_active_messages(&transaction, &turn.thread_id)?;
                    context.push(user_message.clone());
                    validate_context(&context)?;
                    (user_message, context)
                }
                ConversationTurnReservationSource::Retry {
                    source_turn_id,
                    expected_source_revision,
                } => capture_retry_context(
                    &transaction,
                    &turn,
                    &lineage,
                    &source_turn_id,
                    expected_source_revision,
                    user_message,
                )?,
            };
            user_message = sequenced_user;
            insert_message(&transaction, &user_message)?;
            insert_run(&transaction, &run)?;
            append_events(&transaction, &run.id, vec![event])?;
            insert_turn(&transaction, &turn)?;
            insert_context(&transaction, &turn.id, &context)?;
            insert_turn_lineage(&transaction, &turn.id, &lineage)?;
            let mut turn_events = ConversationTurnEventLog::new(turn.id.clone());
            let created_event = turn_events
                .append_kind(turn_event)
                .map_err(|_| StoreError::Conflict)?;
            turn_events
                .validate_snapshot(&turn, None)
                .map_err(|_| StoreError::Conflict)?;
            insert_turn_event(&transaction, &created_event)?;
            let persisted_turn = query_turn(&transaction, &turn.id)?;
            let snapshot = query_snapshot(&transaction, &persisted_turn)?;
            let persisted_context = query_context(&transaction, &persisted_turn.id)?;
            validate_turn_context(&persisted_context, &snapshot)?;
            commit(transaction)?;
            Ok(ConversationTurnReservation {
                snapshot,
                context: persisted_context,
                created: true,
            })
        })
        .await
    }

    async fn load_turn_by_command(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ConversationTurnSnapshot>, StoreError> {
        if !matches!(
            command.scope.as_str(),
            CONVERSATION_COMMAND_SCOPE
                | CONVERSATION_RETRY_COMMAND_SCOPE
                | CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE
                | CONVERSATION_REGENERATE_COMMAND_SCOPE
        ) {
            return Ok(None);
        }
        let command = command.clone();
        self.with_store(move |connection| {
            let Some(turn) = query_turn_by_key(connection, &command.key)? else {
                return Ok(None);
            };
            if turn.request_fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            let snapshot = query_snapshot(connection, &turn)?;
            if !lineage_matches_command_scope(&snapshot.lineage, &command.scope) {
                return Err(StoreError::Conflict);
            }
            Ok(Some(snapshot))
        })
        .await
    }

    async fn load_turn(
        &self,
        id: &ConversationTurnId,
    ) -> Result<Option<ConversationTurnSnapshot>, StoreError> {
        let id = id.clone();
        self.with_store(move |connection| {
            let turn = connection
                .query_row(
                    &format!("SELECT {TURN_COLUMNS} FROM conversation_turns WHERE id=?1"),
                    [id.as_str()],
                    turn_from_row,
                )
                .optional()
                .map_err(map_sqlite)?;
            turn.as_ref()
                .map(|turn| query_snapshot(connection, turn))
                .transpose()
        })
        .await
    }

    async fn load_turn_context(&self, id: &ConversationTurnId) -> Result<Vec<Message>, StoreError> {
        let id = id.clone();
        self.with_store(move |connection| {
            let turn = query_turn(connection, &id)?;
            let snapshot = query_snapshot(connection, &turn)?;
            let context = query_context(connection, &id)?;
            validate_turn_context(&context, &snapshot)?;
            Ok(context)
        })
        .await
    }

    async fn commit_provider_start(
        &self,
        commit_value: ProviderStartCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            let current_turn = query_turn(&transaction, &commit_value.turn.id)?;
            let (current, mut turn_events) =
                query_snapshot_with_events(&transaction, &current_turn)?;
            if !valid_provider_start_commit(&current, &commit_value) {
                return Err(StoreError::Conflict);
            }
            let turn_event = turn_events
                .append_kind(commit_value.turn_event.clone())
                .map_err(|_| StoreError::Conflict)?;
            turn_events
                .validate_snapshot(&commit_value.turn, None)
                .map_err(|_| StoreError::Conflict)?;
            insert_effect(&transaction, &commit_value.effect)?;
            update_run_for_provider_start(
                &transaction,
                &commit_value.run,
                commit_value.expected_run_revision,
            )?;
            update_turn(
                &transaction,
                &commit_value.turn,
                commit_value.expected_turn_revision,
            )?;
            insert_turn_event(&transaction, &turn_event)?;
            append_events(&transaction, &commit_value.run.id, commit_value.events)?;
            let persisted_turn = query_turn(&transaction, &commit_value.turn.id)?;
            let snapshot = query_snapshot(&transaction, &persisted_turn)?;
            commit(transaction)?;
            Ok(snapshot)
        })
        .await
    }

    async fn commit_terminal(
        &self,
        mut commit_value: TerminalTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            let snapshot = commit_terminal_in_transaction(&transaction, &mut commit_value)?;
            commit(transaction)?;
            Ok(snapshot)
        })
        .await
    }

    async fn commit_cancellation(
        &self,
        cancellation: CancelConversationTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        commit_scoped_cancellation(self, cancellation, CONVERSATION_CANCEL_COMMAND_SCOPE).await
    }

    async fn commit_dispatch_exit_reconciliation(
        &self,
        cancellation: CancelConversationTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        commit_scoped_cancellation(
            self,
            cancellation,
            CONVERSATION_RECONCILIATION_COMMAND_SCOPE,
        )
        .await
    }

    async fn append_turn_text(
        &self,
        turn_id: &ConversationTurnId,
        expected_turn_revision: u64,
        start_utf8_offset: u64,
        text: String,
    ) -> Result<Vec<ConversationTurnEvent>, StoreError> {
        let turn_id = turn_id.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            let current_turn = query_turn(&transaction, &turn_id)?;
            let (snapshot, mut turn_events) =
                query_snapshot_with_events(&transaction, &current_turn)?;
            let normalized = normalized_text_chunks(start_utf8_offset, &text)?;

            if snapshot.turn.state != ConversationTurnState::ProviderStarted
                || snapshot.turn.revision != expected_turn_revision
            {
                return Err(StoreError::Conflict);
            }

            if start_utf8_offset < turn_events.next_utf8_offset() {
                let replay =
                    query_exact_text_replay(&transaction, &turn_id, start_utf8_offset, &text)?;
                commit(transaction)?;
                return Ok(replay);
            }
            if start_utf8_offset != turn_events.next_utf8_offset() {
                return Err(StoreError::Conflict);
            }

            let mut appended = Vec::with_capacity(normalized.len());
            for kind in normalized {
                let event = turn_events
                    .append_kind(kind)
                    .map_err(|_| StoreError::Conflict)?;
                insert_turn_event(&transaction, &event)?;
                appended.push(event);
            }
            turn_events
                .validate_snapshot(&snapshot.turn, None)
                .map_err(|_| StoreError::Conflict)?;
            commit(transaction)?;
            Ok(appended)
        })
        .await
    }

    async fn list_turn_events_since(
        &self,
        turn_id: &ConversationTurnId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<ConversationTurnEventPage, StoreError> {
        if !(1..=MAX_CONVERSATION_EVENT_BATCH).contains(&limit) {
            return Err(StoreError::Conflict);
        }
        let turn_id = turn_id.clone();
        self.with_store(move |connection| {
            let turn = query_turn(connection, &turn_id)?;
            query_snapshot_with_events(connection, &turn)?;
            query_event_page(connection, &turn_id, after_sequence, limit)
        })
        .await
    }

    async fn list_incomplete_turns_for_recovery(
        &self,
        limit: usize,
    ) -> Result<Vec<ConversationTurnSnapshot>, StoreError> {
        self.with_store(move |connection| {
            let mut statement = connection
                .prepare(&format!(
                    "SELECT {TURN_COLUMNS} FROM conversation_turns
                     WHERE state IN (0,1) ORDER BY created_at,id LIMIT ?1"
                ))
                .map_err(map_sqlite)?;
            let turns = statement
                .query_map([i64::try_from(limit).unwrap_or(i64::MAX)], turn_from_row)
                .map_err(map_sqlite)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(map_sqlite)?;
            turns
                .iter()
                .map(|turn| query_snapshot(connection, turn))
                .collect()
        })
        .await
    }

    async fn list_thread_turns(
        &self,
        thread_id: &ThreadId,
        after: Option<&ConversationTurnId>,
        limit: usize,
    ) -> Result<Vec<ConversationTurnSnapshot>, StoreError> {
        let thread_id = thread_id.clone();
        let after = after.cloned();
        self.with_store(move |connection| {
            let cursor = after
                .as_ref()
                .map(|id| {
                    connection
                        .query_row(
                            "SELECT created_at,id FROM conversation_turns
                             WHERE id=?1 AND thread_id=?2",
                            params![id.as_str(), thread_id.as_str()],
                            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
                        )
                        .optional()
                        .map_err(map_sqlite)?
                        .ok_or(StoreError::NotFound)
                })
                .transpose()?;
            let turns = if let Some((created_at, id)) = cursor {
                let mut statement = connection
                    .prepare(&format!(
                        "SELECT {TURN_COLUMNS} FROM conversation_turns
                         WHERE thread_id=?1 AND (created_at>?2 OR (created_at=?2 AND id>?3))
                         ORDER BY created_at,id LIMIT ?4"
                    ))
                    .map_err(map_sqlite)?;
                statement
                    .query_map(
                        params![
                            thread_id.as_str(),
                            created_at,
                            id,
                            i64::try_from(limit).unwrap_or(i64::MAX),
                        ],
                        turn_from_row,
                    )
                    .map_err(map_sqlite)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(map_sqlite)?
            } else {
                let mut statement = connection
                    .prepare(&format!(
                        "SELECT {TURN_COLUMNS} FROM conversation_turns
                         WHERE thread_id=?1 ORDER BY created_at,id LIMIT ?2"
                    ))
                    .map_err(map_sqlite)?;
                statement
                    .query_map(
                        params![thread_id.as_str(), i64::try_from(limit).unwrap_or(i64::MAX),],
                        turn_from_row,
                    )
                    .map_err(map_sqlite)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(map_sqlite)?
            };
            turns
                .iter()
                .map(|turn| query_snapshot(connection, turn))
                .collect()
        })
        .await
    }

    async fn summarize_usage(
        &self,
        scope: UsageScope,
        window: UsageWindow,
        as_of: UnixMillis,
    ) -> Result<UsageSummary, StoreError> {
        self.with_store(move |connection| summarize_usage_rows(connection, scope, window, as_of))
            .await
    }

    async fn retry_source_is_latest(&self, id: &ConversationTurnId) -> Result<bool, StoreError> {
        let id = id.clone();
        self.with_store(move |connection| {
            let turn = query_turn(connection, &id)?;
            let snapshot = query_snapshot(connection, &turn)?;
            let latest_sequence: u64 = connection
                .query_row(
                    "SELECT COALESCE(MAX(sequence),0) FROM messages WHERE thread_id=?1",
                    [snapshot.turn.thread_id.as_str()],
                    |row| row.get::<_, i64>(0),
                )
                .map_err(map_sqlite)?
                .try_into()
                .map_err(|_| invalid_persisted_aggregate())?;
            let has_retry_child: bool = connection
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM conversation_turn_lineage
                         WHERE origin=1 AND source_turn_id=?1
                     )",
                    [id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            Ok(latest_sequence == snapshot.user_message.sequence && !has_retry_child)
        })
        .await
    }

    async fn reserve_conversation_fork(
        &self,
        plan: ConversationForkPlan,
    ) -> Result<ConversationForkReservation, StoreError> {
        if !conversation_fork_scope_is_supported(&plan.command.scope) {
            return Err(StoreError::Conflict);
        }
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(record) = query_exact_fork_command(&transaction, &plan.command)? {
                let snapshot = query_fork_snapshot(&transaction, &record)?;
                let context = query_fork_context(&transaction, &record)?;
                commit(transaction)?;
                return Ok(ConversationForkReservation {
                    snapshot,
                    context,
                    created: false,
                    reconciled_pending_delivery: false,
                });
            }
            if let Some(record) = query_pending_fork_command(
                &transaction,
                &plan.command.scope,
                &plan.command.fingerprint,
            )? {
                insert_fork_delivery_alias(&transaction, &plan.command, &record.child_thread_id)?;
                let snapshot = query_fork_snapshot(&transaction, &record)?;
                let context = query_fork_context(&transaction, &record)?;
                commit(transaction)?;
                return Ok(ConversationForkReservation {
                    snapshot,
                    context,
                    created: false,
                    reconciled_pending_delivery: true,
                });
            }

            let prepared = prepare_conversation_fork(&transaction, &plan)?;
            insert_fork_thread(
                &transaction,
                &plan.child_thread,
                &prepared.credential_binding_id,
                &prepared.model_id,
            )?;
            insert_thread_fork(&transaction, &plan.child_thread)?;
            for message in &plan.messages {
                insert_message(&transaction, message)?;
                insert_message_derivation(&transaction, message)?;
            }
            for (message_id, source_turn_id) in &prepared.inherited_outcomes {
                insert_inherited_outcome(&transaction, message_id, source_turn_id)?;
            }
            if let Some(turn_plan) = &plan.started_turn {
                insert_run(&transaction, &turn_plan.run)?;
                append_events(
                    &transaction,
                    &turn_plan.run.id,
                    vec![turn_plan.run_event.clone()],
                )?;
                insert_turn(&transaction, &turn_plan.turn)?;
                insert_context(
                    &transaction,
                    &turn_plan.turn.id,
                    prepared.context.as_deref().ok_or(StoreError::Conflict)?,
                )?;
                insert_turn_lineage(&transaction, &turn_plan.turn.id, &turn_plan.lineage)?;
                insert_turn_event(
                    &transaction,
                    prepared
                        .created_turn_event
                        .as_ref()
                        .ok_or(StoreError::Conflict)?,
                )?;
            }
            insert_fork_command(&transaction, &plan)?;
            let record = query_fork_command(&transaction, &plan.command.scope, &plan.command.key)?
                .ok_or_else(invalid_persisted_aggregate)?;
            let snapshot = query_fork_snapshot(&transaction, &record)?;
            commit(transaction)?;
            Ok(ConversationForkReservation {
                snapshot,
                context: prepared.context,
                created: true,
                reconciled_pending_delivery: false,
            })
        })
        .await
    }

    async fn load_conversation_fork_by_command(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ConversationForkSnapshot>, StoreError> {
        if !conversation_fork_scope_is_supported(&command.scope) {
            return Ok(None);
        }
        let command = command.clone();
        self.with_store(move |connection| {
            let Some(record) = query_exact_fork_command(connection, &command)? else {
                return Ok(None);
            };
            Ok(Some(query_fork_snapshot(connection, &record)?))
        })
        .await
    }

    async fn resolve_conversation_fork_command(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ConversationForkCommandResolution>, StoreError> {
        if !conversation_fork_scope_is_supported(&command.scope) {
            return Ok(None);
        }
        let command = command.clone();
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(record) = query_exact_fork_command(&transaction, &command)? {
                let snapshot = query_fork_snapshot(&transaction, &record)?;
                commit(transaction)?;
                return Ok(Some(ConversationForkCommandResolution {
                    snapshot,
                    reconciled_pending_delivery: false,
                }));
            }
            let Some(record) =
                query_pending_fork_command(&transaction, &command.scope, &command.fingerprint)?
            else {
                commit(transaction)?;
                return Ok(None);
            };
            insert_fork_delivery_alias(&transaction, &command, &record.child_thread_id)?;
            let snapshot = query_fork_snapshot(&transaction, &record)?;
            commit(transaction)?;
            Ok(Some(ConversationForkCommandResolution {
                snapshot,
                reconciled_pending_delivery: true,
            }))
        })
        .await
    }

    async fn acknowledge_conversation_fork_delivery(
        &self,
        command: MutationCommand,
        child_thread_id: ThreadId,
        expected_revision: u64,
    ) -> Result<ConversationForkDelivery, StoreError> {
        if command.scope != CONVERSATION_FORK_DELIVERY_ACK_COMMAND_SCOPE {
            return Err(StoreError::Conflict);
        }
        self.with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(existing) =
                query_fork_delivery_ack_command(&transaction, &command.scope, &command.key)?
            {
                let canonical =
                    query_fork_command_by_child(&transaction, &existing.child_thread_id)?
                        .ok_or_else(invalid_persisted_aggregate)?;
                let persisted_delivery = query_fork_delivery(&transaction, &canonical)?;
                if persisted_delivery.state != ConversationForkDeliveryState::Acknowledged
                    || persisted_delivery.revision != existing.resulting_delivery_revision
                {
                    return Err(invalid_persisted_aggregate());
                }
                if existing.request_fingerprint != command.fingerprint
                    || existing.child_thread_id != child_thread_id
                    || existing.expected_delivery_revision != expected_revision
                {
                    return Err(StoreError::Conflict);
                }
                commit(transaction)?;
                return Ok(persisted_delivery);
            }

            let (_, delivery) =
                query_fork_delivery_for_acknowledgement(&transaction, &child_thread_id)?;
            if delivery.state != ConversationForkDeliveryState::Pending
                || delivery.revision != expected_revision
            {
                return Err(StoreError::Conflict);
            }
            let resulting_revision = expected_revision
                .checked_add(1)
                .ok_or(StoreError::Conflict)?;
            let changed = transaction
                .execute(
                    "UPDATE conversation_fork_deliveries SET state=1,revision=?1
                     WHERE child_thread_id=?2 AND state=0 AND revision=?3",
                    params![
                        number(resulting_revision)?,
                        child_thread_id.as_str(),
                        number(expected_revision)?,
                    ],
                )
                .map_err(map_sqlite)?;
            if changed != 1 {
                return Err(StoreError::Conflict);
            }
            let (_, acknowledged) =
                query_fork_delivery_for_acknowledgement(&transaction, &child_thread_id)?;
            if acknowledged.state != ConversationForkDeliveryState::Acknowledged
                || acknowledged.revision != resulting_revision
            {
                return Err(invalid_persisted_aggregate());
            }
            insert_fork_delivery_ack_command(
                &transaction,
                &command,
                &child_thread_id,
                expected_revision,
                resulting_revision,
            )?;
            let persisted =
                query_fork_delivery_ack_command(&transaction, &command.scope, &command.key)?
                    .ok_or_else(invalid_persisted_aggregate)?;
            if persisted.request_fingerprint != command.fingerprint
                || persisted.child_thread_id != child_thread_id
                || persisted.expected_delivery_revision != expected_revision
                || persisted.resulting_delivery_revision != resulting_revision
            {
                return Err(invalid_persisted_aggregate());
            }
            commit(transaction)?;
            Ok(acknowledged)
        })
        .await
    }

    async fn load_conversation_fork_metadata(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ConversationForkMetadata, StoreError> {
        let thread_id = thread_id.clone();
        self.with_store(move |connection| query_fork_metadata(connection, &thread_id))
            .await
    }

    async fn thread_credential_binding(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ConversationThreadCredentialBinding, StoreError> {
        let thread_id = thread_id.clone();
        self.with_store(move |connection| {
            let thread_exists: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1)",
                    [thread_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if !thread_exists {
                return Err(StoreError::NotFound);
            }
            if let Some(binding) = query_thread_credential_binding(connection, &thread_id)? {
                return Ok(ConversationThreadCredentialBinding::Bound(binding));
            }
            let has_historical_turn: bool = connection
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM conversation_turns WHERE thread_id=?1
                     )",
                    [thread_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if has_historical_turn {
                Ok(ConversationThreadCredentialBinding::LegacyUnbound)
            } else {
                Ok(ConversationThreadCredentialBinding::UnboundEmpty)
            }
        })
        .await
    }

    async fn thread_model_binding(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ConversationThreadModelBinding, StoreError> {
        let thread_id = thread_id.clone();
        self.with_store(move |connection| {
            let binding = query_thread_model_binding(connection, &thread_id)?;
            if let Some(model) = binding {
                return Ok(ConversationThreadModelBinding::Bound(model));
            }
            let has_historical_turn: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM conversation_turns WHERE thread_id=?1)",
                    [thread_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if has_historical_turn {
                Ok(ConversationThreadModelBinding::LegacyUnbound)
            } else {
                Ok(ConversationThreadModelBinding::UnboundEmpty)
            }
        })
        .await
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredForkCommand {
    command_scope: String,
    idempotency_key: String,
    request_fingerprint: [u8; 32],
    source_turn_id: ConversationTurnId,
    expected_source_revision: u64,
    child_thread_id: ThreadId,
    started_turn_id: Option<ConversationTurnId>,
}

#[derive(Debug, Clone)]
struct StoredForkDeliveryAlias {
    request_fingerprint: [u8; 32],
    child_thread_id: ThreadId,
}

#[derive(Debug, Clone)]
struct StoredForkDeliveryAckCommand {
    request_fingerprint: [u8; 32],
    child_thread_id: ThreadId,
    expected_delivery_revision: u64,
    resulting_delivery_revision: u64,
}

struct PreparedConversationFork {
    credential_binding_id: String,
    model_id: String,
    inherited_outcomes: Vec<(MessageId, ConversationTurnId)>,
    context: Option<Vec<Message>>,
    created_turn_event: Option<ConversationTurnEvent>,
}

type PreparedForkMessages = (Vec<(MessageId, ConversationTurnId)>, Option<Vec<Message>>);

fn conversation_fork_scope_is_supported(scope: &str) -> bool {
    matches!(
        scope,
        CONVERSATION_BRANCH_COMMAND_SCOPE
            | CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE
            | CONVERSATION_REGENERATE_COMMAND_SCOPE
    )
}

fn conversation_fork_scope_matches(scope: &str, kind: ConversationForkKind) -> bool {
    matches!(
        (scope, kind),
        (
            CONVERSATION_BRANCH_COMMAND_SCOPE,
            ConversationForkKind::Branch
        ) | (
            CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE,
            ConversationForkKind::EditAndBranch
        ) | (
            CONVERSATION_REGENERATE_COMMAND_SCOPE,
            ConversationForkKind::Regenerate
        )
    )
}

fn conversation_fork_kind(thread: &Thread) -> Result<ConversationForkKind, StoreError> {
    match &thread.lineage.origin {
        ConversationThreadOrigin::Fork { kind, .. } => Ok(*kind),
        ConversationThreadOrigin::Original => Err(StoreError::Conflict),
    }
}

#[allow(clippy::too_many_lines)]
fn prepare_conversation_fork(
    connection: &Connection,
    plan: &ConversationForkPlan,
) -> Result<PreparedConversationFork, StoreError> {
    if plan.command.key.is_empty() || plan.command.key.len() > 128 {
        return Err(StoreError::Conflict);
    }
    let child = Thread::restore(plan.child_thread.clone()).map_err(|_| StoreError::Conflict)?;
    let kind = conversation_fork_kind(&child)?;
    if !conversation_fork_scope_matches(&plan.command.scope, kind)
        || entity_exists(connection, "threads", child.id.as_str())?
    {
        return Err(StoreError::Conflict);
    }

    let source_turn = query_turn(connection, &plan.source_turn_id)?;
    let source = query_snapshot(connection, &source_turn)?;
    if source.turn.revision != plan.expected_source_revision {
        return Err(StoreError::Conflict);
    }
    let parent = query_thread(connection, &source.turn.thread_id)?;
    let project_state: Option<i64> = connection
        .query_row(
            "SELECT state FROM projects WHERE id=?1",
            [source.turn.project_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    if project_state != Some(mapping::project_state_to_i64(ProjectState::Active))
        || parent.project_id != source.turn.project_id
        || child.project_id != source.turn.project_id
        || child.title != parent.title
        || child.created_at < source.turn.updated_at
        || child.created_at < parent.updated_at
    {
        return Err(StoreError::Conflict);
    }

    let source_context = query_context(connection, &source.turn.id)?;
    validate_fork_source_context(&source, &source_context)?;
    let source_message = match kind {
        ConversationForkKind::EditAndBranch => &source.user_message,
        ConversationForkKind::Branch | ConversationForkKind::Regenerate => source
            .assistant_message
            .as_ref()
            .ok_or_else(invalid_persisted_aggregate)?,
    };
    let expected_child = Thread::new_fork(
        child.id.clone(),
        parent.project_id.clone(),
        parent.title.clone(),
        parent.id.clone(),
        &parent.lineage,
        source.turn.id.clone(),
        source_message.id.clone(),
        source_message.role,
        kind,
        child.created_at,
    )
    .map_err(|_| StoreError::Conflict)?;
    if child != expected_child {
        return Err(StoreError::Conflict);
    }

    ensure_fork_source_state(kind, &source)?;
    let credential_binding_id = source
        .lineage
        .credential_binding_id
        .clone()
        .ok_or(StoreError::Conflict)?;
    if query_thread_credential_binding(connection, &parent.id)?.as_deref()
        != Some(credential_binding_id.as_str())
    {
        return Err(StoreError::Conflict);
    }

    let root = query_thread(connection, &parent.lineage.root_thread_id)?;
    if root.project_id != parent.project_id
        || root.lineage.root_thread_id != root.id
        || !matches!(root.lineage.origin, ConversationThreadOrigin::Original)
    {
        return Err(invalid_persisted_aggregate());
    }
    let direct_children: usize = connection
        .query_row(
            "SELECT count(*) FROM conversation_thread_forks WHERE parent_thread_id=?1",
            [parent.id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_sqlite)?
        .try_into()
        .map_err(|_| invalid_persisted_aggregate())?;
    let family_threads: usize = connection
        .query_row(
            "SELECT 1 + count(*) FROM conversation_thread_forks WHERE root_thread_id=?1",
            [parent.lineage.root_thread_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_sqlite)?
        .try_into()
        .map_err(|_| invalid_persisted_aggregate())?;
    if direct_children >= MAX_CONVERSATION_FORK_DIRECT_CHILDREN
        || family_threads >= MAX_CONVERSATION_FORK_FAMILY_THREADS
    {
        return Err(StoreError::Conflict);
    }

    let (inherited_outcomes, provider_context) =
        validate_fork_messages(connection, plan, &source, &source_context, kind)?;
    let created_turn_event = validate_fork_turn_plan(
        connection,
        plan,
        &source,
        &credential_binding_id,
        provider_context.as_deref(),
        kind,
    )?;
    validate_projected_fork_metadata_budget(connection, plan, &inherited_outcomes)?;
    Ok(PreparedConversationFork {
        credential_binding_id,
        model_id: source.turn.model_id.clone(),
        inherited_outcomes,
        context: provider_context,
        created_turn_event,
    })
}

fn validate_projected_fork_metadata_budget(
    connection: &Connection,
    plan: &ConversationForkPlan,
    inherited_outcomes: &[(MessageId, ConversationTurnId)],
) -> Result<(), StoreError> {
    if inherited_outcomes.len() > MAX_CONVERSATION_FORK_INHERITED_OUTCOMES {
        return Err(StoreError::Conflict);
    }
    let mut family_threads =
        query_fork_family_threads(connection, &plan.child_thread.lineage.root_thread_id)?;
    family_threads.push(plan.child_thread.clone());
    let inherited_assistant_outcomes = inherited_outcomes
        .iter()
        .map(|(message_id, source_turn_id)| {
            let child_message = plan
                .messages
                .iter()
                .find(|message| message.id == *message_id)
                .ok_or(StoreError::Conflict)?;
            projected_inherited_outcome(connection, child_message, source_turn_id)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let metadata = ConversationForkMetadata {
        lineage: plan.child_thread.lineage.clone(),
        inherited_assistant_outcomes,
        family_threads,
    };
    conversation_fork_metadata_is_within_bounds(&metadata)
        .then_some(())
        .ok_or(StoreError::Conflict)
}

fn projected_inherited_outcome(
    connection: &Connection,
    child_message: &Message,
    source_turn_id: &ConversationTurnId,
) -> Result<ConversationInheritedAssistantOutcome, StoreError> {
    let source_turn = query_turn(connection, source_turn_id)?;
    let assistant_id = source_turn
        .assistant_message_id
        .as_ref()
        .ok_or(StoreError::Conflict)?;
    let assistant = query_message(connection, assistant_id)?;
    validate_completed_assistant_source(connection, source_turn_id, &assistant)
        .map_err(|_| StoreError::Conflict)?;
    if child_message.role != MessageRole::Assistant || child_message.content != assistant.content {
        return Err(StoreError::Conflict);
    }
    Ok(ConversationInheritedAssistantOutcome {
        child_assistant_message_id: child_message.id.clone(),
        source_turn_id: source_turn_id.clone(),
        model_id: source_turn.model_id,
        citations: source_turn.citations,
        usage: source_turn.usage,
        zero_data_retention: source_turn.zero_data_retention,
    })
}

fn ensure_fork_source_state(
    kind: ConversationForkKind,
    source: &ConversationTurnSnapshot,
) -> Result<(), StoreError> {
    let eligible = match kind {
        ConversationForkKind::Branch | ConversationForkKind::Regenerate => {
            source.turn.state == ConversationTurnState::Completed
        }
        ConversationForkKind::EditAndBranch => matches!(
            source.turn.state,
            ConversationTurnState::Completed
                | ConversationTurnState::Cancelled
                | ConversationTurnState::Failed
        ),
    };
    eligible.then_some(()).ok_or(StoreError::Conflict)
}

fn validate_fork_source_context(
    source: &ConversationTurnSnapshot,
    context: &[Message],
) -> Result<(), StoreError> {
    validate_context(context).map_err(|_| invalid_persisted_aggregate())?;
    let mut ids = HashSet::with_capacity(context.len());
    let mut previous_sequence = 0;
    for message in context {
        if Message::restore(message.clone()).is_err()
            || message.thread_id != source.turn.thread_id
            || message.state != MessageState::Active
            || message.sequence <= previous_sequence
            || !ids.insert(message.id.clone())
        {
            return Err(invalid_persisted_aggregate());
        }
        previous_sequence = message.sequence;
    }
    if context.last() != Some(&source.user_message) {
        return Err(invalid_persisted_aggregate());
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_fork_messages(
    connection: &Connection,
    plan: &ConversationForkPlan,
    source: &ConversationTurnSnapshot,
    source_context: &[Message],
    kind: ConversationForkKind,
) -> Result<PreparedForkMessages, StoreError> {
    let context_copy_count = if kind == ConversationForkKind::EditAndBranch {
        source_context
            .len()
            .checked_sub(1)
            .ok_or(StoreError::Conflict)?
    } else {
        source_context.len()
    };
    let expected_count = context_copy_count
        + usize::from(matches!(
            kind,
            ConversationForkKind::Branch | ConversationForkKind::EditAndBranch
        ));
    if plan.messages.len() != expected_count || plan.messages.is_empty() {
        return Err(StoreError::Conflict);
    }
    let mut ids = HashSet::with_capacity(plan.messages.len());
    let mut inherited = Vec::new();
    for (index, (message, source_message)) in plan
        .messages
        .iter()
        .take(context_copy_count)
        .zip(source_context.iter())
        .enumerate()
    {
        let sequence = u64::try_from(index + 1).map_err(|_| StoreError::Conflict)?;
        let context_position = u32::try_from(index + 1).map_err(|_| StoreError::Conflict)?;
        let expected = Message::new_derived(
            message.id.clone(),
            plan.child_thread.id.clone(),
            sequence,
            source_message.role,
            source_message.content.clone(),
            source_message.id.clone(),
            source.turn.id.clone(),
            Some(context_position),
            ConversationMessageDerivationKind::ContextCopy,
            plan.child_thread.created_at,
        )
        .map_err(|_| StoreError::Conflict)?;
        validate_new_fork_message(connection, &mut ids, message, &expected)?;
        if source_message.role == MessageRole::Assistant {
            inherited.push((
                message.id.clone(),
                inherited_source_turn(connection, source_message)?,
            ));
        }
    }

    match kind {
        ConversationForkKind::Branch => {
            let source_assistant = source
                .assistant_message
                .as_ref()
                .ok_or(StoreError::Conflict)?;
            let message = plan.messages.last().ok_or(StoreError::Conflict)?;
            let expected = Message::new_derived(
                message.id.clone(),
                plan.child_thread.id.clone(),
                u64::try_from(plan.messages.len()).map_err(|_| StoreError::Conflict)?,
                MessageRole::Assistant,
                source_assistant.content.clone(),
                source_assistant.id.clone(),
                source.turn.id.clone(),
                None,
                ConversationMessageDerivationKind::SourceAssistantCopy,
                plan.child_thread.created_at,
            )
            .map_err(|_| StoreError::Conflict)?;
            validate_new_fork_message(connection, &mut ids, message, &expected)?;
            inherited.push((message.id.clone(), source.turn.id.clone()));
            Ok((inherited, None))
        }
        ConversationForkKind::EditAndBranch => {
            let message = plan.messages.last().ok_or(StoreError::Conflict)?;
            if message.content == source.user_message.content {
                return Err(StoreError::Conflict);
            }
            let expected = Message::new_derived(
                message.id.clone(),
                plan.child_thread.id.clone(),
                u64::try_from(plan.messages.len()).map_err(|_| StoreError::Conflict)?,
                MessageRole::User,
                message.content.clone(),
                source.user_message.id.clone(),
                source.turn.id.clone(),
                Some(u32::try_from(source_context.len()).map_err(|_| StoreError::Conflict)?),
                ConversationMessageDerivationKind::EditedUser,
                plan.child_thread.created_at,
            )
            .map_err(|_| StoreError::Conflict)?;
            validate_new_fork_message(connection, &mut ids, message, &expected)?;
            validate_context(&plan.messages)?;
            Ok((inherited, Some(plan.messages.clone())))
        }
        ConversationForkKind::Regenerate => {
            validate_context(&plan.messages)?;
            Ok((inherited, Some(plan.messages.clone())))
        }
    }
}

fn validate_new_fork_message(
    connection: &Connection,
    ids: &mut HashSet<MessageId>,
    actual: &Message,
    expected: &Message,
) -> Result<(), StoreError> {
    if actual != expected
        || entity_exists(connection, "messages", actual.id.as_str())?
        || !ids.insert(actual.id.clone())
    {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn inherited_source_turn(
    connection: &Connection,
    source_message: &Message,
) -> Result<ConversationTurnId, StoreError> {
    if Message::restore(source_message.clone()).is_err()
        || source_message.role != MessageRole::Assistant
        || source_message.state != MessageState::Active
    {
        return Err(invalid_persisted_aggregate());
    }
    let direct = direct_assistant_turn_id(connection, &source_message.id)?;
    let inherited = inherited_outcome_turn_id(connection, &source_message.id)?;
    match (direct, inherited) {
        (Some(turn_id), None) => {
            validate_completed_assistant_source(connection, &turn_id, source_message)?;
            Ok(turn_id)
        }
        (None, Some(turn_id)) => {
            validate_inherited_assistant_chain(connection, source_message, &turn_id)?;
            Ok(turn_id)
        }
        (None, None) | (Some(_), Some(_)) => Err(invalid_persisted_aggregate()),
    }
}

fn direct_assistant_turn_id(
    connection: &Connection,
    message_id: &MessageId,
) -> Result<Option<ConversationTurnId>, StoreError> {
    connection
        .query_row(
            "SELECT id FROM conversation_turns WHERE assistant_message_id=?1",
            [message_id.as_str()],
            |row| {
                ConversationTurnId::new(row.get::<_, String>(0)?)
                    .map_err(|error| conversion(0, error))
            },
        )
        .optional()
        .map_err(map_sqlite)
}

fn inherited_outcome_turn_id(
    connection: &Connection,
    message_id: &MessageId,
) -> Result<Option<ConversationTurnId>, StoreError> {
    connection
        .query_row(
            "SELECT source_turn_id FROM conversation_inherited_assistant_outcomes
             WHERE child_assistant_message_id=?1",
            [message_id.as_str()],
            |row| {
                ConversationTurnId::new(row.get::<_, String>(0)?)
                    .map_err(|error| conversion(0, error))
            },
        )
        .optional()
        .map_err(map_sqlite)
}

fn validate_completed_assistant_source(
    connection: &Connection,
    source_turn_id: &ConversationTurnId,
    expected_assistant: &Message,
) -> Result<ConversationTurn, StoreError> {
    let source_turn = query_turn(connection, source_turn_id)?;
    if source_turn.state != ConversationTurnState::Completed
        || source_turn.assistant_message_id.as_ref() != Some(&expected_assistant.id)
        || source_turn.thread_id != expected_assistant.thread_id
        || expected_assistant.role != MessageRole::Assistant
        || expected_assistant.state != MessageState::Active
        || !matches!(
            expected_assistant.derivation,
            ConversationMessageDerivation::Original
        )
    {
        return Err(invalid_persisted_aggregate());
    }
    let persisted_assistant = query_message(connection, &expected_assistant.id)?;
    let user_message = query_message(connection, &source_turn.user_message_id)?;
    let run = query_run(connection, &source_turn.run_id)?;
    let effect = source_turn
        .effect_id
        .as_ref()
        .map(|effect_id| query_effect(connection, effect_id))
        .transpose()?;
    let lineage = query_turn_lineage(connection, source_turn_id)?;
    let snapshot = ConversationTurnSnapshot {
        turn: source_turn.clone(),
        user_message: user_message.clone(),
        assistant_message: Some(persisted_assistant.clone()),
        run,
        effect,
        lineage,
    };
    let project_id = connection
        .query_row(
            "SELECT project_id FROM threads WHERE id=?1",
            [source_turn.thread_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    if persisted_assistant != *expected_assistant
        || project_id.as_deref() != Some(source_turn.project_id.as_str())
        || !is_canonical_linked_message(
            &user_message,
            &source_turn.user_message_id,
            &source_turn.thread_id,
            MessageRole::User,
            source_turn.created_at,
        )
        || !is_canonical_linked_message(
            &persisted_assistant,
            source_turn
                .assistant_message_id
                .as_ref()
                .ok_or_else(invalid_persisted_aggregate)?,
            &source_turn.thread_id,
            MessageRole::Assistant,
            source_turn.updated_at,
        )
        || persisted_assistant.sequence <= user_message.sequence
        || !canonical_linked_run_and_effect(&snapshot)
        || query_thread_credential_binding(connection, &source_turn.thread_id)?
            != snapshot.lineage.credential_binding_id
    {
        return Err(invalid_persisted_aggregate());
    }
    let turn_events = query_turn_event_log(connection, source_turn_id)?;
    turn_events
        .validate_snapshot(&source_turn, Some(persisted_assistant.content.as_str()))
        .map_err(|_| invalid_persisted_aggregate())?;
    Ok(source_turn)
}

fn validate_inherited_assistant_chain(
    connection: &Connection,
    child_assistant: &Message,
    canonical_turn_id: &ConversationTurnId,
) -> Result<(), StoreError> {
    let canonical_turn = query_turn(connection, canonical_turn_id)?;
    let canonical_assistant_id = canonical_turn
        .assistant_message_id
        .as_ref()
        .ok_or_else(invalid_persisted_aggregate)?;
    let canonical_assistant = query_message(connection, canonical_assistant_id)?;
    validate_completed_assistant_source(connection, canonical_turn_id, &canonical_assistant)?;
    if child_assistant.content != canonical_assistant.content {
        return Err(invalid_persisted_aggregate());
    }

    let mut current = child_assistant.clone();
    let mut visited = HashSet::with_capacity(MAX_CONVERSATION_INHERITED_SOURCE_EDGES);
    for _ in 0..MAX_CONVERSATION_INHERITED_SOURCE_EDGES {
        if current.id == canonical_assistant.id {
            return if current == canonical_assistant
                && direct_assistant_turn_id(connection, &current.id)?.as_ref()
                    == Some(canonical_turn_id)
                && inherited_outcome_turn_id(connection, &current.id)?.is_none()
            {
                Ok(())
            } else {
                Err(invalid_persisted_aggregate())
            };
        }
        if !visited.insert(current.id.clone())
            || inherited_outcome_turn_id(connection, &current.id)?.as_ref()
                != Some(canonical_turn_id)
        {
            return Err(invalid_persisted_aggregate());
        }
        let ConversationMessageDerivation::Fork {
            kind,
            source_message_id,
            source_turn_id,
            source_context_sequence,
        } = &current.derivation
        else {
            return Err(invalid_persisted_aggregate());
        };
        let source_message = query_message(connection, source_message_id)?;
        if source_message.role != MessageRole::Assistant
            || source_message.state != MessageState::Active
            || source_message.content != current.content
        {
            return Err(invalid_persisted_aggregate());
        }
        match kind {
            ConversationMessageDerivationKind::SourceAssistantCopy
                if source_turn_id == canonical_turn_id
                    && source_context_sequence.is_none()
                    && source_message.id == canonical_assistant.id =>
            {
                validate_source_assistant_copy_edge(
                    connection,
                    &current,
                    &source_message,
                    canonical_turn_id,
                )?;
            }
            ConversationMessageDerivationKind::ContextCopy if source_context_sequence.is_some() => {
                validate_context_copy_edge(
                    connection,
                    &current,
                    &source_message,
                    source_turn_id,
                    source_context_sequence.ok_or_else(invalid_persisted_aggregate)?,
                )?;
            }
            ConversationMessageDerivationKind::ContextCopy
            | ConversationMessageDerivationKind::SourceAssistantCopy
            | ConversationMessageDerivationKind::EditedUser => {
                return Err(invalid_persisted_aggregate());
            }
        }
        current = source_message;
    }
    Err(invalid_persisted_aggregate())
}

fn validate_source_assistant_copy_edge(
    connection: &Connection,
    child_message: &Message,
    source_message: &Message,
    source_turn_id: &ConversationTurnId,
) -> Result<(), StoreError> {
    let child_thread = query_thread(connection, &child_message.thread_id)?;
    let source_thread = query_thread(connection, &source_message.thread_id)?;
    let ConversationThreadOrigin::Fork {
        parent_thread_id,
        source_turn_id: fork_source_turn_id,
        source_message_id,
        kind,
    } = &child_thread.lineage.origin
    else {
        return Err(invalid_persisted_aggregate());
    };
    let source_context_count: i64 = connection
        .query_row(
            "SELECT count(*) FROM conversation_turn_context WHERE turn_id=?1",
            [source_turn_id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if *kind != ConversationForkKind::Branch
        || fork_source_turn_id != source_turn_id
        || source_message_id != &source_message.id
        || parent_thread_id != &source_message.thread_id
        || child_thread.lineage.root_thread_id != source_thread.lineage.root_thread_id
        || source_thread.lineage.fork_depth.checked_add(1) != Some(child_thread.lineage.fork_depth)
        || i64::try_from(child_message.sequence).ok() != source_context_count.checked_add(1)
    {
        return Err(invalid_persisted_aggregate());
    }
    Ok(())
}

fn validate_context_copy_edge(
    connection: &Connection,
    child_message: &Message,
    source_message: &Message,
    source_turn_id: &ConversationTurnId,
    source_context_sequence: u32,
) -> Result<(), StoreError> {
    let child_thread = query_thread(connection, &child_message.thread_id)?;
    let ConversationThreadOrigin::Fork {
        parent_thread_id,
        source_turn_id: fork_source_turn_id,
        ..
    } = &child_thread.lineage.origin
    else {
        return Err(invalid_persisted_aggregate());
    };
    let source_turn = query_turn(connection, source_turn_id)?;
    let source_thread = query_thread(connection, &source_message.thread_id)?;
    let stored_context: Option<(i64, String, String, i64, i64, i64, i64)> = connection
        .query_row(
            "SELECT context.role,context.content,context.message_id,
                    context.revision,context.sequence,context.created_at,context.updated_at
             FROM conversation_turn_context context
             WHERE context.turn_id=?1 AND context.message_id=?2",
            [source_turn_id.as_str(), source_message.id.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?;
    let Some((role, content, message_id, revision, sequence, created_at, updated_at)) =
        stored_context
    else {
        return Err(invalid_persisted_aggregate());
    };
    let ordinal: i64 = connection
        .query_row(
            "SELECT count(*) FROM conversation_turn_context
             WHERE turn_id=?1 AND sequence<=?2",
            params![source_turn_id.as_str(), sequence],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if fork_source_turn_id != source_turn_id
        || parent_thread_id != &source_turn.thread_id
        || source_message.thread_id != source_turn.thread_id
        || child_thread.lineage.root_thread_id != source_thread.lineage.root_thread_id
        || source_thread.lineage.fork_depth.checked_add(1) != Some(child_thread.lineage.fork_depth)
        || child_message.sequence != u64::from(source_context_sequence)
        || ordinal != i64::from(source_context_sequence)
        || role != mapping::message_role_to_i64(source_message.role)
        || content != source_message.content
        || message_id != source_message.id.as_str()
        || revision != i64::try_from(source_message.revision).unwrap_or(-1)
        || created_at != i64::try_from(source_message.created_at).unwrap_or(-1)
        || updated_at != i64::try_from(source_message.updated_at).unwrap_or(-1)
    {
        return Err(invalid_persisted_aggregate());
    }
    Ok(())
}

fn validate_fork_turn_plan(
    connection: &Connection,
    plan: &ConversationForkPlan,
    source: &ConversationTurnSnapshot,
    credential_binding_id: &str,
    context: Option<&[Message]>,
    kind: ConversationForkKind,
) -> Result<Option<ConversationTurnEvent>, StoreError> {
    if kind == ConversationForkKind::Branch {
        return if plan.started_turn.is_none() && context.is_none() {
            Ok(None)
        } else {
            Err(StoreError::Conflict)
        };
    }
    let turn_plan = plan.started_turn.as_ref().ok_or(StoreError::Conflict)?;
    let context = context.ok_or(StoreError::Conflict)?;
    let user_message = context.last().ok_or(StoreError::Conflict)?;
    let expected_origin = match kind {
        ConversationForkKind::EditAndBranch => ConversationTurnOrigin::EditAndBranch {
            source_turn_id: source.turn.id.clone(),
        },
        ConversationForkKind::Regenerate => ConversationTurnOrigin::Regenerate {
            source_turn_id: source.turn.id.clone(),
        },
        ConversationForkKind::Branch => return Err(StoreError::Conflict),
    };
    if turn_plan.lineage.origin != expected_origin
        || turn_plan.lineage.credential_binding_id.as_deref() != Some(credential_binding_id)
        || turn_plan.lineage.retry_depth != 0
        || ConversationTurnLineage::restore(turn_plan.lineage.clone(), &turn_plan.turn.id).is_err()
        || turn_plan.turn.idempotency_key != plan.command.key
        || turn_plan.turn.request_fingerprint != plan.command.fingerprint
        || turn_plan.turn.project_id != plan.child_thread.project_id
        || turn_plan.turn.thread_id != plan.child_thread.id
        || turn_plan.turn.user_message_id != user_message.id
        || turn_plan.turn.model_id != source.turn.model_id
        || turn_plan.turn.created_at != plan.child_thread.created_at
        || turn_plan.turn.state != ConversationTurnState::Reserved
        || turn_plan.turn.revision != 0
        || user_message.role != MessageRole::User
        || entity_exists(connection, "conversation_turns", turn_plan.turn.id.as_str())?
        || entity_exists(connection, "runs", turn_plan.run.id.as_str())?
        || query_turn_by_key(connection, &turn_plan.turn.idempotency_key)?.is_some()
    {
        return Err(StoreError::Conflict);
    }
    let expected_turn = ConversationTurn::reserve(
        turn_plan.turn.id.clone(),
        plan.command.key.clone(),
        plan.command.fingerprint,
        plan.child_thread.project_id.clone(),
        plan.child_thread.id.clone(),
        user_message.id.clone(),
        turn_plan.run.id.clone(),
        source.turn.model_id.clone(),
        plan.child_thread.created_at,
    )
    .map_err(|_| StoreError::Conflict)?;
    let expected_run = Run::queued(
        turn_plan.run.id.clone(),
        plan.child_thread.project_id.clone(),
        plan.child_thread.id.clone(),
        plan.child_thread.created_at,
    );
    if turn_plan.turn != expected_turn
        || turn_plan.run != expected_run
        || turn_plan.run.id != turn_plan.turn.run_id
        || turn_plan.run_event
            != (NewRunEvent {
                occurred_at: plan.child_thread.created_at,
                kind: RunEventKind::Created,
            })
        || turn_plan.turn_event != ConversationTurnEventKind::Created
    {
        return Err(StoreError::Conflict);
    }
    let mut event_log = ConversationTurnEventLog::new(turn_plan.turn.id.clone());
    let event = event_log
        .append_kind(ConversationTurnEventKind::Created)
        .map_err(|_| StoreError::Conflict)?;
    event_log
        .validate_snapshot(&turn_plan.turn, None)
        .map_err(|_| StoreError::Conflict)?;
    Ok(Some(event))
}

fn entity_exists(connection: &Connection, table: &str, id: &str) -> Result<bool, StoreError> {
    if !matches!(
        table,
        "threads" | "messages" | "runs" | "conversation_turns"
    ) {
        return Err(StoreError::Internal("invalid entity table".into()));
    }
    connection
        .query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)"),
            [id],
            |row| row.get(0),
        )
        .map_err(map_sqlite)
}

fn query_thread(connection: &Connection, id: &ThreadId) -> Result<Thread, StoreError> {
    connection
        .query_row(
            &format!("SELECT {THREAD_COLUMNS} FROM threads WHERE id=?1"),
            [id.as_str()],
            mapping::thread_from_row,
        )
        .map_err(map_sqlite)
}

fn insert_fork_thread(
    connection: &Connection,
    thread: &Thread,
    credential_binding_id: &str,
    model_id: &str,
) -> Result<(), StoreError> {
    connection
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
    let changed = connection
        .execute(
            "UPDATE conversation_thread_identity
             SET credential_binding_id=?1,model_id=?2
             WHERE thread_id=?3 AND source=0
               AND credential_binding_id IS NULL AND model_id IS NULL",
            params![credential_binding_id, model_id, thread.id.as_str()],
        )
        .map_err(map_sqlite)?;
    if changed != 1 {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn insert_thread_fork(connection: &Connection, thread: &Thread) -> Result<(), StoreError> {
    let ConversationThreadOrigin::Fork {
        parent_thread_id,
        source_turn_id,
        source_message_id,
        kind,
    } = &thread.lineage.origin
    else {
        return Err(StoreError::Conflict);
    };
    let kind = match kind {
        ConversationForkKind::Branch => 0_i64,
        ConversationForkKind::EditAndBranch => 1_i64,
        ConversationForkKind::Regenerate => 2_i64,
    };
    connection
        .execute(
            "INSERT INTO conversation_thread_forks(
                 child_thread_id,parent_thread_id,root_thread_id,source_turn_id,
                 source_message_id,kind,fork_depth
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                thread.id.as_str(),
                parent_thread_id.as_str(),
                thread.lineage.root_thread_id.as_str(),
                source_turn_id.as_str(),
                source_message_id.as_str(),
                kind,
                number(u64::from(thread.lineage.fork_depth))?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn insert_message_derivation(connection: &Connection, message: &Message) -> Result<(), StoreError> {
    let ConversationMessageDerivation::Fork {
        kind,
        source_message_id,
        source_turn_id,
        source_context_sequence,
    } = &message.derivation
    else {
        return Err(StoreError::Conflict);
    };
    let kind = match kind {
        ConversationMessageDerivationKind::ContextCopy => 0_i64,
        ConversationMessageDerivationKind::SourceAssistantCopy => 1_i64,
        ConversationMessageDerivationKind::EditedUser => 2_i64,
    };
    connection
        .execute(
            "INSERT INTO conversation_message_derivations(
                 child_message_id,source_message_id,source_turn_id,kind,
                 source_context_sequence
             ) VALUES (?1,?2,?3,?4,?5)",
            params![
                message.id.as_str(),
                source_message_id.as_str(),
                source_turn_id.as_str(),
                kind,
                source_context_sequence.map(i64::from),
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn insert_inherited_outcome(
    connection: &Connection,
    child_message_id: &MessageId,
    source_turn_id: &ConversationTurnId,
) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO conversation_inherited_assistant_outcomes(
                 child_assistant_message_id,source_turn_id
             ) VALUES (?1,?2)",
            params![child_message_id.as_str(), source_turn_id.as_str()],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn query_fork_command(
    connection: &Connection,
    scope: &str,
    key: &str,
) -> Result<Option<StoredForkCommand>, StoreError> {
    connection
        .query_row(
            "SELECT request_fingerprint,source_turn_id,expected_source_revision,
                    child_thread_id,started_turn_id
             FROM conversation_fork_commands
             WHERE command_scope=?1 AND idempotency_key=?2",
            params![scope, key],
            |row| {
                Ok(StoredForkCommand {
                    command_scope: scope.to_owned(),
                    idempotency_key: key.to_owned(),
                    request_fingerprint: fingerprint(row, 0, false)?
                        .ok_or_else(|| conversion(0, "missing fork request fingerprint"))?,
                    source_turn_id: ConversationTurnId::new(row.get::<_, String>(1)?)
                        .map_err(|error| conversion(1, error))?,
                    expected_source_revision: unsigned(row, 2)?,
                    child_thread_id: ThreadId::new(row.get::<_, String>(3)?)
                        .map_err(|error| conversion(3, error))?,
                    started_turn_id: row
                        .get::<_, Option<String>>(4)?
                        .map(ConversationTurnId::new)
                        .transpose()
                        .map_err(|error| conversion(4, error))?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite)
}

fn query_fork_command_by_child(
    connection: &Connection,
    child_thread_id: &ThreadId,
) -> Result<Option<StoredForkCommand>, StoreError> {
    connection
        .query_row(
            "SELECT command_scope,idempotency_key,request_fingerprint,source_turn_id,
                    expected_source_revision,child_thread_id,started_turn_id
             FROM conversation_fork_commands WHERE child_thread_id=?1",
            [child_thread_id.as_str()],
            |row| {
                Ok(StoredForkCommand {
                    command_scope: row.get(0)?,
                    idempotency_key: row.get(1)?,
                    request_fingerprint: fingerprint(row, 2, false)?
                        .ok_or_else(|| conversion(2, "missing fork request fingerprint"))?,
                    source_turn_id: ConversationTurnId::new(row.get::<_, String>(3)?)
                        .map_err(|error| conversion(3, error))?,
                    expected_source_revision: unsigned(row, 4)?,
                    child_thread_id: ThreadId::new(row.get::<_, String>(5)?)
                        .map_err(|error| conversion(5, error))?,
                    started_turn_id: row
                        .get::<_, Option<String>>(6)?
                        .map(ConversationTurnId::new)
                        .transpose()
                        .map_err(|error| conversion(6, error))?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite)
}

fn query_fork_delivery_alias(
    connection: &Connection,
    scope: &str,
    key: &str,
) -> Result<Option<StoredForkDeliveryAlias>, StoreError> {
    connection
        .query_row(
            "SELECT request_fingerprint,child_thread_id
             FROM conversation_fork_delivery_aliases
             WHERE command_scope=?1 AND idempotency_key=?2",
            params![scope, key],
            |row| {
                Ok(StoredForkDeliveryAlias {
                    request_fingerprint: fingerprint(row, 0, false)?
                        .ok_or_else(|| conversion(0, "missing fork delivery alias fingerprint"))?,
                    child_thread_id: ThreadId::new(row.get::<_, String>(1)?)
                        .map_err(|error| conversion(1, error))?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite)
}

fn query_exact_fork_command(
    connection: &Connection,
    command: &MutationCommand,
) -> Result<Option<StoredForkCommand>, StoreError> {
    let canonical = query_fork_command(connection, &command.scope, &command.key)?;
    let alias = query_fork_delivery_alias(connection, &command.scope, &command.key)?;
    match (canonical, alias) {
        (Some(_), Some(_)) => Err(invalid_persisted_aggregate()),
        (Some(record), None) => {
            if record.request_fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            Ok(Some(record))
        }
        (None, Some(alias)) => {
            if alias.request_fingerprint != command.fingerprint {
                return Err(StoreError::Conflict);
            }
            let record = query_fork_command_by_child(connection, &alias.child_thread_id)?
                .ok_or_else(invalid_persisted_aggregate)?;
            if record.command_scope != command.scope
                || record.request_fingerprint != alias.request_fingerprint
                || record.child_thread_id != alias.child_thread_id
            {
                return Err(invalid_persisted_aggregate());
            }
            Ok(Some(record))
        }
        (None, None) => Ok(None),
    }
}

fn query_pending_fork_command(
    connection: &Connection,
    scope: &str,
    request_fingerprint: &[u8; 32],
) -> Result<Option<StoredForkCommand>, StoreError> {
    let (matching_deliveries, child_thread_id) = connection
        .query_row(
            "SELECT count(*),min(child_thread_id) FROM conversation_fork_deliveries
             WHERE command_scope=?1 AND request_fingerprint=?2
               AND state=0 AND revision=0",
            params![scope, request_fingerprint.as_slice()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .map_err(map_sqlite)?;
    if !(0..=1).contains(&matching_deliveries) {
        return Err(invalid_persisted_aggregate());
    }
    let Some(child_thread_id) = child_thread_id else {
        if matching_deliveries != 0 {
            return Err(invalid_persisted_aggregate());
        }
        return Ok(None);
    };
    if matching_deliveries != 1 {
        return Err(invalid_persisted_aggregate());
    }
    let child_thread_id =
        ThreadId::new(child_thread_id).map_err(|_| invalid_persisted_aggregate())?;
    let record = query_fork_command_by_child(connection, &child_thread_id)?
        .ok_or_else(invalid_persisted_aggregate)?;
    if record.command_scope != scope
        || record.request_fingerprint.as_slice() != request_fingerprint.as_slice()
        || record.child_thread_id != child_thread_id
    {
        return Err(invalid_persisted_aggregate());
    }
    Ok(Some(record))
}

fn insert_fork_command(
    connection: &Connection,
    plan: &ConversationForkPlan,
) -> Result<(), StoreError> {
    let kind = conversation_fork_kind(&plan.child_thread)?;
    if !conversation_fork_scope_matches(&plan.command.scope, kind) {
        return Err(StoreError::Conflict);
    }
    connection
        .execute(
            "INSERT INTO conversation_fork_commands(
                 command_scope,idempotency_key,request_fingerprint,source_turn_id,
                 expected_source_revision,child_thread_id,started_turn_id
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                plan.command.scope,
                plan.command.key,
                plan.command.fingerprint.as_slice(),
                plan.source_turn_id.as_str(),
                number(plan.expected_source_revision)?,
                plan.child_thread.id.as_str(),
                plan.started_turn
                    .as_ref()
                    .map(|turn_plan| turn_plan.turn.id.as_str()),
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn insert_fork_delivery_alias(
    connection: &Connection,
    command: &MutationCommand,
    child_thread_id: &ThreadId,
) -> Result<(), StoreError> {
    let alias_count: usize = connection
        .query_row(
            "SELECT count(*) FROM conversation_fork_delivery_aliases
             WHERE child_thread_id=?1",
            [child_thread_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_sqlite)?
        .try_into()
        .map_err(|_| invalid_persisted_aggregate())?;
    if alias_count > MAX_CONVERSATION_FORK_DELIVERY_ALIASES {
        return Err(invalid_persisted_aggregate());
    }
    if alias_count == MAX_CONVERSATION_FORK_DELIVERY_ALIASES {
        return Err(StoreError::Conflict);
    }
    connection
        .execute(
            "INSERT INTO conversation_fork_delivery_aliases(
                 command_scope,idempotency_key,request_fingerprint,child_thread_id
             ) VALUES (?1,?2,?3,?4)",
            params![
                command.scope,
                command.key,
                command.fingerprint.as_slice(),
                child_thread_id.as_str(),
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn query_fork_delivery(
    connection: &Connection,
    record: &StoredForkCommand,
) -> Result<ConversationForkDelivery, StoreError> {
    let (command_scope, request_fingerprint, state, revision) = connection
        .query_row(
            "SELECT command_scope,request_fingerprint,state,revision
             FROM conversation_fork_deliveries WHERE child_thread_id=?1",
            [record.child_thread_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    fingerprint(row, 1, false)?.ok_or_else(|| {
                        conversion(1, "missing fork delivery request fingerprint")
                    })?,
                    row.get::<_, i64>(2)?,
                    unsigned(row, 3)?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?
        .ok_or_else(invalid_persisted_aggregate)?;
    let delivery_state = match (state, revision) {
        (0, 0) => ConversationForkDeliveryState::Pending,
        (1, 1) => ConversationForkDeliveryState::Acknowledged,
        _ => return Err(invalid_persisted_aggregate()),
    };
    if command_scope != record.command_scope
        || request_fingerprint != record.request_fingerprint
        || query_fork_command_by_child(connection, &record.child_thread_id)?
            .as_ref()
            .is_none_or(|canonical| canonical != record)
    {
        return Err(invalid_persisted_aggregate());
    }
    let (alias_count, invalid_alias_count): (i64, i64) = connection
        .query_row(
            "SELECT count(*),coalesce(sum(CASE
                 WHEN alias.command_scope!=?2
                   OR alias.request_fingerprint!=?3
                   OR EXISTS (
                       SELECT 1 FROM conversation_fork_commands command
                       WHERE command.command_scope=alias.command_scope
                         AND command.idempotency_key=alias.idempotency_key
                   )
                 THEN 1 ELSE 0 END),0)
             FROM conversation_fork_delivery_aliases alias
             WHERE alias.child_thread_id=?1",
            params![
                record.child_thread_id.as_str(),
                record.command_scope,
                record.request_fingerprint.as_slice(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(map_sqlite)?;
    if alias_count < 0
        || usize::try_from(alias_count)
            .map_or(true, |count| count > MAX_CONVERSATION_FORK_DELIVERY_ALIASES)
        || invalid_alias_count != 0
    {
        return Err(invalid_persisted_aggregate());
    }
    Ok(ConversationForkDelivery {
        child_thread_id: record.child_thread_id.clone(),
        state: delivery_state,
        revision,
    })
}

fn query_fork_delivery_for_acknowledgement(
    connection: &Connection,
    child_thread_id: &ThreadId,
) -> Result<(StoredForkCommand, ConversationForkDelivery), StoreError> {
    let Some(record) = query_fork_command_by_child(connection, child_thread_id)? else {
        let has_fork_material: bool = connection
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM conversation_thread_forks WHERE child_thread_id=?1
                     UNION ALL
                     SELECT 1 FROM conversation_fork_deliveries WHERE child_thread_id=?1
                 )",
                [child_thread_id.as_str()],
                |row| row.get(0),
            )
            .map_err(map_sqlite)?;
        if has_fork_material {
            return Err(invalid_persisted_aggregate());
        }
        return if entity_exists(connection, "threads", child_thread_id.as_str())? {
            Err(StoreError::Conflict)
        } else {
            Err(StoreError::NotFound)
        };
    };
    let delivery = query_fork_delivery(connection, &record)?;
    if delivery.child_thread_id != *child_thread_id {
        return Err(invalid_persisted_aggregate());
    }
    Ok((record, delivery))
}

fn query_fork_delivery_ack_command(
    connection: &Connection,
    scope: &str,
    key: &str,
) -> Result<Option<StoredForkDeliveryAckCommand>, StoreError> {
    connection
        .query_row(
            "SELECT request_fingerprint,child_thread_id,expected_delivery_revision,
                    resulting_delivery_revision
             FROM conversation_fork_delivery_ack_commands
             WHERE command_scope=?1 AND idempotency_key=?2",
            params![scope, key],
            |row| {
                Ok(StoredForkDeliveryAckCommand {
                    request_fingerprint: fingerprint(row, 0, false)?.ok_or_else(|| {
                        conversion(0, "missing fork delivery acknowledgement fingerprint")
                    })?,
                    child_thread_id: ThreadId::new(row.get::<_, String>(1)?)
                        .map_err(|error| conversion(1, error))?,
                    expected_delivery_revision: unsigned(row, 2)?,
                    resulting_delivery_revision: unsigned(row, 3)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite)
}

fn insert_fork_delivery_ack_command(
    connection: &Connection,
    command: &MutationCommand,
    child_thread_id: &ThreadId,
    expected_delivery_revision: u64,
    resulting_delivery_revision: u64,
) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO conversation_fork_delivery_ack_commands(
                 command_scope,idempotency_key,request_fingerprint,child_thread_id,
                 expected_delivery_revision,resulting_delivery_revision
             ) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                command.scope,
                command.key,
                command.fingerprint.as_slice(),
                child_thread_id.as_str(),
                number(expected_delivery_revision)?,
                number(resulting_delivery_revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn query_fork_context(
    connection: &Connection,
    record: &StoredForkCommand,
) -> Result<Option<Vec<Message>>, StoreError> {
    record
        .started_turn_id
        .as_ref()
        .map(|turn_id| {
            let turn = query_turn(connection, turn_id)?;
            let snapshot = query_snapshot(connection, &turn)?;
            let context = query_context(connection, turn_id)?;
            validate_turn_context(&context, &snapshot)?;
            Ok(context)
        })
        .transpose()
}

fn query_fork_snapshot(
    connection: &Connection,
    record: &StoredForkCommand,
) -> Result<ConversationForkSnapshot, StoreError> {
    let child_thread = query_thread(connection, &record.child_thread_id)?;
    let kind = conversation_fork_kind(&child_thread).map_err(|_| invalid_persisted_aggregate())?;
    let ConversationThreadOrigin::Fork { source_turn_id, .. } = &child_thread.lineage.origin else {
        return Err(invalid_persisted_aggregate());
    };
    if source_turn_id != &record.source_turn_id {
        return Err(invalid_persisted_aggregate());
    }
    let source_turn = query_turn(connection, &record.source_turn_id)?;
    if source_turn.revision != record.expected_source_revision {
        return Err(invalid_persisted_aggregate());
    }
    let mut statement = connection
        .prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE thread_id=?1 AND EXISTS (
                 SELECT 1 FROM conversation_message_derivations derivation
                 WHERE derivation.child_message_id=messages.id
             ) ORDER BY sequence"
        ))
        .map_err(map_sqlite)?;
    let messages = statement
        .query_map([child_thread.id.as_str()], mapping::message_from_row)
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    drop(statement);
    if messages.is_empty()
        || messages.iter().enumerate().any(|(index, message)| {
            Message::restore(message.clone()).is_err()
                || message.sequence != u64::try_from(index + 1).unwrap_or(u64::MAX)
        })
    {
        return Err(invalid_persisted_aggregate());
    }
    validate_persisted_fork_messages(connection, &child_thread, &messages)?;
    let started_turn = record
        .started_turn_id
        .as_ref()
        .map(|turn_id| {
            let turn = query_turn(connection, turn_id)?;
            query_snapshot(connection, &turn)
        })
        .transpose()?;
    if (kind == ConversationForkKind::Branch) != started_turn.is_none()
        || started_turn.as_ref().is_some_and(|turn| {
            let origin_matches = match (&turn.lineage.origin, kind) {
                (
                    ConversationTurnOrigin::EditAndBranch { source_turn_id },
                    ConversationForkKind::EditAndBranch,
                )
                | (
                    ConversationTurnOrigin::Regenerate { source_turn_id },
                    ConversationForkKind::Regenerate,
                ) => source_turn_id == &record.source_turn_id,
                _ => false,
            };
            turn.turn.thread_id != child_thread.id
                || turn.turn.request_fingerprint != record.request_fingerprint
                || turn.turn.idempotency_key != record.idempotency_key
                || !origin_matches
        })
    {
        return Err(invalid_persisted_aggregate());
    }
    let delivery = query_fork_delivery(connection, record)?;
    Ok(ConversationForkSnapshot {
        child_thread,
        messages,
        started_turn,
        delivery,
    })
}

fn validate_persisted_fork_messages(
    connection: &Connection,
    child: &Thread,
    messages: &[Message],
) -> Result<(), StoreError> {
    let ConversationThreadOrigin::Fork { source_turn_id, .. } = &child.lineage.origin else {
        return Err(invalid_persisted_aggregate());
    };
    let source_turn = query_turn(connection, source_turn_id)?;
    let source = query_snapshot(connection, &source_turn)?;
    let source_context = query_context(connection, source_turn_id)?;
    validate_persisted_fork_messages_with_source(
        connection,
        child,
        messages,
        &source,
        &source_context,
    )
}

#[allow(clippy::too_many_lines)]
fn validate_persisted_fork_messages_with_source(
    connection: &Connection,
    child: &Thread,
    messages: &[Message],
    source: &ConversationTurnSnapshot,
    source_context: &[Message],
) -> Result<(), StoreError> {
    let ConversationThreadOrigin::Fork {
        parent_thread_id,
        source_turn_id,
        source_message_id,
        kind,
    } = &child.lineage.origin
    else {
        return Err(invalid_persisted_aggregate());
    };
    if source.turn.id != *source_turn_id {
        return Err(invalid_persisted_aggregate());
    }
    validate_fork_source_context(source, source_context)
        .map_err(|_| invalid_persisted_aggregate())?;
    if source.turn.thread_id != *parent_thread_id {
        return Err(invalid_persisted_aggregate());
    }
    let context_count = if *kind == ConversationForkKind::EditAndBranch {
        source_context
            .len()
            .checked_sub(1)
            .ok_or_else(invalid_persisted_aggregate)?
    } else {
        source_context.len()
    };
    let expected_count = context_count
        + usize::from(matches!(
            kind,
            ConversationForkKind::Branch | ConversationForkKind::EditAndBranch
        ));
    if messages.len() != expected_count {
        return Err(invalid_persisted_aggregate());
    }
    for (index, (message, source_message)) in messages
        .iter()
        .take(context_count)
        .zip(source_context.iter())
        .enumerate()
    {
        let ConversationMessageDerivation::Fork {
            kind: derivation_kind,
            source_message_id: derivation_source_message_id,
            source_turn_id: derivation_source_turn_id,
            source_context_sequence,
        } = &message.derivation
        else {
            return Err(invalid_persisted_aggregate());
        };
        if *derivation_kind != ConversationMessageDerivationKind::ContextCopy
            || derivation_source_message_id != &source_message.id
            || derivation_source_turn_id != source_turn_id
            || *source_context_sequence != u32::try_from(index + 1).ok()
            || message.role != source_message.role
            || message.content != source_message.content
            || message.thread_id != child.id
        {
            return Err(invalid_persisted_aggregate());
        }
    }
    match kind {
        ConversationForkKind::Branch => {
            let message = messages.last().ok_or_else(invalid_persisted_aggregate)?;
            let assistant = source
                .assistant_message
                .as_ref()
                .ok_or_else(invalid_persisted_aggregate)?;
            if source_message_id != &assistant.id
                || message.role != MessageRole::Assistant
                || message.content != assistant.content
                || message.derivation
                    != (ConversationMessageDerivation::Fork {
                        kind: ConversationMessageDerivationKind::SourceAssistantCopy,
                        source_message_id: assistant.id.clone(),
                        source_turn_id: source_turn_id.clone(),
                        source_context_sequence: None,
                    })
            {
                return Err(invalid_persisted_aggregate());
            }
        }
        ConversationForkKind::EditAndBranch => {
            let message = messages.last().ok_or_else(invalid_persisted_aggregate)?;
            if source_message_id != &source.user_message.id
                || message.role != MessageRole::User
                || message.content == source.user_message.content
                || message.derivation
                    != (ConversationMessageDerivation::Fork {
                        kind: ConversationMessageDerivationKind::EditedUser,
                        source_message_id: source.user_message.id.clone(),
                        source_turn_id: source_turn_id.clone(),
                        source_context_sequence: u32::try_from(source_context.len()).ok(),
                    })
            {
                return Err(invalid_persisted_aggregate());
            }
        }
        ConversationForkKind::Regenerate => {
            let assistant = source
                .assistant_message
                .as_ref()
                .ok_or_else(invalid_persisted_aggregate)?;
            if source_message_id != &assistant.id {
                return Err(invalid_persisted_aggregate());
            }
        }
    }
    for message in messages
        .iter()
        .filter(|message| message.role == MessageRole::Assistant)
    {
        let outcome_source = inherited_source_turn(connection, message)?;
        if matches!(
            &message.derivation,
            ConversationMessageDerivation::Fork {
                kind: ConversationMessageDerivationKind::SourceAssistantCopy,
                ..
            }
        ) && outcome_source != *source_turn_id
        {
            return Err(invalid_persisted_aggregate());
        }
    }
    Ok(())
}

#[allow(clippy::too_many_lines)]
fn query_fork_metadata(
    connection: &Connection,
    thread_id: &ThreadId,
) -> Result<ConversationForkMetadata, StoreError> {
    let thread = query_thread(connection, thread_id).map_err(|error| match error {
        StoreError::NotFound => StoreError::NotFound,
        other => other,
    })?;
    let family_threads = query_fork_family_threads(connection, &thread.lineage.root_thread_id)?;
    if family_threads.is_empty()
        || family_threads.len() > MAX_CONVERSATION_FORK_FAMILY_THREADS
        || family_threads.iter().any(|candidate| {
            candidate.project_id != thread.project_id
                || candidate.lineage.root_thread_id != thread.lineage.root_thread_id
                || Thread::restore(candidate.clone()).is_err()
        })
    {
        return Err(invalid_persisted_aggregate());
    }
    for candidate in &family_threads {
        let ConversationThreadOrigin::Fork {
            parent_thread_id,
            source_turn_id,
            source_message_id,
            kind,
        } = &candidate.lineage.origin
        else {
            continue;
        };
        let parent = family_threads
            .iter()
            .find(|family| &family.id == parent_thread_id)
            .ok_or_else(invalid_persisted_aggregate)?;
        let source_turn = query_turn(connection, source_turn_id)?;
        let source_message = query_message(connection, source_message_id)?;
        if parent.lineage.root_thread_id != candidate.lineage.root_thread_id
            || parent.lineage.fork_depth.checked_add(1) != Some(candidate.lineage.fork_depth)
            || source_turn.thread_id != parent.id
            || source_message.thread_id != parent.id
            || source_message.role != kind.source_message_role()
            || (match kind {
                ConversationForkKind::Branch | ConversationForkKind::Regenerate => {
                    source_turn.assistant_message_id.as_ref()
                }
                ConversationForkKind::EditAndBranch => Some(&source_turn.user_message_id),
            } != Some(source_message_id))
        {
            return Err(invalid_persisted_aggregate());
        }
    }

    validate_persisted_fork_metadata_storage_budget(connection, thread_id)?;
    let mut statement = connection
        .prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE thread_id=?1 AND role=2 AND EXISTS (
                 SELECT 1 FROM conversation_message_derivations derivation
                 WHERE derivation.child_message_id=messages.id
             ) ORDER BY sequence"
        ))
        .map_err(map_sqlite)?;
    let derived_assistants = statement
        .query_map([thread_id.as_str()], mapping::message_from_row)
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    drop(statement);
    let inherited_assistant_outcomes = derived_assistants
        .iter()
        .map(|message| persisted_inherited_outcome(connection, message))
        .collect::<Result<Vec<_>, _>>()?;
    let unexpected_outcome: bool = connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1
                 FROM conversation_inherited_assistant_outcomes outcome
                 JOIN messages child ON child.id=outcome.child_assistant_message_id
                 LEFT JOIN conversation_message_derivations derivation
                   ON derivation.child_message_id=child.id
                 WHERE child.thread_id=?1
                   AND (child.role!=2 OR derivation.child_message_id IS NULL)
             )",
            [thread_id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    if unexpected_outcome {
        return Err(invalid_persisted_aggregate());
    }
    let metadata = ConversationForkMetadata {
        lineage: thread.lineage,
        inherited_assistant_outcomes,
        family_threads,
    };
    if !conversation_fork_metadata_is_within_bounds(&metadata) {
        return Err(invalid_persisted_aggregate());
    }
    Ok(metadata)
}

fn query_fork_family_threads(
    connection: &Connection,
    root_thread_id: &ThreadId,
) -> Result<Vec<Thread>, StoreError> {
    let mut statement = connection
        .prepare(&format!(
            "SELECT {THREAD_COLUMNS} FROM threads
             WHERE id=?1 OR EXISTS (
                 SELECT 1 FROM conversation_thread_forks family
                 WHERE family.child_thread_id=threads.id AND family.root_thread_id=?1
             )
             ORDER BY COALESCE((
                 SELECT fork_depth FROM conversation_thread_forks family
                 WHERE family.child_thread_id=threads.id
             ),0),created_at,id"
        ))
        .map_err(map_sqlite)?;
    let threads = statement
        .query_map([root_thread_id.as_str()], mapping::thread_from_row)
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    drop(statement);
    Ok(threads)
}

fn validate_persisted_fork_metadata_storage_budget(
    connection: &Connection,
    thread_id: &ThreadId,
) -> Result<(), StoreError> {
    let (count, bytes): (i64, i64) = connection
        .query_row(
            "SELECT count(*),COALESCE(SUM(
                 256
                 + length(CAST(child.id AS BLOB))
                 + COALESCE(length(CAST(outcome.source_turn_id AS BLOB)),0)
                 + COALESCE(length(CAST(source.model_id AS BLOB)),0)
                 + COALESCE(length(CAST(source.citations_json AS BLOB)),0)
             ),0)
             FROM messages child
             JOIN conversation_message_derivations derivation
               ON derivation.child_message_id=child.id
             LEFT JOIN conversation_inherited_assistant_outcomes outcome
               ON outcome.child_assistant_message_id=child.id
             LEFT JOIN conversation_turns source ON source.id=outcome.source_turn_id
             WHERE child.thread_id=?1 AND child.role=2",
            [thread_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(map_sqlite)?;
    let count = usize::try_from(count).map_err(|_| invalid_persisted_aggregate())?;
    let bytes = usize::try_from(bytes).map_err(|_| invalid_persisted_aggregate())?;
    if count > MAX_CONVERSATION_FORK_INHERITED_OUTCOMES
        || bytes > MAX_CONVERSATION_FORK_METADATA_BYTES
    {
        return Err(invalid_persisted_aggregate());
    }
    Ok(())
}

fn persisted_inherited_outcome(
    connection: &Connection,
    child_message: &Message,
) -> Result<ConversationInheritedAssistantOutcome, StoreError> {
    let stored_source_turn_id = inherited_outcome_turn_id(connection, &child_message.id)?
        .ok_or_else(invalid_persisted_aggregate)?;
    let resolved_source_turn_id = inherited_source_turn(connection, child_message)?;
    if stored_source_turn_id != resolved_source_turn_id {
        return Err(invalid_persisted_aggregate());
    }
    let source_turn = query_turn(connection, &stored_source_turn_id)?;
    Ok(ConversationInheritedAssistantOutcome {
        child_assistant_message_id: child_message.id.clone(),
        source_turn_id: stored_source_turn_id,
        model_id: source_turn.model_id,
        citations: source_turn.citations,
        usage: source_turn.usage,
        zero_data_retention: source_turn.zero_data_retention,
    })
}

async fn commit_scoped_cancellation(
    store: &SqlCipherStore,
    mut cancellation: CancelConversationTurnCommit,
    expected_scope: &'static str,
) -> Result<ConversationTurnSnapshot, StoreError> {
    if cancellation.command.scope != expected_scope {
        return Err(StoreError::Conflict);
    }
    store
        .with_store(move |connection| {
            let transaction = begin(connection)?;
            if let Some(record) = query_cancellation_command(
                &transaction,
                &cancellation.command.scope,
                &cancellation.command.key,
            )? {
                if record.request_fingerprint != cancellation.command.fingerprint
                    || record.turn_id != cancellation.turn_id
                {
                    return Err(StoreError::Conflict);
                }
                let turn = query_turn(&transaction, &record.turn_id)?;
                let snapshot = query_snapshot(&transaction, &turn)?;
                if !snapshot.turn.state.is_terminal()
                    || snapshot.turn.state != record.outcome_state
                    || snapshot.turn.revision != record.outcome_revision
                {
                    return Err(invalid_persisted_aggregate());
                }
                commit(transaction)?;
                return Ok(snapshot);
            }

            let current_turn = query_turn(&transaction, &cancellation.turn_id)?;
            let current = query_snapshot(&transaction, &current_turn)?;
            let outcome = if current.turn.state.is_terminal() {
                let winner_revision = cancellation
                    .expected_turn_revision
                    .checked_add(1)
                    .ok_or(StoreError::Conflict)?;
                if current.turn.revision != winner_revision {
                    return Err(StoreError::Conflict);
                }
                current
            } else {
                if current.turn.revision != cancellation.expected_turn_revision {
                    return Err(StoreError::Conflict);
                }
                let terminal = cancellation.terminal.as_mut().ok_or(StoreError::Conflict)?;
                if terminal.turn.id != cancellation.turn_id
                    || terminal.expected_turn_revision != cancellation.expected_turn_revision
                    || !is_exact_cancellation_edge(current.turn.state, terminal.turn.state)
                {
                    return Err(StoreError::Conflict);
                }
                commit_terminal_in_transaction(&transaction, terminal)?
            };
            insert_cancellation_command(
                &transaction,
                &cancellation.command,
                &cancellation.turn_id,
                &outcome.turn,
            )?;
            commit(transaction)?;
            Ok(outcome)
        })
        .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StoredCancellationCommand {
    request_fingerprint: [u8; 32],
    turn_id: ConversationTurnId,
    outcome_state: ConversationTurnState,
    outcome_revision: u64,
}

fn commit_terminal_in_transaction(
    transaction: &Transaction<'_>,
    commit_value: &mut TerminalTurnCommit,
) -> Result<ConversationTurnSnapshot, StoreError> {
    let current_turn = query_turn(transaction, &commit_value.turn.id)?;
    let (current, mut turn_events) = query_snapshot_with_events(transaction, &current_turn)?;
    if !valid_terminal_commit(&current, commit_value) {
        return Err(StoreError::Conflict);
    }

    let turn_event = turn_events
        .append_kind(commit_value.turn_event.clone())
        .map_err(|_| StoreError::Conflict)?;
    turn_events
        .validate_snapshot(
            &commit_value.turn,
            commit_value
                .assistant_message
                .as_ref()
                .map(|message| message.content.as_str()),
        )
        .map_err(|_| StoreError::Conflict)?;

    if let Some(assistant) = commit_value.assistant_message.as_mut() {
        assistant.sequence = transaction
            .query_row(
                "SELECT COALESCE(MAX(sequence),0)+1 FROM messages WHERE thread_id=?1",
                [assistant.thread_id.as_str()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(map_sqlite)?
            .try_into()
            .map_err(|_| StoreError::Internal("message sequence exhausted".into()))?;
        insert_message(transaction, assistant)?;
    }
    match (
        commit_value.effect.as_ref(),
        commit_value.expected_effect_revision,
    ) {
        (Some(effect), Some(expected_revision)) => {
            update_effect(transaction, effect, expected_revision)?;
        }
        (None, None) => {}
        _ => return Err(StoreError::Conflict),
    }
    update_run(
        transaction,
        &commit_value.run,
        commit_value.expected_run_revision,
    )?;
    update_turn(
        transaction,
        &commit_value.turn,
        commit_value.expected_turn_revision,
    )?;
    insert_turn_event(transaction, &turn_event)?;
    append_events(
        transaction,
        &commit_value.run.id,
        std::mem::take(&mut commit_value.events),
    )?;

    let persisted_turn = query_turn(transaction, &commit_value.turn.id)?;
    query_snapshot(transaction, &persisted_turn)
}

const fn is_exact_cancellation_edge(
    from: ConversationTurnState,
    to: ConversationTurnState,
) -> bool {
    matches!(
        (from, to),
        (
            ConversationTurnState::Reserved,
            ConversationTurnState::Cancelled
        ) | (
            ConversationTurnState::ProviderStarted,
            ConversationTurnState::InterruptedNeedsReview
        )
    )
}

fn query_cancellation_command(
    connection: &Connection,
    scope: &str,
    key: &str,
) -> Result<Option<StoredCancellationCommand>, StoreError> {
    connection
        .query_row(
            "SELECT request_fingerprint,turn_id,outcome_state,outcome_revision
             FROM conversation_turn_cancel_commands
             WHERE command_scope=?1 AND idempotency_key=?2",
            params![scope, key],
            |row| {
                Ok(StoredCancellationCommand {
                    request_fingerprint: fingerprint(row, 0, false)?
                        .ok_or_else(|| conversion(0, "missing cancellation request fingerprint"))?,
                    turn_id: ConversationTurnId::new(row.get::<_, String>(1)?)
                        .map_err(|error| conversion(1, error))?,
                    outcome_state: turn_state_from_i64(row.get(2)?)
                        .map_err(|error| conversion(2, error))?,
                    outcome_revision: unsigned(row, 3)?,
                })
            },
        )
        .optional()
        .map_err(map_sqlite)
}

fn insert_cancellation_command(
    connection: &Connection,
    command: &MutationCommand,
    turn_id: &ConversationTurnId,
    outcome: &ConversationTurn,
) -> Result<(), StoreError> {
    if !matches!(
        command.scope.as_str(),
        CONVERSATION_CANCEL_COMMAND_SCOPE | CONVERSATION_RECONCILIATION_COMMAND_SCOPE
    ) || outcome.id != *turn_id
        || !outcome.state.is_terminal()
    {
        return Err(StoreError::Conflict);
    }
    connection
        .execute(
            "INSERT INTO conversation_turn_cancel_commands(
                 command_scope,idempotency_key,request_fingerprint,turn_id,
                 outcome_state,outcome_revision
             ) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                command.scope,
                command.key,
                command.fingerprint.as_slice(),
                turn_id.as_str(),
                turn_state_to_i64(outcome.state),
                number(outcome.revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn validate_reservation_links(
    connection: &Connection,
    turn: &ConversationTurn,
    user: &Message,
    run: &Run,
    event: &NewRunEvent,
    turn_event: &ConversationTurnEventKind,
) -> Result<(), StoreError> {
    let active: Option<(String, i64, i64)> = connection
        .query_row(
            "SELECT threads.project_id,threads.state,projects.state FROM threads
             JOIN projects ON projects.id=threads.project_id WHERE threads.id=?1",
            [turn.thread_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(map_sqlite)?;
    let canonical_turn = ConversationTurn::reserve(
        turn.id.clone(),
        turn.idempotency_key.clone(),
        turn.request_fingerprint,
        turn.project_id.clone(),
        turn.thread_id.clone(),
        turn.user_message_id.clone(),
        turn.run_id.clone(),
        turn.model_id.clone(),
        turn.created_at,
    )
    .ok();
    let canonical_user = Message::new(
        user.id.clone(),
        user.thread_id.clone(),
        MessageRole::User,
        user.content.clone(),
        turn.created_at,
    )
    .ok();
    let canonical_run = Run::queued(
        run.id.clone(),
        run.project_id.clone(),
        run.thread_id.clone(),
        turn.created_at,
    );
    let canonical_event = NewRunEvent {
        occurred_at: turn.created_at,
        kind: RunEventKind::Created,
    };
    if !matches!(
        active,
        Some((ref project_id, 0, 0)) if project_id == turn.project_id.as_str()
    ) || canonical_turn.as_ref() != Some(turn)
        || canonical_user.as_ref() != Some(user)
        || canonical_run != *run
        || canonical_event != *event
        || *turn_event != ConversationTurnEventKind::Created
        || user.id != turn.user_message_id
        || user.thread_id != turn.thread_id
        || run.id != turn.run_id
        || run.project_id != turn.project_id
        || run.thread_id != turn.thread_id
    {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn reservation_source_matches_lineage(
    source: &ConversationTurnReservationSource,
    lineage: &ConversationTurnLineage,
) -> bool {
    match (source, &lineage.origin) {
        (ConversationTurnReservationSource::CurrentThread, ConversationTurnOrigin::Original) => {
            true
        }
        (
            ConversationTurnReservationSource::Retry { source_turn_id, .. },
            ConversationTurnOrigin::Retry {
                source_turn_id: lineage_source,
            },
        ) => source_turn_id == lineage_source,
        _ => false,
    }
}

fn lineage_matches_command_scope(lineage: &ConversationTurnLineage, scope: &str) -> bool {
    matches!(
        (&lineage.origin, scope),
        (ConversationTurnOrigin::Original, CONVERSATION_COMMAND_SCOPE)
            | (
                ConversationTurnOrigin::Retry { .. },
                CONVERSATION_RETRY_COMMAND_SCOPE
            )
            | (
                ConversationTurnOrigin::EditAndBranch { .. },
                CONVERSATION_EDIT_BRANCH_COMMAND_SCOPE
            )
            | (
                ConversationTurnOrigin::Regenerate { .. },
                CONVERSATION_REGENERATE_COMMAND_SCOPE
            )
    )
}

fn query_thread_credential_binding(
    connection: &Connection,
    thread_id: &ThreadId,
) -> Result<Option<String>, StoreError> {
    let identity = connection
        .query_row(
            "SELECT source,credential_binding_id
             FROM conversation_thread_identity WHERE thread_id=?1",
            [thread_id.as_str()],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
        )
        .optional()
        .map_err(map_sqlite)?;
    let Some((source, credential_binding_id)) = identity else {
        return Err(invalid_persisted_aggregate());
    };
    if source != 0 {
        return Err(invalid_persisted_aggregate());
    }
    Ok(credential_binding_id)
}

fn query_thread_model_binding(
    connection: &Connection,
    thread_id: &ThreadId,
) -> Result<Option<String>, StoreError> {
    connection
        .query_row(
            "SELECT model_id FROM conversation_thread_identity
             WHERE thread_id=?1 AND source=0",
            [thread_id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(map_sqlite)?
        .ok_or_else(invalid_persisted_aggregate)
}

fn bind_or_validate_thread_identity(
    connection: &Connection,
    thread_id: &ThreadId,
    lineage: &ConversationTurnLineage,
    model_id: &str,
) -> Result<(), StoreError> {
    let requested_binding = lineage
        .credential_binding_id
        .as_deref()
        .ok_or(StoreError::Conflict)?;
    let credential_binding = query_thread_credential_binding(connection, thread_id)?;
    let model_binding = query_thread_model_binding(connection, thread_id)?;
    match (credential_binding, model_binding) {
        (Some(bound_credential), Some(bound_model))
            if bound_credential == requested_binding && bound_model == model_id =>
        {
            Ok(())
        }
        (Some(_), _) | (None, Some(_)) => Err(StoreError::Conflict),
        (None, None) => {
            let has_historical_turn: bool = connection
                .query_row(
                    "SELECT EXISTS(
                         SELECT 1 FROM conversation_turns WHERE thread_id=?1
                     )",
                    [thread_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if has_historical_turn {
                return Err(StoreError::Conflict);
            }
            let changed = connection
                .execute(
                    "UPDATE conversation_thread_identity
                     SET credential_binding_id=?1,model_id=?2
                     WHERE thread_id=?3
                       AND credential_binding_id IS NULL AND model_id IS NULL",
                    params![requested_binding, model_id, thread_id.as_str()],
                )
                .map_err(map_sqlite)?;
            if changed == 1 {
                Ok(())
            } else {
                Err(StoreError::Conflict)
            }
        }
    }
}

fn next_message_sequence(connection: &Connection, thread_id: &ThreadId) -> Result<u64, StoreError> {
    connection
        .query_row(
            "SELECT COALESCE(MAX(sequence),0)+1 FROM messages WHERE thread_id=?1",
            [thread_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_sqlite)?
        .try_into()
        .map_err(|_| StoreError::Internal("message sequence exhausted".into()))
}

#[allow(clippy::too_many_arguments)]
fn capture_retry_context(
    connection: &Connection,
    turn: &ConversationTurn,
    lineage: &ConversationTurnLineage,
    source_turn_id: &ConversationTurnId,
    expected_source_revision: u64,
    mut user_message: Message,
) -> Result<(Message, Vec<Message>), StoreError> {
    let source_turn = query_turn(connection, source_turn_id)?;
    let source = query_snapshot(connection, &source_turn)?;
    let thread_binding = query_thread_credential_binding(connection, &source.turn.thread_id)?;
    let source_context = query_context(connection, source_turn_id)?;
    validate_turn_context(&source_context, &source)?;
    let source_user = &source.user_message;
    let eligible_state = source.turn.state == ConversationTurnState::Cancelled
        || (source.turn.state == ConversationTurnState::Failed
            && source
                .turn
                .failure
                .as_ref()
                .is_some_and(|failure| failure.retryable));
    let newest_sequence: u64 = connection
        .query_row(
            "SELECT COALESCE(MAX(sequence),0) FROM messages WHERE thread_id=?1",
            [source.turn.thread_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .map_err(map_sqlite)?
        .try_into()
        .map_err(|_| invalid_persisted_aggregate())?;
    let already_retried: bool = connection
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM conversation_turn_lineage
                 WHERE origin=1 AND source_turn_id=?1
             )",
            [source_turn_id.as_str()],
            |row| row.get(0),
        )
        .map_err(map_sqlite)?;
    let expected_retry_depth = source
        .lineage
        .retry_depth
        .checked_add(1)
        .filter(|depth| *depth <= 64);
    if source.turn.revision != expected_source_revision
        || !eligible_state
        || source.turn.thread_id != turn.thread_id
        || source.turn.project_id != turn.project_id
        || source.turn.model_id != turn.model_id
        || newest_sequence != source_user.sequence
        || already_retried
        || source.lineage.credential_binding_id.is_none()
        || source.lineage.credential_binding_id != lineage.credential_binding_id
        || source.lineage.rail != lineage.rail
        || thread_binding != lineage.credential_binding_id
        || expected_retry_depth != Some(lineage.retry_depth)
        || user_message.content != source_user.content
    {
        return Err(StoreError::Conflict);
    }

    user_message.sequence = source_user
        .sequence
        .checked_add(1)
        .ok_or(StoreError::Conflict)?;
    let mut context = source_context;
    if context.last() != Some(source_user) {
        return Err(invalid_persisted_aggregate());
    }
    context.pop();
    context.push(user_message.clone());
    validate_context(&context)?;
    Ok((user_message, context))
}

fn query_active_messages(
    connection: &Connection,
    thread_id: &ThreadId,
) -> Result<Vec<Message>, StoreError> {
    let eligibility = "thread_id=?1 AND state=0
        AND NOT EXISTS (
            SELECT 1 FROM conversation_turns turns
            WHERE turns.user_message_id=messages.id AND turns.state!=2
        )";
    let (count, total_bytes, largest_message): (i64, i64, i64) = connection
        .query_row(
            &format!(
                "SELECT COUNT(*),
                        COALESCE(SUM(length(CAST(content AS BLOB))),0),
                        COALESCE(MAX(length(CAST(content AS BLOB))),0)
                 FROM messages WHERE {eligibility}"
            ),
            [thread_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(map_sqlite)?;
    let count = usize::try_from(count).map_err(|_| invalid_persisted_aggregate())?;
    let total_bytes = usize::try_from(total_bytes).map_err(|_| invalid_persisted_aggregate())?;
    let largest_message =
        usize::try_from(largest_message).map_err(|_| invalid_persisted_aggregate())?;
    if largest_message > MAX_MESSAGE_BYTES {
        return Err(invalid_persisted_aggregate());
    }
    if count > MAX_CONVERSATION_CONTEXT_MESSAGES || total_bytes > MAX_CONVERSATION_CONTEXT_BYTES {
        return Err(StoreError::Conflict);
    }
    let mut statement = connection
        .prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE {eligibility}
             ORDER BY sequence"
        ))
        .map_err(map_sqlite)?;
    let messages = statement
        .query_map([thread_id.as_str()], mapping::message_from_row)
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    drop(statement);
    if !context_entries_are_canonical(&messages) {
        return Err(invalid_persisted_aggregate());
    }
    validate_thread_fork_material(connection, thread_id)?;
    validate_context_source_turns(connection, &messages)?;
    Ok(messages)
}

fn validate_thread_fork_material(
    connection: &Connection,
    thread_id: &ThreadId,
) -> Result<(), StoreError> {
    let thread = query_thread(connection, thread_id)?;
    let mut statement = connection
        .prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE thread_id=?1 AND EXISTS (
                 SELECT 1 FROM conversation_message_derivations derivation
                 WHERE derivation.child_message_id=messages.id
             ) ORDER BY sequence"
        ))
        .map_err(map_sqlite)?;
    let derived = statement
        .query_map([thread_id.as_str()], mapping::message_from_row)
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    drop(statement);
    match &thread.lineage.origin {
        ConversationThreadOrigin::Original if derived.is_empty() => Ok(()),
        ConversationThreadOrigin::Fork { .. } if !derived.is_empty() => {
            validate_persisted_fork_messages(connection, &thread, &derived)
        }
        ConversationThreadOrigin::Original | ConversationThreadOrigin::Fork { .. } => {
            Err(invalid_persisted_aggregate())
        }
    }
}

fn validate_context_source_turns(
    connection: &Connection,
    messages: &[Message],
) -> Result<(), StoreError> {
    let mut validated = HashSet::new();
    for message in messages {
        let mut statement = connection
            .prepare(&format!(
                "SELECT {TURN_COLUMNS} FROM conversation_turns
                 WHERE user_message_id=?1 OR assistant_message_id=?1"
            ))
            .map_err(map_sqlite)?;
        let turns = statement
            .query_map([message.id.as_str()], turn_from_row)
            .map_err(map_sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_sqlite)?;
        drop(statement);
        for turn in turns {
            if !validated.insert(turn.id.clone()) {
                continue;
            }
            let snapshot = query_snapshot(connection, &turn)?;
            if snapshot.turn.state != ConversationTurnState::Completed {
                return Err(invalid_persisted_aggregate());
            }
        }
    }
    Ok(())
}

fn validate_context(context: &[Message]) -> Result<(), StoreError> {
    if context.is_empty() || context.len() > MAX_CONVERSATION_CONTEXT_MESSAGES {
        return Err(StoreError::Conflict);
    }
    let bytes = context
        .iter()
        .try_fold(0usize, |total, message| {
            total.checked_add(message.content.len())
        })
        .ok_or(StoreError::Conflict)?;
    if bytes > MAX_CONVERSATION_CONTEXT_BYTES {
        return Err(StoreError::Conflict);
    }
    if !context_entries_are_canonical(context) {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn context_entries_are_canonical(context: &[Message]) -> bool {
    let mut ids = HashSet::with_capacity(context.len());
    let mut previous_sequence = None;
    for message in context {
        if !is_reachable_active_message(message)
            || !ids.insert(message.id.clone())
            || previous_sequence.is_some_and(|previous| message.sequence <= previous)
        {
            return false;
        }
        previous_sequence = Some(message.sequence);
    }
    true
}

fn insert_message(connection: &Connection, message: &Message) -> Result<(), StoreError> {
    connection
        .execute(
            "INSERT INTO messages(id,thread_id,sequence,role,content,state,revision,created_at,updated_at)
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
    Ok(())
}

fn insert_context(
    connection: &Connection,
    turn_id: &ConversationTurnId,
    context: &[Message],
) -> Result<(), StoreError> {
    for message in context {
        connection
            .execute(
                "INSERT INTO conversation_turn_context(
                    turn_id,sequence,message_id,role,content,revision,created_at,updated_at
                 ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                params![
                    turn_id.as_str(),
                    number(message.sequence)?,
                    message.id.as_str(),
                    mapping::message_role_to_i64(message.role),
                    message.content,
                    number(message.revision)?,
                    number(message.created_at)?,
                    number(message.updated_at)?,
                ],
            )
            .map_err(map_sqlite)?;
    }
    Ok(())
}

fn insert_turn_lineage(
    connection: &Connection,
    turn_id: &ConversationTurnId,
    lineage: &ConversationTurnLineage,
) -> Result<(), StoreError> {
    let (origin, source_turn_id) = match &lineage.origin {
        ConversationTurnOrigin::Original => (0_i64, None),
        ConversationTurnOrigin::Retry { source_turn_id } => (1_i64, Some(source_turn_id.as_str())),
        ConversationTurnOrigin::EditAndBranch { source_turn_id } => {
            (2_i64, Some(source_turn_id.as_str()))
        }
        ConversationTurnOrigin::Regenerate { source_turn_id } => {
            (3_i64, Some(source_turn_id.as_str()))
        }
    };
    connection
        .execute(
            "INSERT INTO conversation_turn_lineage(
                 turn_id,origin,source_turn_id,credential_binding_id,retry_depth,rail
             ) VALUES (?1,?2,?3,?4,?5,?6)",
            params![
                turn_id.as_str(),
                origin,
                source_turn_id,
                lineage.credential_binding_id,
                number(u64::from(lineage.retry_depth))?,
                match lineage.rail {
                    ChatRail::XaiApiKey => 0_i64,
                    ChatRail::SuperGrokApi => 1_i64,
                },
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn query_turn_lineage(
    connection: &Connection,
    turn_id: &ConversationTurnId,
) -> Result<ConversationTurnLineage, StoreError> {
    let lineage = connection
        .query_row(
            "SELECT origin,source_turn_id,credential_binding_id,retry_depth,rail
             FROM conversation_turn_lineage WHERE turn_id=?1",
            [turn_id.as_str()],
            |row| {
                let origin = match row.get::<_, i64>(0)? {
                    0 => ConversationTurnOrigin::Original,
                    1 => ConversationTurnOrigin::Retry {
                        source_turn_id: ConversationTurnId::new(row.get::<_, String>(1)?)
                            .map_err(|error| conversion(1, error))?,
                    },
                    2 => ConversationTurnOrigin::EditAndBranch {
                        source_turn_id: ConversationTurnId::new(row.get::<_, String>(1)?)
                            .map_err(|error| conversion(1, error))?,
                    },
                    3 => ConversationTurnOrigin::Regenerate {
                        source_turn_id: ConversationTurnId::new(row.get::<_, String>(1)?)
                            .map_err(|error| conversion(1, error))?,
                    },
                    value => return Err(conversion(0, format!("invalid turn origin {value}"))),
                };
                Ok(ConversationTurnLineage {
                    origin,
                    credential_binding_id: row.get(2)?,
                    retry_depth: unsigned(row, 3)?
                        .try_into()
                        .map_err(|error| conversion(3, format!("invalid retry depth: {error}")))?,
                    rail: match row.get::<_, i64>(4)? {
                        0 => ChatRail::XaiApiKey,
                        1 => ChatRail::SuperGrokApi,
                        value => return Err(conversion(4, format!("invalid chat rail {value}"))),
                    },
                })
            },
        )
        .map_err(map_sqlite)?;
    ConversationTurnLineage::restore(lineage, turn_id).map_err(|_| invalid_persisted_aggregate())
}

fn query_context(
    connection: &Connection,
    id: &ConversationTurnId,
) -> Result<Vec<Message>, StoreError> {
    let (count, total_bytes, largest_message): (i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(length(CAST(content AS BLOB))),0),
                    COALESCE(MAX(length(CAST(content AS BLOB))),0)
             FROM conversation_turn_context WHERE turn_id=?1",
            [id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(map_sqlite)?;
    let count = usize::try_from(count).map_err(|_| invalid_persisted_aggregate())?;
    let total_bytes = usize::try_from(total_bytes).map_err(|_| invalid_persisted_aggregate())?;
    let largest_message =
        usize::try_from(largest_message).map_err(|_| invalid_persisted_aggregate())?;
    if count == 0
        || count > MAX_CONVERSATION_CONTEXT_MESSAGES
        || total_bytes > MAX_CONVERSATION_CONTEXT_BYTES
        || largest_message > MAX_MESSAGE_BYTES
    {
        return Err(invalid_persisted_aggregate());
    }
    let mut statement = connection
        .prepare(
            "SELECT context.message_id,turns.thread_id,context.sequence,context.role,
                    context.content,0,context.revision,context.created_at,context.updated_at,
                    (SELECT kind FROM conversation_message_derivations
                     WHERE child_message_id=context.message_id),
                    (SELECT source_message_id FROM conversation_message_derivations
                     WHERE child_message_id=context.message_id),
                    (SELECT source_turn_id FROM conversation_message_derivations
                     WHERE child_message_id=context.message_id),
                    (SELECT source_context_sequence FROM conversation_message_derivations
                     WHERE child_message_id=context.message_id)
             FROM conversation_turn_context context
             JOIN conversation_turns turns ON turns.id=context.turn_id
             WHERE context.turn_id=?1 ORDER BY context.sequence",
        )
        .map_err(map_sqlite)?;
    let context = statement
        .query_map([id.as_str()], mapping::message_from_row)
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    validate_context(&context).map_err(|_| invalid_persisted_aggregate())?;
    Ok(context)
}

fn insert_turn(connection: &Connection, turn: &ConversationTurn) -> Result<(), StoreError> {
    let citations = encode_citations(&turn.citations)?;
    let (failure_kind, failure_message, failure_retryable) = failure_parts(turn.failure.as_ref());
    connection
        .execute(
            "INSERT INTO conversation_turns(
                id,idempotency_key,request_fingerprint,provider_request_fingerprint,project_id,
                thread_id,user_message_id,run_id,model_id,state,effect_id,assistant_message_id,
                failure_kind,failure_message,failure_retryable,provider_response_id,citations_json,
                input_tokens,output_tokens,cost_in_usd_ticks,zero_data_retention,revision,
                created_at,updated_at
             ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,
                       ?17,?18,?19,?20,?21,?22,?23,?24)",
            params![
                turn.id.as_str(),
                turn.idempotency_key,
                turn.request_fingerprint.as_slice(),
                turn.provider_request_fingerprint
                    .as_ref()
                    .map(<[u8; 32]>::as_slice),
                turn.project_id.as_str(),
                turn.thread_id.as_str(),
                turn.user_message_id.as_str(),
                turn.run_id.as_str(),
                turn.model_id,
                turn_state_to_i64(turn.state),
                turn.effect_id.as_ref().map(EffectId::as_str),
                turn.assistant_message_id.as_ref().map(MessageId::as_str),
                failure_kind,
                failure_message,
                failure_retryable,
                turn.provider_response_id,
                citations,
                number(turn.usage.input_tokens)?,
                number(turn.usage.output_tokens)?,
                number(turn.usage.cost_in_usd_ticks)?,
                turn.zero_data_retention.map(i64::from),
                number(turn.revision)?,
                number(turn.created_at)?,
                number(turn.updated_at)?,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn update_turn(
    connection: &Connection,
    turn: &ConversationTurn,
    expected_revision: u64,
) -> Result<(), StoreError> {
    let citations = encode_citations(&turn.citations)?;
    let (failure_kind, failure_message, failure_retryable) = failure_parts(turn.failure.as_ref());
    let changed = connection
        .execute(
            "UPDATE conversation_turns SET
                provider_request_fingerprint=?1,state=?2,effect_id=?3,assistant_message_id=?4,
                failure_kind=?5,failure_message=?6,failure_retryable=?7,provider_response_id=?8,
                citations_json=?9,input_tokens=?10,output_tokens=?11,cost_in_usd_ticks=?12,
                zero_data_retention=?13,revision=?14,updated_at=?15
             WHERE id=?16 AND revision=?17",
            params![
                turn.provider_request_fingerprint
                    .as_ref()
                    .map(<[u8; 32]>::as_slice),
                turn_state_to_i64(turn.state),
                turn.effect_id.as_ref().map(EffectId::as_str),
                turn.assistant_message_id.as_ref().map(MessageId::as_str),
                failure_kind,
                failure_message,
                failure_retryable,
                turn.provider_response_id,
                citations,
                number(turn.usage.input_tokens)?,
                number(turn.usage.output_tokens)?,
                number(turn.usage.cost_in_usd_ticks)?,
                turn.zero_data_retention.map(i64::from),
                number(turn.revision)?,
                number(turn.updated_at)?,
                turn.id.as_str(),
                number(expected_revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    if changed != 1 || turn.revision != expected_revision.saturating_add(1) {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn query_turn_by_key(
    connection: &Connection,
    key: &str,
) -> Result<Option<ConversationTurn>, StoreError> {
    connection
        .query_row(
            &format!("SELECT {TURN_COLUMNS} FROM conversation_turns WHERE idempotency_key=?1"),
            [key],
            turn_from_row,
        )
        .optional()
        .map_err(map_sqlite)
}

fn query_turn(
    connection: &Connection,
    id: &ConversationTurnId,
) -> Result<ConversationTurn, StoreError> {
    connection
        .query_row(
            &format!("SELECT {TURN_COLUMNS} FROM conversation_turns WHERE id=?1"),
            [id.as_str()],
            turn_from_row,
        )
        .map_err(map_sqlite)
}

fn query_snapshot(
    connection: &Connection,
    turn: &ConversationTurn,
) -> Result<ConversationTurnSnapshot, StoreError> {
    let mut visiting = HashSet::with_capacity(MAX_CONVERSATION_SNAPSHOT_ANCESTRY);
    query_snapshot_bounded(connection, turn, &mut visiting)
}

fn query_snapshot_with_events(
    connection: &Connection,
    turn: &ConversationTurn,
) -> Result<(ConversationTurnSnapshot, ConversationTurnEventLog), StoreError> {
    let mut visiting = HashSet::with_capacity(MAX_CONVERSATION_SNAPSHOT_ANCESTRY);
    query_snapshot_with_events_bounded(connection, turn, &mut visiting)
}

fn query_snapshot_bounded(
    connection: &Connection,
    turn: &ConversationTurn,
    visiting: &mut HashSet<ConversationTurnId>,
) -> Result<ConversationTurnSnapshot, StoreError> {
    query_snapshot_with_events_bounded(connection, turn, visiting).map(|(snapshot, _)| snapshot)
}

fn query_snapshot_with_events_bounded(
    connection: &Connection,
    turn: &ConversationTurn,
    visiting: &mut HashSet<ConversationTurnId>,
) -> Result<(ConversationTurnSnapshot, ConversationTurnEventLog), StoreError> {
    if visiting.len() >= MAX_CONVERSATION_SNAPSHOT_ANCESTRY || !visiting.insert(turn.id.clone()) {
        return Err(invalid_persisted_aggregate());
    }
    let result = query_snapshot_with_events_inner(connection, turn, visiting);
    visiting.remove(&turn.id);
    result
}

fn query_snapshot_with_events_inner(
    connection: &Connection,
    turn: &ConversationTurn,
    visiting: &mut HashSet<ConversationTurnId>,
) -> Result<(ConversationTurnSnapshot, ConversationTurnEventLog), StoreError> {
    let lineage = query_turn_lineage(connection, &turn.id)?;
    let user_message = query_message(connection, &turn.user_message_id)?;
    let assistant_message = turn
        .assistant_message_id
        .as_ref()
        .map(|id| query_message(connection, id))
        .transpose()?;
    let run = query_run(connection, &turn.run_id)?;
    let effect = turn
        .effect_id
        .as_ref()
        .map(|id| query_effect(connection, id))
        .transpose()?;
    let snapshot = ConversationTurnSnapshot {
        turn: turn.clone(),
        user_message,
        assistant_message,
        run,
        effect,
        lineage,
    };
    validate_persisted_snapshot(connection, &snapshot, visiting)?;
    let turn_events = query_turn_event_log(connection, &snapshot.turn.id)?;
    turn_events
        .validate_snapshot(
            &snapshot.turn,
            snapshot
                .assistant_message
                .as_ref()
                .map(|message| message.content.as_str()),
        )
        .map_err(|_| invalid_persisted_aggregate())?;
    Ok((snapshot, turn_events))
}

fn query_turn_event_log(
    connection: &Connection,
    turn_id: &ConversationTurnId,
) -> Result<ConversationTurnEventLog, StoreError> {
    let (count, total_text_bytes, largest_text, minimum_sequence, maximum_sequence): (
        i64,
        i64,
        i64,
        Option<i64>,
        Option<i64>,
    ) = connection
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN kind=2 THEN length(CAST(text AS BLOB)) ELSE 0 END),0),
                    COALESCE(MAX(CASE WHEN kind=2 THEN length(CAST(text AS BLOB)) ELSE 0 END),0),
                    MIN(sequence),MAX(sequence)
             FROM conversation_turn_events WHERE turn_id=?1",
            [turn_id.as_str()],
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
        .map_err(map_sqlite)?;
    let count = usize::try_from(count).map_err(|_| invalid_persisted_aggregate())?;
    let total_text_bytes =
        usize::try_from(total_text_bytes).map_err(|_| invalid_persisted_aggregate())?;
    let largest_text = usize::try_from(largest_text).map_err(|_| invalid_persisted_aggregate())?;
    let maximum_event_count = MAX_CONVERSATION_TEXT_EVENTS.saturating_add(3);
    if count == 0
        || count > maximum_event_count
        || total_text_bytes > MAX_MESSAGE_BYTES
        || largest_text > MAX_CONVERSATION_TEXT_CHUNK_BYTES
        || minimum_sequence != Some(1)
        || usize::try_from(maximum_sequence.unwrap_or_default()).ok() != Some(count)
    {
        return Err(invalid_persisted_aggregate());
    }

    let mut statement = connection
        .prepare(&format!(
            "SELECT {TURN_EVENT_COLUMNS} FROM conversation_turn_events
             WHERE turn_id=?1 ORDER BY sequence"
        ))
        .map_err(map_sqlite)?;
    let mut rows = statement.query([turn_id.as_str()]).map_err(map_sqlite)?;
    let mut log = ConversationTurnEventLog::new(turn_id.clone());
    while let Some(row) = rows.next().map_err(map_sqlite)? {
        let event = turn_event_from_row(row).map_err(map_sqlite)?;
        log.append_event(event)
            .map_err(|_| invalid_persisted_aggregate())?;
    }
    Ok(log)
}

fn normalized_text_chunks(
    start_utf8_offset: u64,
    text: &str,
) -> Result<Vec<ConversationTurnEventKind>, StoreError> {
    if text.is_empty() || text.len() > MAX_MESSAGE_BYTES {
        return Err(StoreError::Conflict);
    }
    let text_length = u64::try_from(text.len()).map_err(|_| StoreError::Conflict)?;
    let maximum = u64::try_from(MAX_MESSAGE_BYTES).map_err(|_| StoreError::Conflict)?;
    if start_utf8_offset
        .checked_add(text_length)
        .is_none_or(|end| end > maximum)
    {
        return Err(StoreError::Conflict);
    }

    let mut kinds = Vec::with_capacity(text.len().div_ceil(MAX_CONVERSATION_TEXT_CHUNK_BYTES));
    let mut start = 0usize;
    while start < text.len() {
        let mut end = start
            .saturating_add(MAX_CONVERSATION_TEXT_CHUNK_BYTES)
            .min(text.len());
        while !text.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        if end == start {
            return Err(StoreError::Conflict);
        }
        kinds.push(ConversationTurnEventKind::TextAppended {
            start_utf8_offset: start_utf8_offset
                .checked_add(u64::try_from(start).map_err(|_| StoreError::Conflict)?)
                .ok_or(StoreError::Conflict)?,
            text: text[start..end].to_owned(),
        });
        start = end;
    }
    Ok(kinds)
}

fn query_exact_text_replay(
    connection: &Connection,
    turn_id: &ConversationTurnId,
    start_utf8_offset: u64,
    expected_text: &str,
) -> Result<Vec<ConversationTurnEvent>, StoreError> {
    if expected_text.is_empty() {
        return Err(StoreError::Conflict);
    }
    let mut statement = connection
        .prepare(&format!(
            "SELECT {TURN_EVENT_COLUMNS} FROM conversation_turn_events
             WHERE turn_id=?1 AND kind=2 AND start_utf8_offset>=?2
             ORDER BY start_utf8_offset LIMIT ?3"
        ))
        .map_err(map_sqlite)?;
    let mut rows = statement
        .query(params![
            turn_id.as_str(),
            number(start_utf8_offset)?,
            i64::try_from(MAX_CONVERSATION_TEXT_EVENTS).map_err(|_| StoreError::Conflict)?,
        ])
        .map_err(map_sqlite)?;
    let mut events = Vec::new();
    let mut actual = String::with_capacity(expected_text.len());
    let mut expected_offset = start_utf8_offset;
    while let Some(row) = rows.next().map_err(map_sqlite)? {
        let event = turn_event_from_row(row).map_err(map_sqlite)?;
        let ConversationTurnEventKind::TextAppended {
            start_utf8_offset,
            text,
        } = &event.kind
        else {
            return Err(StoreError::Conflict);
        };
        if *start_utf8_offset != expected_offset || actual.len() >= expected_text.len() {
            return Err(StoreError::Conflict);
        }
        actual.push_str(text);
        expected_offset = expected_offset
            .checked_add(u64::try_from(text.len()).map_err(|_| StoreError::Conflict)?)
            .ok_or(StoreError::Conflict)?;
        events.push(event);
        if actual.len() >= expected_text.len() {
            break;
        }
    }
    if actual != expected_text {
        return Err(StoreError::Conflict);
    }
    Ok(events)
}

fn query_event_page(
    connection: &Connection,
    turn_id: &ConversationTurnId,
    after_sequence: u64,
    limit: usize,
) -> Result<ConversationTurnEventPage, StoreError> {
    let look_ahead = limit.checked_add(1).ok_or(StoreError::Conflict)?;
    let after_sequence = i64::try_from(after_sequence).unwrap_or(i64::MAX);
    let sql_limit = i64::try_from(look_ahead).map_err(|_| StoreError::Conflict)?;
    let (count, total_text_bytes, largest_text): (i64, i64, i64) = connection
        .query_row(
            "SELECT COUNT(*),
                    COALESCE(SUM(CASE WHEN kind=2 THEN length(CAST(text AS BLOB)) ELSE 0 END),0),
                    COALESCE(MAX(CASE WHEN kind=2 THEN length(CAST(text AS BLOB)) ELSE 0 END),0)
             FROM (
                 SELECT kind,text FROM conversation_turn_events
                 WHERE turn_id=?1 AND sequence>?2
                 ORDER BY sequence LIMIT ?3
             )",
            params![turn_id.as_str(), after_sequence, sql_limit],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(map_sqlite)?;
    let count = usize::try_from(count).map_err(|_| invalid_persisted_aggregate())?;
    let total_text_bytes =
        usize::try_from(total_text_bytes).map_err(|_| invalid_persisted_aggregate())?;
    let largest_text = usize::try_from(largest_text).map_err(|_| invalid_persisted_aggregate())?;
    if count > look_ahead
        || total_text_bytes > look_ahead.saturating_mul(MAX_CONVERSATION_TEXT_CHUNK_BYTES)
        || largest_text > MAX_CONVERSATION_TEXT_CHUNK_BYTES
    {
        return Err(invalid_persisted_aggregate());
    }

    let mut statement = connection
        .prepare(&format!(
            "SELECT {TURN_EVENT_COLUMNS} FROM conversation_turn_events
             WHERE turn_id=?1 AND sequence>?2 ORDER BY sequence LIMIT ?3"
        ))
        .map_err(map_sqlite)?;
    let mut events = statement
        .query_map(
            params![turn_id.as_str(), after_sequence, sql_limit],
            turn_event_from_row,
        )
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    let has_more = events.len() > limit;
    events.truncate(limit);
    Ok(ConversationTurnEventPage { events, has_more })
}

fn insert_turn_event(
    connection: &Connection,
    event: &ConversationTurnEvent,
) -> Result<(), StoreError> {
    let event = ConversationTurnEvent::restore(event.clone()).map_err(|_| StoreError::Conflict)?;
    let (kind, from_state, to_state, start_utf8_offset, text) = match &event.kind {
        ConversationTurnEventKind::Created => (0, None, None, None, None),
        ConversationTurnEventKind::StateChanged { from, to } => (
            1,
            Some(turn_state_to_i64(*from)),
            Some(turn_state_to_i64(*to)),
            None,
            None,
        ),
        ConversationTurnEventKind::TextAppended {
            start_utf8_offset,
            text,
        } => (
            2,
            None,
            None,
            Some(number(*start_utf8_offset)?),
            Some(text.as_str()),
        ),
    };
    connection
        .execute(
            "INSERT INTO conversation_turn_events(
                 turn_id,sequence,kind,from_state,to_state,start_utf8_offset,text
             ) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                event.turn_id.as_str(),
                number(event.sequence)?,
                kind,
                from_state,
                to_state,
                start_utf8_offset,
                text,
            ],
        )
        .map_err(map_sqlite)?;
    Ok(())
}

fn turn_event_from_row(row: &Row<'_>) -> rusqlite::Result<ConversationTurnEvent> {
    let kind: i64 = row.get(2)?;
    let from_state: Option<i64> = row.get(3)?;
    let to_state: Option<i64> = row.get(4)?;
    let start_utf8_offset: Option<i64> = row.get(5)?;
    let text: Option<String> = row.get(6)?;
    let kind = match (kind, from_state, to_state, start_utf8_offset, text) {
        (0, None, None, None, None) => ConversationTurnEventKind::Created,
        (1, Some(from), Some(to), None, None) => ConversationTurnEventKind::StateChanged {
            from: turn_state_from_i64(from).map_err(|error| conversion(3, error))?,
            to: turn_state_from_i64(to).map_err(|error| conversion(4, error))?,
        },
        (2, None, None, Some(start), Some(text)) => ConversationTurnEventKind::TextAppended {
            start_utf8_offset: start.try_into().map_err(|error| conversion(5, error))?,
            text,
        },
        _ => return Err(conversion(2, "invalid conversation turn event columns")),
    };
    ConversationTurnEvent::restore(ConversationTurnEvent {
        sequence: unsigned(row, 0)?,
        turn_id: ConversationTurnId::new(row.get::<_, String>(1)?)
            .map_err(|error| conversion(1, error))?,
        kind,
    })
    .map_err(|error| conversion(2, error))
}

fn validate_persisted_snapshot(
    connection: &Connection,
    snapshot: &ConversationTurnSnapshot,
    visiting: &mut HashSet<ConversationTurnId>,
) -> Result<(), StoreError> {
    let project_id = connection
        .query_row(
            "SELECT threads.project_id FROM threads
             JOIN projects ON projects.id=threads.project_id WHERE threads.id=?1",
            [snapshot.turn.thread_id.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(map_sqlite)?;
    if project_id.as_deref() != Some(snapshot.turn.project_id.as_str())
        || !is_canonical_linked_message(
            &snapshot.user_message,
            &snapshot.turn.user_message_id,
            &snapshot.turn.thread_id,
            MessageRole::User,
            snapshot.turn.created_at,
        )
        || !canonical_linked_run_and_effect(snapshot)
    {
        return Err(invalid_persisted_aggregate());
    }

    match (
        snapshot.turn.state,
        snapshot.assistant_message.as_ref(),
        snapshot.turn.assistant_message_id.as_ref(),
    ) {
        (ConversationTurnState::Completed, Some(assistant), Some(assistant_id))
            if is_canonical_linked_message(
                assistant,
                assistant_id,
                &snapshot.turn.thread_id,
                MessageRole::Assistant,
                snapshot.turn.updated_at,
            ) && assistant.sequence > snapshot.user_message.sequence => {}
        (ConversationTurnState::Completed, _, _) => return Err(invalid_persisted_aggregate()),
        (_, None, None) => {}
        _ => return Err(invalid_persisted_aggregate()),
    }
    validate_persisted_lineage(connection, snapshot, visiting)
}

fn validate_persisted_lineage(
    connection: &Connection,
    snapshot: &ConversationTurnSnapshot,
    visiting: &mut HashSet<ConversationTurnId>,
) -> Result<(), StoreError> {
    let thread_binding = query_thread_credential_binding(connection, &snapshot.turn.thread_id)?;
    if thread_binding != snapshot.lineage.credential_binding_id {
        return Err(invalid_persisted_aggregate());
    }
    match &snapshot.lineage.origin {
        ConversationTurnOrigin::Original => Ok(()),
        ConversationTurnOrigin::Retry { source_turn_id } => {
            validate_persisted_retry_lineage(connection, snapshot, source_turn_id)
        }
        ConversationTurnOrigin::EditAndBranch { source_turn_id } => validate_persisted_fork_turn(
            connection,
            snapshot,
            source_turn_id,
            ConversationForkKind::EditAndBranch,
            visiting,
        ),
        ConversationTurnOrigin::Regenerate { source_turn_id } => validate_persisted_fork_turn(
            connection,
            snapshot,
            source_turn_id,
            ConversationForkKind::Regenerate,
            visiting,
        ),
    }
}

fn validate_persisted_retry_lineage(
    connection: &Connection,
    snapshot: &ConversationTurnSnapshot,
    source_turn_id: &ConversationTurnId,
) -> Result<(), StoreError> {
    let source_turn = query_turn(connection, source_turn_id)?;
    let source_lineage = query_turn_lineage(connection, source_turn_id)?;
    let source_user = query_message(connection, &source_turn.user_message_id)?;
    let source_context = query_context(connection, source_turn_id)?;
    let retry_context = query_context(connection, &snapshot.turn.id)?;
    let eligible_state = source_turn.state == ConversationTurnState::Cancelled
        || (source_turn.state == ConversationTurnState::Failed
            && source_turn
                .failure
                .as_ref()
                .is_some_and(|failure| failure.retryable));
    let expected_depth = source_lineage
        .retry_depth
        .checked_add(1)
        .filter(|depth| *depth <= 64);
    if !eligible_state
        || source_turn.project_id != snapshot.turn.project_id
        || source_turn.thread_id != snapshot.turn.thread_id
        || source_turn.model_id != snapshot.turn.model_id
        || source_lineage.credential_binding_id.is_none()
        || source_lineage.credential_binding_id != snapshot.lineage.credential_binding_id
        || source_lineage.rail != snapshot.lineage.rail
        || expected_depth != Some(snapshot.lineage.retry_depth)
        || source_user.role != MessageRole::User
        || source_user.state != MessageState::Active
        || source_user.sequence.checked_add(1) != Some(snapshot.user_message.sequence)
        || source_user.content != snapshot.user_message.content
        || source_context.last() != Some(&source_user)
        || retry_context.last() != Some(&snapshot.user_message)
        || source_context.len() != retry_context.len()
        || source_context[..source_context.len().saturating_sub(1)]
            != retry_context[..retry_context.len().saturating_sub(1)]
    {
        return Err(invalid_persisted_aggregate());
    }
    Ok(())
}

fn validate_persisted_fork_turn(
    connection: &Connection,
    snapshot: &ConversationTurnSnapshot,
    source_turn_id: &ConversationTurnId,
    expected_kind: ConversationForkKind,
    visiting: &mut HashSet<ConversationTurnId>,
) -> Result<(), StoreError> {
    let child = query_thread(connection, &snapshot.turn.thread_id)?;
    let ConversationThreadOrigin::Fork {
        parent_thread_id,
        source_turn_id: thread_source_turn_id,
        kind,
        ..
    } = &child.lineage.origin
    else {
        return Err(invalid_persisted_aggregate());
    };
    let source_turn = query_turn(connection, source_turn_id)?;
    let source_snapshot = query_snapshot_bounded(connection, &source_turn, visiting)?;
    let source_context = query_context(connection, source_turn_id)?;
    let child_context = query_context(connection, &snapshot.turn.id)?;
    let mut statement = connection
        .prepare(&format!(
            "SELECT {MESSAGE_COLUMNS} FROM messages
             WHERE thread_id=?1 AND EXISTS (
                 SELECT 1 FROM conversation_message_derivations derivation
                 WHERE derivation.child_message_id=messages.id
             ) ORDER BY sequence"
        ))
        .map_err(map_sqlite)?;
    let derived_messages = statement
        .query_map([child.id.as_str()], mapping::message_from_row)
        .map_err(map_sqlite)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_sqlite)?;
    drop(statement);
    validate_persisted_fork_messages_with_source(
        connection,
        &child,
        &derived_messages,
        &source_snapshot,
        &source_context,
    )?;
    let command: Option<([u8; 32], ConversationTurnId, ThreadId)> = connection
        .query_row(
            "SELECT request_fingerprint,source_turn_id,child_thread_id
             FROM conversation_fork_commands WHERE started_turn_id=?1",
            [snapshot.turn.id.as_str()],
            |row| {
                Ok((
                    fingerprint(row, 0, false)?
                        .ok_or_else(|| conversion(0, "missing fork command fingerprint"))?,
                    ConversationTurnId::new(row.get::<_, String>(1)?)
                        .map_err(|error| conversion(1, error))?,
                    ThreadId::new(row.get::<_, String>(2)?)
                        .map_err(|error| conversion(2, error))?,
                ))
            },
        )
        .optional()
        .map_err(map_sqlite)?;
    let source_eligible = match expected_kind {
        ConversationForkKind::EditAndBranch => matches!(
            source_turn.state,
            ConversationTurnState::Completed
                | ConversationTurnState::Failed
                | ConversationTurnState::Cancelled
        ),
        ConversationForkKind::Regenerate => source_turn.state == ConversationTurnState::Completed,
        ConversationForkKind::Branch => false,
    };
    if *kind != expected_kind
        || thread_source_turn_id != source_turn_id
        || source_turn.thread_id != *parent_thread_id
        || source_turn.project_id != snapshot.turn.project_id
        || source_turn.model_id != snapshot.turn.model_id
        || source_snapshot.lineage.credential_binding_id != snapshot.lineage.credential_binding_id
        || source_snapshot.lineage.rail != snapshot.lineage.rail
        || !source_eligible
        || child.created_at != snapshot.turn.created_at
        || child_context != derived_messages
        || child_context.last() != Some(&snapshot.user_message)
        || command
            != Some((
                snapshot.turn.request_fingerprint,
                source_turn_id.clone(),
                child.id,
            ))
    {
        return Err(invalid_persisted_aggregate());
    }
    Ok(())
}

fn canonical_linked_run_and_effect(snapshot: &ConversationTurnSnapshot) -> bool {
    let turn = &snapshot.turn;
    let run = &snapshot.run;
    if run.id != turn.run_id
        || run.project_id != turn.project_id
        || run.thread_id != turn.thread_id
        || run.created_at != turn.created_at
    {
        return false;
    }
    let mut expected_run = Run::queued(
        run.id.clone(),
        turn.project_id.clone(),
        turn.thread_id.clone(),
        turn.created_at,
    );

    match turn.state {
        ConversationTurnState::Reserved => run == &expected_run && snapshot.effect.is_none(),
        ConversationTurnState::Cancelled => {
            expected_run
                .transition(RunState::Cancelled, turn.updated_at)
                .is_ok()
                && run == &expected_run
                && snapshot.effect.is_none()
        }
        ConversationTurnState::ProviderStarted
        | ConversationTurnState::Completed
        | ConversationTurnState::Failed
        | ConversationTurnState::InterruptedNeedsReview => {
            let Some(effect) = snapshot.effect.as_ref() else {
                return false;
            };
            if turn.effect_id.as_ref() != Some(&effect.id)
                || effect.run_id != run.id
                || effect.kind != EffectKind::ExternalMutation
                || effect.idempotency != Idempotency::NonIdempotent
                || effect.target != canonical_effect_target(turn)
                || effect.created_at < turn.created_at
                || expected_run
                    .transition(RunState::Planning, effect.created_at)
                    .is_err()
                || expected_run
                    .transition(RunState::Running, effect.created_at)
                    .is_err()
            {
                return false;
            }
            let mut expected_effect = SideEffect::prepare(
                effect.id.clone(),
                run.id.clone(),
                EffectKind::ExternalMutation,
                canonical_effect_target(turn),
                Idempotency::NonIdempotent,
                effect.created_at,
            );
            if expected_effect.start(effect.created_at).is_err() {
                return false;
            }
            match turn.state {
                ConversationTurnState::ProviderStarted => {
                    turn.updated_at == effect.created_at
                        && run == &expected_run
                        && effect == &expected_effect
                }
                ConversationTurnState::Completed => {
                    expected_run
                        .transition(RunState::Completed, turn.updated_at)
                        .is_ok()
                        && expected_effect.finish(true, turn.updated_at).is_ok()
                        && run == &expected_run
                        && effect == &expected_effect
                }
                ConversationTurnState::Failed => {
                    expected_run
                        .transition(RunState::Failed, turn.updated_at)
                        .is_ok()
                        && expected_effect.finish(false, turn.updated_at).is_ok()
                        && run == &expected_run
                        && effect == &expected_effect
                }
                ConversationTurnState::InterruptedNeedsReview => {
                    expected_run
                        .transition(RunState::InterruptedNeedsReview, turn.updated_at)
                        .is_ok()
                        && expected_effect.interrupt(turn.updated_at).is_ok()
                        && run == &expected_run
                        && effect == &expected_effect
                }
                ConversationTurnState::Reserved | ConversationTurnState::Cancelled => false,
            }
        }
    }
}

fn is_canonical_linked_message(
    message: &Message,
    expected_id: &MessageId,
    expected_thread_id: &ThreadId,
    role: MessageRole,
    created_at: u64,
) -> bool {
    message.id == *expected_id
        && message.thread_id == *expected_thread_id
        && message.sequence > 0
        && message.role == role
        && message.state == MessageState::Active
        && message.revision == 0
        && message.created_at == created_at
        && message.updated_at == created_at
        && Message::restore(message.clone()).is_ok()
}

fn is_canonical_unsequenced_assistant(
    message: &Message,
    expected_id: &MessageId,
    expected_thread_id: &ThreadId,
    created_at: u64,
) -> bool {
    Message::new(
        expected_id.clone(),
        expected_thread_id.clone(),
        MessageRole::Assistant,
        message.content.clone(),
        created_at,
    )
    .is_ok_and(|canonical| canonical == *message)
}

fn is_reachable_active_message(message: &Message) -> bool {
    message.sequence > 0
        && message.state == MessageState::Active
        && message.updated_at >= message.created_at
        && (message.revision > 0 || message.updated_at == message.created_at)
        && Message::restore(message.clone()).is_ok()
}

fn validate_turn_context(
    context: &[Message],
    snapshot: &ConversationTurnSnapshot,
) -> Result<(), StoreError> {
    validate_context(context).map_err(|_| invalid_persisted_aggregate())?;
    if context
        .iter()
        .any(|message| message.thread_id != snapshot.turn.thread_id)
        || context.last() != Some(&snapshot.user_message)
    {
        return Err(invalid_persisted_aggregate());
    }
    Ok(())
}

fn invalid_persisted_aggregate() -> StoreError {
    StoreError::Internal("invalid persisted conversation aggregate".into())
}

fn valid_provider_start_commit(
    current: &ConversationTurnSnapshot,
    commit: &ProviderStartCommit,
) -> bool {
    if current.turn.state != ConversationTurnState::Reserved
        || current.run.state != RunState::Queued
        || current.effect.is_some()
        || commit.expected_turn_revision != current.turn.revision
        || commit.expected_run_revision != current.run.revision
    {
        return false;
    }
    let Some(provider_fingerprint) = commit.turn.provider_request_fingerprint else {
        return false;
    };
    let now = commit.turn.updated_at;
    let mut expected_turn = current.turn.clone();
    if expected_turn
        .start_provider(commit.effect.id.clone(), provider_fingerprint, now)
        .is_err()
        || expected_turn != commit.turn
    {
        return false;
    }
    let mut expected_run = current.run.clone();
    if expected_run.transition(RunState::Planning, now).is_err()
        || expected_run.transition(RunState::Running, now).is_err()
        || expected_run != commit.run
    {
        return false;
    }
    let mut expected_effect = SideEffect::prepare(
        commit.effect.id.clone(),
        current.run.id.clone(),
        EffectKind::ExternalMutation,
        canonical_effect_target(&current.turn),
        Idempotency::NonIdempotent,
        now,
    );
    if expected_effect.start(now).is_err() || expected_effect != commit.effect {
        return false;
    }
    commit.events
        == vec![
            NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::StateChanged {
                    from: RunState::Queued,
                    to: RunState::Planning,
                },
            },
            NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::StateChanged {
                    from: RunState::Planning,
                    to: RunState::Running,
                },
            },
            NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::EffectPrepared {
                    effect_id: commit.effect.id.clone(),
                },
            },
        ]
}

fn valid_terminal_commit(current: &ConversationTurnSnapshot, commit: &TerminalTurnCommit) -> bool {
    if commit.expected_turn_revision != current.turn.revision
        || commit.expected_run_revision != current.run.revision
    {
        return false;
    }
    let now = commit.turn.updated_at;
    match commit.turn.state {
        ConversationTurnState::Cancelled => valid_cancelled_terminal(current, commit, now),
        ConversationTurnState::Completed
        | ConversationTurnState::Failed
        | ConversationTurnState::InterruptedNeedsReview => {
            valid_provider_terminal(current, commit, now)
        }
        ConversationTurnState::Reserved | ConversationTurnState::ProviderStarted => false,
    }
}

fn valid_cancelled_terminal(
    current: &ConversationTurnSnapshot,
    commit: &TerminalTurnCommit,
    now: u64,
) -> bool {
    let mut expected_turn = current.turn.clone();
    let mut expected_run = current.run.clone();
    current.turn.state == ConversationTurnState::Reserved
        && current.effect.is_none()
        && commit.effect.is_none()
        && commit.expected_effect_revision.is_none()
        && commit.assistant_message.is_none()
        && expected_turn.cancel(now).is_ok()
        && expected_run.transition(RunState::Cancelled, now).is_ok()
        && expected_turn == commit.turn
        && expected_run == commit.run
        && commit.events
            == vec![NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::StateChanged {
                    from: RunState::Queued,
                    to: RunState::Cancelled,
                },
            }]
}

fn valid_provider_terminal(
    current: &ConversationTurnSnapshot,
    commit: &TerminalTurnCommit,
    now: u64,
) -> bool {
    let (Some(current_effect), Some(committed_effect), Some(expected_effect_revision)) = (
        current.effect.as_ref(),
        commit.effect.as_ref(),
        commit.expected_effect_revision,
    ) else {
        return false;
    };
    if current.turn.state != ConversationTurnState::ProviderStarted
        || current.run.state != RunState::Running
        || current_effect.state != EffectState::Executing
        || expected_effect_revision != current_effect.revision
    {
        return false;
    }
    let Some((expected_turn, expected_effect, run_state, expected_events)) =
        expected_provider_terminal_transition(current, commit, now)
    else {
        return false;
    };
    let mut expected_run = current.run.clone();
    expected_run.transition(run_state, now).is_ok()
        && expected_turn == commit.turn
        && expected_run == commit.run
        && expected_effect == *committed_effect
        && commit.events == expected_events
}

fn expected_provider_terminal_transition(
    current: &ConversationTurnSnapshot,
    commit: &TerminalTurnCommit,
    now: u64,
) -> Option<(ConversationTurn, SideEffect, RunState, Vec<NewRunEvent>)> {
    let current_effect = current.effect.as_ref()?;
    let mut turn = current.turn.clone();
    let mut effect = current_effect.clone();
    let run_state = match commit.turn.state {
        ConversationTurnState::Completed => {
            let assistant = commit.assistant_message.as_ref()?;
            let assistant_id = commit.turn.assistant_message_id.as_ref()?;
            if !is_canonical_unsequenced_assistant(
                assistant,
                assistant_id,
                &current.turn.thread_id,
                now,
            ) || turn
                .complete(
                    assistant_id.clone(),
                    commit.turn.provider_response_id.clone(),
                    commit.turn.citations.clone(),
                    commit.turn.usage,
                    commit.turn.zero_data_retention,
                    now,
                )
                .is_err()
                || effect.finish(true, now).is_err()
            {
                return None;
            }
            RunState::Completed
        }
        ConversationTurnState::Failed => {
            if commit.assistant_message.is_some()
                || turn.fail(commit.turn.failure.clone()?, now).is_err()
                || effect.finish(false, now).is_err()
            {
                return None;
            }
            RunState::Failed
        }
        ConversationTurnState::InterruptedNeedsReview => {
            if commit.assistant_message.is_some()
                || turn.interrupt(now).is_err()
                || effect.interrupt(now).is_err()
            {
                return None;
            }
            RunState::InterruptedNeedsReview
        }
        ConversationTurnState::Reserved
        | ConversationTurnState::ProviderStarted
        | ConversationTurnState::Cancelled => return None,
    };
    let events = terminal_events(run_state, &current_effect.id, now);
    Some((turn, effect, run_state, events))
}

fn terminal_events(run_state: RunState, effect_id: &EffectId, now: u64) -> Vec<NewRunEvent> {
    let mut events = Vec::with_capacity(2);
    if run_state == RunState::InterruptedNeedsReview {
        events.push(NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::EffectNeedsReview {
                effect_id: effect_id.clone(),
            },
        });
    }
    events.push(NewRunEvent {
        occurred_at: now,
        kind: RunEventKind::StateChanged {
            from: RunState::Running,
            to: run_state,
        },
    });
    events
}

fn canonical_effect_target(turn: &ConversationTurn) -> String {
    format!("official xAI Responses API model {}", turn.model_id)
}

fn query_message(connection: &Connection, id: &MessageId) -> Result<Message, StoreError> {
    connection
        .query_row(
            &format!("SELECT {MESSAGE_COLUMNS} FROM messages WHERE id=?1"),
            [id.as_str()],
            mapping::message_from_row,
        )
        .map_err(|error| match error {
            rusqlite::Error::FromSqlConversionFailure(..) => invalid_persisted_aggregate(),
            other => map_sqlite(other),
        })
}

fn query_run(connection: &Connection, id: &RunId) -> Result<Run, StoreError> {
    connection
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM runs WHERE id=?1"),
            [id.as_str()],
            mapping::run_from_row,
        )
        .map_err(map_sqlite)
}

fn query_effect(connection: &Connection, id: &EffectId) -> Result<SideEffect, StoreError> {
    connection
        .query_row(
            &format!("SELECT {EFFECT_COLUMNS} FROM side_effects WHERE id=?1"),
            [id.as_str()],
            mapping::effect_from_row,
        )
        .map_err(map_sqlite)
}

fn update_run_for_provider_start(
    connection: &Connection,
    run: &Run,
    expected_revision: u64,
) -> Result<(), StoreError> {
    let changed = connection
        .execute(
            "UPDATE runs SET state=?1,revision=?2,updated_at=?3 WHERE id=?4 AND revision=?5",
            params![
                mapping::run_state_to_i64(run.state),
                number(run.revision)?,
                number(run.updated_at)?,
                run.id.as_str(),
                number(expected_revision)?,
            ],
        )
        .map_err(map_sqlite)?;
    if changed != 1 || run.revision != expected_revision.saturating_add(2) {
        return Err(StoreError::Conflict);
    }
    Ok(())
}

fn turn_from_row(row: &Row<'_>) -> rusqlite::Result<ConversationTurn> {
    let request_fingerprint =
        fingerprint(row, 2, false)?.ok_or_else(|| conversion(2, "missing request fingerprint"))?;
    let provider_request_fingerprint = fingerprint(row, 3, true)?;
    let citations_json: String = row.get(16)?;
    let citations = serde_json::from_str::<Vec<StoredCitation>>(&citations_json)
        .map_err(|error| conversion(16, error))?
        .into_iter()
        .map(|citation| ConversationCitation {
            title: citation.title,
            url: citation.url,
        })
        .collect();
    let failure_kind: Option<i64> = row.get(12)?;
    let failure_message: Option<String> = row.get(13)?;
    let failure_retryable: Option<i64> = row.get(14)?;
    let failure = match (failure_kind, failure_message, failure_retryable) {
        (None, None, None) => None,
        (Some(kind), Some(message), Some(retryable)) => Some(ConversationFailure {
            kind: failure_kind_from_i64(kind).map_err(|error| conversion(12, error))?,
            message,
            retryable: retryable != 0,
        }),
        _ => return Err(conversion(12, "invalid failure columns")),
    };
    ConversationTurn::restore(ConversationTurn {
        id: ConversationTurnId::new(row.get::<_, String>(0)?)
            .map_err(|error| conversion(0, error))?,
        idempotency_key: row.get(1)?,
        request_fingerprint,
        provider_request_fingerprint,
        project_id: ProjectId::new(row.get::<_, String>(4)?)
            .map_err(|error| conversion(4, error))?,
        thread_id: ThreadId::new(row.get::<_, String>(5)?).map_err(|error| conversion(5, error))?,
        user_message_id: MessageId::new(row.get::<_, String>(6)?)
            .map_err(|error| conversion(6, error))?,
        run_id: RunId::new(row.get::<_, String>(7)?).map_err(|error| conversion(7, error))?,
        model_id: row.get(8)?,
        state: turn_state_from_i64(row.get(9)?).map_err(|error| conversion(9, error))?,
        effect_id: row
            .get::<_, Option<String>>(10)?
            .map(EffectId::new)
            .transpose()
            .map_err(|error| conversion(10, error))?,
        assistant_message_id: row
            .get::<_, Option<String>>(11)?
            .map(MessageId::new)
            .transpose()
            .map_err(|error| conversion(11, error))?,
        failure,
        provider_response_id: row.get(15)?,
        citations,
        usage: ConversationUsage {
            input_tokens: unsigned(row, 17)?,
            output_tokens: unsigned(row, 18)?,
            cost_in_usd_ticks: unsigned(row, 19)?,
        },
        zero_data_retention: row.get::<_, Option<i64>>(20)?.map(|value| value != 0),
        revision: unsigned(row, 21)?,
        created_at: unsigned(row, 22)?,
        updated_at: unsigned(row, 23)?,
    })
    .map_err(|error| conversion(9, error))
}

fn fingerprint(row: &Row<'_>, index: usize, optional: bool) -> rusqlite::Result<Option<[u8; 32]>> {
    let value = if optional {
        row.get::<_, Option<Vec<u8>>>(index)?
    } else {
        Some(row.get::<_, Vec<u8>>(index)?)
    };
    value
        .map(|value| {
            value
                .try_into()
                .map_err(|_| conversion(index, "invalid fingerprint length"))
        })
        .transpose()
}

fn encode_citations(values: &[ConversationCitation]) -> Result<String, StoreError> {
    let values = values
        .iter()
        .map(|citation| StoredCitation {
            title: citation.title.clone(),
            url: citation.url.clone(),
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&values)
        .map_err(|error| StoreError::Internal(format!("failed to encode citations: {error}")))
}

fn failure_parts(
    failure: Option<&ConversationFailure>,
) -> (Option<i64>, Option<&str>, Option<i64>) {
    failure.map_or((None, None, None), |failure| {
        (
            Some(failure_kind_to_i64(failure.kind)),
            Some(failure.message.as_str()),
            Some(i64::from(failure.retryable)),
        )
    })
}

fn summarize_usage_rows(
    connection: &Connection,
    scope: UsageScope,
    window: UsageWindow,
    as_of: UnixMillis,
) -> Result<UsageSummary, StoreError> {
    let completed = turn_state_to_i64(ConversationTurnState::Completed);
    let lower = window_lower_bound(window, as_of).map(|value| i64::try_from(value).unwrap_or(0));
    let as_of_i64 = i64::try_from(as_of).unwrap_or(i64::MAX);

    // Scope existence fails closed for project/thread so callers cannot invent IDs.
    match &scope {
        UsageScope::Workspace => {}
        UsageScope::Project(project_id) => {
            let exists: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM projects WHERE id=?1)",
                    [project_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if !exists {
                return Err(StoreError::NotFound);
            }
        }
        UsageScope::Thread(thread_id) => {
            let exists: bool = connection
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM threads WHERE id=?1)",
                    [thread_id.as_str()],
                    |row| row.get(0),
                )
                .map_err(map_sqlite)?;
            if !exists {
                return Err(StoreError::NotFound);
            }
        }
    }

    let (input_tokens, output_tokens, cost_in_usd_ticks, turn_count) = match (&scope, lower) {
        (UsageScope::Workspace, None) => connection
            .query_row(
                "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                        COALESCE(SUM(cost_in_usd_ticks),0), COUNT(*)
                 FROM conversation_turns
                 WHERE state=?1 AND created_at<=?2",
                params![completed, as_of_i64],
                sum_usage_row,
            )
            .map_err(map_sqlite)?,
        (UsageScope::Workspace, Some(since)) => connection
            .query_row(
                "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                        COALESCE(SUM(cost_in_usd_ticks),0), COUNT(*)
                 FROM conversation_turns
                 WHERE state=?1 AND created_at>=?2 AND created_at<=?3",
                params![completed, since, as_of_i64],
                sum_usage_row,
            )
            .map_err(map_sqlite)?,
        (UsageScope::Project(project_id), None) => connection
            .query_row(
                "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                        COALESCE(SUM(cost_in_usd_ticks),0), COUNT(*)
                 FROM conversation_turns
                 WHERE state=?1 AND project_id=?2 AND created_at<=?3",
                params![completed, project_id.as_str(), as_of_i64],
                sum_usage_row,
            )
            .map_err(map_sqlite)?,
        (UsageScope::Project(project_id), Some(since)) => connection
            .query_row(
                "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                        COALESCE(SUM(cost_in_usd_ticks),0), COUNT(*)
                 FROM conversation_turns
                 WHERE state=?1 AND project_id=?2 AND created_at>=?3 AND created_at<=?4",
                params![completed, project_id.as_str(), since, as_of_i64],
                sum_usage_row,
            )
            .map_err(map_sqlite)?,
        (UsageScope::Thread(thread_id), None) => connection
            .query_row(
                "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                        COALESCE(SUM(cost_in_usd_ticks),0), COUNT(*)
                 FROM conversation_turns
                 WHERE state=?1 AND thread_id=?2 AND created_at<=?3",
                params![completed, thread_id.as_str(), as_of_i64],
                sum_usage_row,
            )
            .map_err(map_sqlite)?,
        (UsageScope::Thread(thread_id), Some(since)) => connection
            .query_row(
                "SELECT COALESCE(SUM(input_tokens),0), COALESCE(SUM(output_tokens),0),
                        COALESCE(SUM(cost_in_usd_ticks),0), COUNT(*)
                 FROM conversation_turns
                 WHERE state=?1 AND thread_id=?2 AND created_at>=?3 AND created_at<=?4",
                params![completed, thread_id.as_str(), since, as_of_i64],
                sum_usage_row,
            )
            .map_err(map_sqlite)?,
    };

    Ok(UsageSummary {
        input_tokens,
        output_tokens,
        cost_in_usd_ticks,
        turn_count,
        scope,
        window,
        as_of,
    })
}

fn sum_usage_row(row: &Row<'_>) -> rusqlite::Result<(u64, u64, u64, u64)> {
    let read = |index: usize| -> rusqlite::Result<u64> {
        let value: i64 = row.get(index)?;
        u64::try_from(value).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                index,
                rusqlite::types::Type::Integer,
                Box::new(error),
            )
        })
    };
    Ok((read(0)?, read(1)?, read(2)?, read(3)?))
}

const fn turn_state_to_i64(value: ConversationTurnState) -> i64 {
    match value {
        ConversationTurnState::Reserved => 0,
        ConversationTurnState::ProviderStarted => 1,
        ConversationTurnState::Completed => 2,
        ConversationTurnState::Failed => 3,
        ConversationTurnState::Cancelled => 4,
        ConversationTurnState::InterruptedNeedsReview => 5,
    }
}

fn turn_state_from_i64(value: i64) -> Result<ConversationTurnState, &'static str> {
    match value {
        0 => Ok(ConversationTurnState::Reserved),
        1 => Ok(ConversationTurnState::ProviderStarted),
        2 => Ok(ConversationTurnState::Completed),
        3 => Ok(ConversationTurnState::Failed),
        4 => Ok(ConversationTurnState::Cancelled),
        5 => Ok(ConversationTurnState::InterruptedNeedsReview),
        _ => Err("invalid conversation turn state"),
    }
}

const fn failure_kind_to_i64(value: ConversationFailureKind) -> i64 {
    match value {
        ConversationFailureKind::Authentication => 0,
        ConversationFailureKind::Forbidden => 1,
        ConversationFailureKind::InvalidRequest => 2,
        ConversationFailureKind::RateLimited => 3,
        ConversationFailureKind::Unavailable => 4,
        ConversationFailureKind::Protocol => 5,
    }
}

fn failure_kind_from_i64(value: i64) -> Result<ConversationFailureKind, &'static str> {
    match value {
        0 => Ok(ConversationFailureKind::Authentication),
        1 => Ok(ConversationFailureKind::Forbidden),
        2 => Ok(ConversationFailureKind::InvalidRequest),
        3 => Ok(ConversationFailureKind::RateLimited),
        4 => Ok(ConversationFailureKind::Unavailable),
        5 => Ok(ConversationFailureKind::Protocol),
        _ => Err("invalid conversation failure kind"),
    }
}

fn unsigned(row: &Row<'_>, index: usize) -> rusqlite::Result<u64> {
    let value: i64 = row.get(index)?;
    value.try_into().map_err(|error| conversion(index, error))
}

fn conversion(index: usize, error: impl std::fmt::Display) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        index,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error.to_string(),
        )),
    )
}

fn begin(connection: &mut Connection) -> Result<Transaction<'_>, StoreError> {
    connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(map_sqlite)
}

fn commit(transaction: Transaction<'_>) -> Result<(), StoreError> {
    transaction.commit().map_err(map_sqlite)
}
