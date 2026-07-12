//! Adversarial `SQLCipher` conversation aggregate and linkage coverage.

use std::{path::Path, sync::Arc};

use grok_application::{
    CancelConversationTurnCommit, ConversationForkDeliveryState, ConversationForkPlan,
    ConversationForkTurnPlan, ConversationThreadCredentialBinding, ConversationTurnReservation,
    ConversationTurnReservationSource, ConversationTurnSnapshot, ConversationTurnStore,
    CreateMessage, CreateProject, CreateThread, MAX_CONVERSATION_FORK_DELIVERY_ALIASES,
    MutationCommand, NewRunEvent, ProviderStartCommit, StoreError, TerminalTurnCommit,
    WorkspaceService, WorkspaceStore,
};
use grok_domain::{
    ChatRail, ConversationCitation, ConversationFailure, ConversationFailureKind,
    ConversationForkKind, ConversationMessageDerivationKind, ConversationTurn,
    ConversationTurnEventKind, ConversationTurnId, ConversationTurnLineage, ConversationTurnOrigin,
    ConversationTurnState, ConversationUsage, EffectId, EffectKind, EffectState, Idempotency,
    MAX_CONVERSATION_TEXT_CHUNK_BYTES, MAX_MESSAGE_BYTES, Message, MessageId, MessageRole,
    MessageState, ProjectId, Run, RunEventKind, RunId, RunState, SideEffect, Thread, ThreadId,
};
use grok_memory::{EphemeralKeyProvider, FixedClock, SequentialIdGenerator};
use grok_sqlcipher::SqlCipherStore;

const TEST_CREDENTIAL_BINDING: &str = "xai-binding-test-generation";

#[tokio::test]
async fn provider_start_rejects_forged_aggregates_and_returns_persisted_snapshot() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory.path().join("forged-provider-start.db"),
        Arc::new(EphemeralKeyProvider::new([81; 32])),
    )
    .await;
    let (project_id, thread_id) = seed_workspace(&store, "provider-start").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 1).await;
    assert_eq!(
        store
            .load_turn(&reserved.snapshot.turn.id)
            .await
            .expect("load reserved turn by ID"),
        Some(reserved.snapshot.clone())
    );
    assert_eq!(
        store
            .load_turn(&ConversationTurnId::new("missing-turn").expect("missing turn ID"))
            .await
            .expect("load missing turn by ID"),
        None
    );
    let canonical = provider_start_commit(&reserved.snapshot, 1);

    let mut forged_turn_identity = canonical.clone();
    forged_turn_identity.turn.model_id = "forged-model".into();
    let mut forged_run_identity = canonical.clone();
    forged_run_identity.run.project_id = ProjectId::new("forged-project").expect("project id");
    let mut non_executing_effect = canonical.clone();
    non_executing_effect.effect.state = EffectState::Prepared;
    let mut forged_effect_target = canonical.clone();
    forged_effect_target.effect.target = "forged target".into();
    let mut forged_effect_policy = canonical.clone();
    forged_effect_policy.effect.idempotency = Idempotency::Idempotent;
    let mut forged_events = canonical.clone();
    forged_events.events.clear();
    let mut forged_turn_event = canonical.clone();
    forged_turn_event.turn_event = ConversationTurnEventKind::StateChanged {
        from: ConversationTurnState::Reserved,
        to: ConversationTurnState::Cancelled,
    };

    for (case, forged) in [
        ("turn identity", forged_turn_identity),
        ("run identity", forged_run_identity),
        ("effect state", non_executing_effect),
        ("effect target", forged_effect_target),
        ("effect retry policy", forged_effect_policy),
        ("audit events", forged_events),
        ("turn event", forged_turn_event),
    ] {
        assert!(
            matches!(
                store.commit_provider_start(forged).await,
                Err(StoreError::Conflict)
            ),
            "forged provider start was accepted: {case}"
        );
        assert_eq!(load_turn(&store, 1).await, reserved.snapshot, "{case}");
    }

    let started = store
        .commit_provider_start(canonical)
        .await
        .expect("canonical provider start");
    assert_eq!(started.turn.state, ConversationTurnState::ProviderStarted);
    assert_eq!(
        started.effect.as_ref().expect("effect").state,
        EffectState::Executing
    );
    assert_eq!(started, load_turn(&store, 1).await);
}

#[tokio::test]
async fn terminal_commit_rejects_forged_turn_run_effect_message_and_events() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory.path().join("forged-terminal.db"),
        Arc::new(EphemeralKeyProvider::new([82; 32])),
    )
    .await;
    let (project_id, thread_id) = seed_workspace(&store, "terminal").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 2).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 2))
        .await
        .expect("provider start");
    append_completed_text(&store, &started, 2).await;
    let canonical = completed_commit(&started, 2);

    let mut forged_turn_identity = canonical.clone();
    forged_turn_identity.turn.idempotency_key = "forged-command".into();
    let mut forged_run_identity = canonical.clone();
    forged_run_identity.run.thread_id = ThreadId::new("forged-thread").expect("thread id");
    let mut mismatched_states = canonical.clone();
    mismatched_states.run.state = RunState::Failed;
    let mut forged_effect = canonical.clone();
    forged_effect.effect.as_mut().expect("effect").state = EffectState::Failed;
    let mut revised_assistant = canonical.clone();
    revised_assistant
        .assistant_message
        .as_mut()
        .expect("assistant")
        .revision = 1;
    let mut deleted_assistant = canonical.clone();
    deleted_assistant
        .assistant_message
        .as_mut()
        .expect("assistant")
        .state = MessageState::Deleted;
    let mut sequenced_assistant = canonical.clone();
    sequenced_assistant
        .assistant_message
        .as_mut()
        .expect("assistant")
        .sequence = 99;
    let mut forged_events = canonical.clone();
    forged_events.events.clear();
    let mut forged_turn_event = canonical.clone();
    forged_turn_event.turn_event = ConversationTurnEventKind::StateChanged {
        from: ConversationTurnState::ProviderStarted,
        to: ConversationTurnState::Failed,
    };

    for (case, forged) in [
        ("turn identity", forged_turn_identity),
        ("run identity", forged_run_identity),
        ("cross-aggregate states", mismatched_states),
        ("effect state", forged_effect),
        ("assistant revision", revised_assistant),
        ("assistant lifecycle", deleted_assistant),
        ("assistant sequence", sequenced_assistant),
        ("audit events", forged_events),
        ("turn event", forged_turn_event),
    ] {
        assert!(
            matches!(
                store.commit_terminal(forged).await,
                Err(StoreError::Conflict)
            ),
            "forged terminal commit was accepted: {case}"
        );
        assert_eq!(load_turn(&store, 2).await, started, "{case}");
    }

    assert_eq!(
        canonical
            .assistant_message
            .as_ref()
            .expect("canonical assistant")
            .sequence,
        0
    );
    let completed = store
        .commit_terminal(canonical)
        .await
        .expect("canonical completion");
    assert_eq!(completed.turn.state, ConversationTurnState::Completed);
    assert!(
        completed
            .assistant_message
            .as_ref()
            .expect("persisted assistant")
            .sequence
            > completed.user_message.sequence
    );
    assert_eq!(completed, load_turn(&store, 2).await);
}

#[tokio::test]
async fn linked_conversation_corruption_fails_closed_on_every_snapshot_load() {
    let corruptions = [
        (
            "run ownership",
            "UPDATE runs SET project_id='forged-project' WHERE id=(SELECT run_id FROM conversation_turns LIMIT 1)",
        ),
        (
            "run state",
            "UPDATE runs SET state=6 WHERE id=(SELECT run_id FROM conversation_turns LIMIT 1)",
        ),
        (
            "effect state",
            "UPDATE side_effects SET state=3 WHERE id=(SELECT effect_id FROM conversation_turns LIMIT 1)",
        ),
        (
            "effect target",
            "UPDATE side_effects SET target='forged target' WHERE id=(SELECT effect_id FROM conversation_turns LIMIT 1)",
        ),
        (
            "user message state",
            "UPDATE messages SET state=1 WHERE id=(SELECT user_message_id FROM conversation_turns LIMIT 1)",
        ),
        (
            "assistant message revision",
            "UPDATE messages SET revision=1 WHERE id=(SELECT assistant_message_id FROM conversation_turns LIMIT 1)",
        ),
    ];

    for (case, corruption) in corruptions {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("linked-corruption.db");
        let key = Arc::new(EphemeralKeyProvider::new([83; 32]));
        let store = open(&path, key.clone()).await;
        let (project_id, thread_id) = seed_workspace(&store, "corruption").await;
        let reserved = reserve_turn(&store, project_id, thread_id.clone(), 3).await;
        let started = store
            .commit_provider_start(provider_start_commit(&reserved.snapshot, 3))
            .await
            .expect("provider start");
        append_completed_text(&store, &started, 3).await;
        store
            .commit_terminal(completed_commit(&started, 3))
            .await
            .expect("completion");
        drop(store);

        let connection = rusqlite::Connection::open(&path).expect("raw encrypted connection");
        connection
            .execute_batch(&format!(
                "PRAGMA key = \"x'{}'\";",
                hex::encode([83_u8; 32])
            ))
            .expect("unlock encrypted fixture");
        connection
            .execute(corruption, [])
            .expect("inject linked corruption");
        drop(connection);

        let reopened = open(&path, key).await;
        assert!(
            matches!(
                reopened.load_turn_by_command(&command(3)).await,
                Err(StoreError::Internal(message))
                    if message == "invalid persisted conversation aggregate"
            ),
            "command load did not fail closed: {case}"
        );
        assert!(
            matches!(
                reopened.list_thread_turns(&thread_id, None, 10).await,
                Err(StoreError::Internal(message))
                    if message == "invalid persisted conversation aggregate"
            ),
            "history load did not fail closed: {case}"
        );
        assert!(
            matches!(
                reopened
                    .load_turn(&ConversationTurnId::new("security-turn-3").expect("turn ID"))
                    .await,
                Err(StoreError::Internal(message))
                    if message == "invalid persisted conversation aggregate"
            ),
            "identity load did not fail closed: {case}"
        );
    }
}

#[tokio::test]
async fn restart_context_excludes_noncompleted_prompts_and_keeps_completed_history() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("context-filter.db");
    let key = Arc::new(EphemeralKeyProvider::new([84; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "context").await;

    let cancelled = reserve_turn(&store, project_id.clone(), thread_id.clone(), 10).await;
    assert_eq!(
        cancelled.context,
        vec![cancelled.snapshot.user_message.clone()]
    );
    store
        .commit_terminal(cancelled_commit(&cancelled.snapshot))
        .await
        .expect("cancelled turn");

    let failed = reserve_turn(&store, project_id.clone(), thread_id.clone(), 11).await;
    let failed_started = store
        .commit_provider_start(provider_start_commit(&failed.snapshot, 11))
        .await
        .expect("failed provider start");
    store
        .commit_terminal(failed_commit(&failed_started))
        .await
        .expect("failed turn");

    let interrupted = reserve_turn(&store, project_id.clone(), thread_id.clone(), 12).await;
    let interrupted_started = store
        .commit_provider_start(provider_start_commit(&interrupted.snapshot, 12))
        .await
        .expect("interrupted provider start");
    store
        .commit_terminal(interrupted_commit(&interrupted_started))
        .await
        .expect("interrupted turn");

    let completed = reserve_turn(&store, project_id.clone(), thread_id.clone(), 13).await;
    let completed_started = store
        .commit_provider_start(provider_start_commit(&completed.snapshot, 13))
        .await
        .expect("completed provider start");
    append_completed_text(&store, &completed_started, 13).await;
    let completed = store
        .commit_terminal(completed_commit(&completed_started, 13))
        .await
        .expect("completed turn");
    drop(store);

    let reopened = open(&path, key).await;
    let next = reserve_turn(&reopened, project_id, thread_id, 14).await;
    assert_eq!(
        next.context
            .iter()
            .map(|message| message.id.clone())
            .collect::<Vec<_>>(),
        vec![
            completed.user_message.id,
            completed.assistant_message.expect("assistant").id,
            next.snapshot.user_message.id.clone(),
        ]
    );
    for excluded in [
        cancelled.snapshot.user_message.id,
        failed.snapshot.user_message.id,
        interrupted.snapshot.user_message.id,
    ] {
        assert!(next.context.iter().all(|message| message.id != excluded));
    }
}

#[tokio::test]
async fn text_events_replay_split_at_maximum_and_survive_completion_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("event-max-restart.db");
    let key = Arc::new(EphemeralKeyProvider::new([85; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "event-max").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 20).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 20))
        .await
        .expect("provider start");
    let text = "x".repeat(MAX_MESSAGE_BYTES);
    let appended = store
        .append_turn_text(&started.turn.id, started.turn.revision, 0, text.clone())
        .await
        .expect("maximum text append");
    assert_eq!(
        appended.len(),
        MAX_MESSAGE_BYTES / MAX_CONVERSATION_TEXT_CHUNK_BYTES
    );
    assert_eq!(appended.first().expect("first chunk").sequence, 3);
    assert!(appended.iter().all(|event| {
        matches!(
            &event.kind,
            ConversationTurnEventKind::TextAppended { text, .. }
                if text.len() == MAX_CONVERSATION_TEXT_CHUNK_BYTES
        )
    }));
    assert_eq!(
        store
            .append_turn_text(&started.turn.id, started.turn.revision, 0, text.clone())
            .await
            .expect("exact append replay"),
        appended
    );
    assert!(matches!(
        store
            .append_turn_text(&started.turn.id, started.turn.revision + 1, 0, text.clone(),)
            .await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        store
            .append_turn_text(
                &started.turn.id,
                started.turn.revision,
                u64::try_from(MAX_MESSAGE_BYTES).expect("maximum offset"),
                "x".into(),
            )
            .await,
        Err(StoreError::Conflict)
    ));

    let completion = completed_commit_with_text(&started, 20, text.clone());
    let completed = store
        .commit_terminal(completion)
        .await
        .expect("complete maximum response");
    drop(store);

    let reopened = open(&path, key).await;
    let snapshot = load_turn(&reopened, 20).await;
    assert_eq!(snapshot, completed);
    assert_eq!(
        reopened
            .load_turn(&completed.turn.id)
            .await
            .expect("load completed turn by ID"),
        Some(completed.clone())
    );
    let page = reopened
        .list_turn_events_since(&completed.turn.id, 0, 100)
        .await
        .expect("restarted event page");
    assert!(!page.has_more);
    assert!(matches!(
        page.events.last().map(|event| &event.kind),
        Some(ConversationTurnEventKind::StateChanged {
            from: ConversationTurnState::ProviderStarted,
            to: ConversationTurnState::Completed,
        })
    ));
    let reconstructed = page
        .events
        .iter()
        .filter_map(|event| match &event.kind {
            ConversationTurnEventKind::TextAppended { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(reconstructed, text);
}

#[tokio::test]
async fn event_pages_return_exact_hundred_with_one_lookahead() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory.path().join("event-pages.db"),
        Arc::new(EphemeralKeyProvider::new([86; 32])),
    )
    .await;
    let (project_id, thread_id) = seed_workspace(&store, "event-pages").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 21).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 21))
        .await
        .expect("provider start");
    for offset in 0..101_u64 {
        store
            .append_turn_text(&started.turn.id, started.turn.revision, offset, "x".into())
            .await
            .expect("single-byte append");
    }
    let coalesced_replay = store
        .append_turn_text(&started.turn.id, started.turn.revision, 0, "xx".into())
        .await
        .expect("coalesced exact replay");
    assert_eq!(
        coalesced_replay
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![3, 4]
    );

    let first = store
        .list_turn_events_since(&started.turn.id, 0, 100)
        .await
        .expect("first event page");
    assert_eq!(first.events.len(), 100);
    assert!(first.has_more);
    assert_eq!(first.events.last().expect("cursor event").sequence, 100);
    let second = store
        .list_turn_events_since(&started.turn.id, 100, 100)
        .await
        .expect("second event page");
    assert_eq!(second.events.len(), 3);
    assert!(!second.has_more);
    assert!(matches!(
        store.list_turn_events_since(&started.turn.id, 0, 101).await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        store.list_turn_events_since(&started.turn.id, 0, 0).await,
        Err(StoreError::Conflict)
    ));
}

#[tokio::test]
async fn lifecycle_event_fault_rolls_back_provider_start_atomically() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("event-fault.db");
    let key = Arc::new(EphemeralKeyProvider::new([87; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "event-fault").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 22).await;
    let provider_start = provider_start_commit(&reserved.snapshot, 22);
    drop(store);

    let connection = raw_connection(&path, [87; 32]);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_conversation_state_event
             BEFORE INSERT ON conversation_turn_events WHEN new.kind=1 BEGIN
                 SELECT RAISE(ABORT, 'injected conversation event failure');
             END;",
        )
        .expect("install event fault");
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened.commit_provider_start(provider_start).await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(load_turn(&reopened, 22).await, reserved.snapshot);
    let events = reopened
        .list_turn_events_since(&reserved.snapshot.turn.id, 0, 100)
        .await
        .expect("reservation events after rollback");
    assert_eq!(events.events.len(), 1);
    assert_eq!(events.events[0].kind, ConversationTurnEventKind::Created);
}

#[tokio::test]
async fn forged_event_sequence_fails_closed_before_paged_materialization() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("event-corruption.db");
    let key = Arc::new(EphemeralKeyProvider::new([88; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "event-corruption").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 23).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 23))
        .await
        .expect("provider start");
    drop(store);

    let connection = raw_connection(&path, [88; 32]);
    connection
        .execute_batch(
            "DROP TRIGGER conversation_turn_events_immutable_update;
             UPDATE conversation_turn_events SET sequence=4
             WHERE turn_id='security-turn-23' AND sequence=2;",
        )
        .expect("forge event gap");
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened
            .list_turn_events_since(&started.turn.id, 0, 100)
            .await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
    assert!(matches!(
        reopened.load_turn_by_command(&command(23)).await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
}

#[tokio::test]
async fn completed_assistant_must_equal_durable_text_projection() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory.path().join("event-text-mismatch.db"),
        Arc::new(EphemeralKeyProvider::new([89; 32])),
    )
    .await;
    let (project_id, thread_id) = seed_workspace(&store, "event-mismatch").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 24).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 24))
        .await
        .expect("provider start");
    store
        .append_turn_text(
            &started.turn.id,
            started.turn.revision,
            0,
            "durable answer".into(),
        )
        .await
        .expect("append durable answer");
    assert!(matches!(
        store
            .commit_terminal(completed_commit_with_text(
                &started,
                24,
                "different answer".into(),
            ))
            .await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(load_turn(&store, 24).await, started);
    let events = store
        .list_turn_events_since(&started.turn.id, 0, 100)
        .await
        .expect("events after rejected completion");
    assert_eq!(events.events.len(), 3);
    assert!(matches!(
        &events.events[2].kind,
        ConversationTurnEventKind::TextAppended { text, .. } if text == "durable answer"
    ));
}

#[tokio::test]
async fn cancellation_command_commits_and_replays_atomically_across_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("cancellation-replay.db");
    let key = Arc::new(EphemeralKeyProvider::new([90; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "cancellation-replay").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 30).await;
    let command = cancel_command("reserved", [130; 32]);
    let cancellation = CancelConversationTurnCommit {
        command: command.clone(),
        turn_id: reserved.snapshot.turn.id.clone(),
        expected_turn_revision: reserved.snapshot.turn.revision,
        terminal: Some(cancelled_commit(&reserved.snapshot)),
    };

    let cancelled = store
        .commit_cancellation(cancellation)
        .await
        .expect("commit cancellation command");
    assert_eq!(cancelled.turn.state, ConversationTurnState::Cancelled);
    assert_eq!(cancelled.turn.revision, 1);
    assert_eq!(cancelled, load_turn(&store, 30).await);

    let replay = store
        .commit_cancellation(CancelConversationTurnCommit {
            command: command.clone(),
            turn_id: cancelled.turn.id.clone(),
            expected_turn_revision: u64::MAX,
            terminal: None,
        })
        .await
        .expect("exact in-process replay");
    assert_eq!(replay, cancelled);
    assert!(matches!(
        store
            .commit_cancellation(CancelConversationTurnCommit {
                command: cancel_command("reserved", [131; 32]),
                turn_id: cancelled.turn.id.clone(),
                expected_turn_revision: 0,
                terminal: None,
            })
            .await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        store
            .commit_cancellation(CancelConversationTurnCommit {
                command: command.clone(),
                turn_id: ConversationTurnId::new("different-turn").expect("different turn ID"),
                expected_turn_revision: 0,
                terminal: None,
            })
            .await,
        Err(StoreError::Conflict)
    ));
    drop(store);

    let reopened = open(&path, key).await;
    let restarted_replay = reopened
        .commit_cancellation(CancelConversationTurnCommit {
            command,
            turn_id: cancelled.turn.id.clone(),
            expected_turn_revision: u64::MAX,
            terminal: None,
        })
        .await
        .expect("exact restarted replay");
    assert_eq!(restarted_replay, cancelled);
}

#[tokio::test]
async fn renderer_cancellation_key_cannot_poison_internal_reconciliation_across_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("cancellation-scope-separation.db");
    let key = Arc::new(EphemeralKeyProvider::new([94; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "cancellation-scope").await;
    let external_reserved = reserve_turn(&store, project_id.clone(), thread_id.clone(), 34).await;
    let shared_key = "predicted-dispatch-exit-key";
    let external_command = cancel_command(shared_key, [136; 32]);
    let external = store
        .commit_cancellation(CancelConversationTurnCommit {
            command: external_command.clone(),
            turn_id: external_reserved.snapshot.turn.id.clone(),
            expected_turn_revision: external_reserved.snapshot.turn.revision,
            terminal: Some(cancelled_commit(&external_reserved.snapshot)),
        })
        .await
        .expect("bind renderer-controlled cancellation key");

    let internal_reserved = reserve_turn(&store, project_id, thread_id, 35).await;
    let internal_command = reconciliation_command(shared_key, [137; 32]);
    let internal_commit = CancelConversationTurnCommit {
        command: internal_command.clone(),
        turn_id: internal_reserved.snapshot.turn.id.clone(),
        expected_turn_revision: internal_reserved.snapshot.turn.revision,
        terminal: Some(cancelled_commit(&internal_reserved.snapshot)),
    };
    assert!(matches!(
        store.commit_cancellation(internal_commit.clone()).await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(load_turn(&store, 35).await, internal_reserved.snapshot);
    let internal = store
        .commit_dispatch_exit_reconciliation(internal_commit)
        .await
        .expect("internal scope remains independent of renderer key");
    assert_eq!(internal.turn.state, ConversationTurnState::Cancelled);

    drop(store);
    let reopened = open(&path, key).await;
    let external_replay = reopened
        .commit_cancellation(CancelConversationTurnCommit {
            command: external_command,
            turn_id: external.turn.id.clone(),
            expected_turn_revision: u64::MAX,
            terminal: None,
        })
        .await
        .expect("external scoped replay after restart");
    let internal_replay = reopened
        .commit_dispatch_exit_reconciliation(CancelConversationTurnCommit {
            command: internal_command,
            turn_id: internal.turn.id.clone(),
            expected_turn_revision: u64::MAX,
            terminal: None,
        })
        .await
        .expect("internal scoped replay after restart");
    assert_eq!(external_replay, external);
    assert_eq!(internal_replay, internal);
}

#[tokio::test]
async fn corrupted_cancellation_scope_fails_closed_in_both_namespaces() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("cancellation-scope-corruption.db");
    let key = Arc::new(EphemeralKeyProvider::new([95; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "cancellation-scope-corruption").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 36).await;
    let external_command = cancel_command("scope-corruption", [138; 32]);
    let cancelled = store
        .commit_cancellation(CancelConversationTurnCommit {
            command: external_command.clone(),
            turn_id: reserved.snapshot.turn.id.clone(),
            expected_turn_revision: reserved.snapshot.turn.revision,
            terminal: Some(cancelled_commit(&reserved.snapshot)),
        })
        .await
        .expect("persist external cancellation");
    drop(store);

    let connection = raw_connection(&path, [95; 32]);
    connection
        .execute_batch(
            "DROP TRIGGER conversation_turn_cancel_commands_immutable_update;
             UPDATE conversation_turn_cancel_commands
             SET command_scope='reconcile_conversation_dispatch_exit'
             WHERE command_scope='cancel_conversation_turn';",
        )
        .expect("inject cancellation scope corruption");
    assert!(
        connection
            .execute(
                "UPDATE conversation_turn_cancel_commands SET command_scope='forged_scope'",
                [],
            )
            .is_err()
    );
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened
            .commit_cancellation(CancelConversationTurnCommit {
                command: external_command,
                turn_id: cancelled.turn.id.clone(),
                expected_turn_revision: u64::MAX,
                terminal: None,
            })
            .await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        reopened
            .commit_dispatch_exit_reconciliation(CancelConversationTurnCommit {
                command: reconciliation_command("scope-corruption", [139; 32]),
                turn_id: cancelled.turn.id,
                expected_turn_revision: u64::MAX,
                terminal: None,
            })
            .await,
        Err(StoreError::Conflict)
    ));
}

#[tokio::test]
async fn cancellation_command_binds_the_exact_terminal_race_winner() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory.path().join("cancellation-race.db"),
        Arc::new(EphemeralKeyProvider::new([91; 32])),
    )
    .await;
    let (project_id, thread_id) = seed_workspace(&store, "cancellation-race").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 31).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 31))
        .await
        .expect("provider start");
    let cancellation_candidate = interrupted_commit(&started);
    let winner = store
        .commit_terminal(failed_commit(&started))
        .await
        .expect("concurrent terminal winner");
    let command = cancel_command("race", [132; 32]);

    let bound = store
        .commit_cancellation(CancelConversationTurnCommit {
            command: command.clone(),
            turn_id: started.turn.id.clone(),
            expected_turn_revision: started.turn.revision,
            terminal: Some(cancellation_candidate),
        })
        .await
        .expect("bind race winner");
    assert_eq!(bound, winner);
    assert_eq!(bound.turn.state, ConversationTurnState::Failed);
    assert!(matches!(
        store
            .commit_cancellation(CancelConversationTurnCommit {
                command: cancel_command("wrong-race-revision", [133; 32]),
                turn_id: winner.turn.id.clone(),
                expected_turn_revision: winner.turn.revision,
                terminal: None,
            })
            .await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(
        store
            .commit_cancellation(CancelConversationTurnCommit {
                command,
                turn_id: winner.turn.id.clone(),
                expected_turn_revision: 0,
                terminal: None,
            })
            .await
            .expect("record replay ignores a new stale candidate"),
        winner
    );
}

#[tokio::test]
async fn cancellation_record_fault_rolls_back_the_terminal_transition() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("cancellation-fault.db");
    let key = Arc::new(EphemeralKeyProvider::new([92; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "cancellation-fault").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 32).await;
    let cancellation = CancelConversationTurnCommit {
        command: cancel_command("fault", [134; 32]),
        turn_id: reserved.snapshot.turn.id.clone(),
        expected_turn_revision: reserved.snapshot.turn.revision,
        terminal: Some(cancelled_commit(&reserved.snapshot)),
    };
    drop(store);

    let connection = raw_connection(&path, [92; 32]);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_conversation_cancellation_record
             BEFORE INSERT ON conversation_turn_cancel_commands BEGIN
                 SELECT RAISE(ABORT, 'injected cancellation record failure');
             END;",
        )
        .expect("install cancellation record fault");
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened.commit_cancellation(cancellation).await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(load_turn(&reopened, 32).await, reserved.snapshot);
    let events = reopened
        .list_turn_events_since(&reserved.snapshot.turn.id, 0, 100)
        .await
        .expect("events after cancellation rollback");
    assert_eq!(events.events.len(), 1);
    assert_eq!(events.events[0].kind, ConversationTurnEventKind::Created);
}

#[tokio::test]
async fn cancellation_command_rejects_a_non_cancellation_terminal_edge() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory.path().join("cancellation-edge.db"),
        Arc::new(EphemeralKeyProvider::new([93; 32])),
    )
    .await;
    let (project_id, thread_id) = seed_workspace(&store, "cancellation-edge").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 33).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 33))
        .await
        .expect("provider start");

    assert!(matches!(
        store
            .commit_cancellation(CancelConversationTurnCommit {
                command: cancel_command("wrong-edge", [135; 32]),
                turn_id: started.turn.id.clone(),
                expected_turn_revision: started.turn.revision,
                terminal: Some(failed_commit(&started)),
            })
            .await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(load_turn(&store, 33).await, started);
}

#[tokio::test]
async fn retry_reservation_reuses_exact_context_and_replays_in_its_own_scope_after_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("retry-context-replay.db");
    let key = Arc::new(EphemeralKeyProvider::new([96; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "retry-context").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 40).await;
    let source = store
        .commit_terminal(cancelled_commit(&reserved.snapshot))
        .await
        .expect("cancel retry source");
    assert!(
        store
            .retry_source_is_latest(&source.turn.id)
            .await
            .expect("eligible retry source")
    );
    let candidate = retry_candidate(&source, 41);
    let retried = store
        .reserve_turn(
            candidate.0.clone(),
            candidate.1.clone(),
            ConversationTurnReservationSource::Retry {
                source_turn_id: source.turn.id.clone(),
                expected_source_revision: source.turn.revision,
            },
            candidate.2.clone(),
            candidate.3.clone(),
            candidate.4.clone(),
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("reserve retry");
    assert!(retried.created);
    assert_eq!(retried.context.len(), 1);
    assert_eq!(retried.context[0], retried.snapshot.user_message);
    assert_eq!(retried.context[0].content, source.user_message.content);
    assert_eq!(retried.snapshot.lineage, candidate.1);
    assert!(
        !store
            .retry_source_is_latest(&source.turn.id)
            .await
            .expect("source after retry child")
    );

    let replay = store
        .reserve_turn(
            candidate.0.clone(),
            candidate.1.clone(),
            ConversationTurnReservationSource::Retry {
                source_turn_id: source.turn.id.clone(),
                expected_source_revision: source.turn.revision,
            },
            candidate.2,
            candidate.3,
            candidate.4,
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("exact retry reservation replay");
    assert!(!replay.created);
    assert_eq!(replay.snapshot, retried.snapshot);
    let retry_command = MutationCommand {
        scope: "retry_conversation_turn".into(),
        key: candidate.0.idempotency_key.clone(),
        fingerprint: candidate.0.request_fingerprint,
    };
    assert_eq!(
        store
            .load_turn_by_command(&retry_command)
            .await
            .expect("load retry command"),
        Some(retried.snapshot.clone())
    );
    let mut wrong_scope = retry_command.clone();
    wrong_scope.scope = "execute_conversation_turn".into();
    assert!(matches!(
        store.load_turn_by_command(&wrong_scope).await,
        Err(StoreError::Conflict)
    ));
    drop(store);

    let reopened = open(&path, key).await;
    assert_eq!(
        reopened
            .load_turn_by_command(&retry_command)
            .await
            .expect("restart retry command"),
        Some(retried.snapshot)
    );
}

#[tokio::test]
async fn sealed_cancelled_turn_context_rejects_raw_insert_update_and_delete() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("sealed-turn-context.db");
    let key = Arc::new(EphemeralKeyProvider::new([103; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "sealed-context").await;
    let reserved = reserve_turn(&store, project_id, thread_id, 57).await;
    let cancelled = store
        .commit_terminal(cancelled_commit(&reserved.snapshot))
        .await
        .expect("cancel sealed source");
    let expected_context = store
        .load_turn_context(&cancelled.turn.id)
        .await
        .expect("sealed source context");
    drop(store);

    let connection = raw_connection(&path, [103; 32]);
    assert!(
        connection
            .execute(
                "INSERT INTO conversation_turn_context(
                     turn_id,sequence,message_id,role,content,revision,created_at,updated_at
                 ) VALUES (
                     'security-turn-57',2,'forged-context-message',1,
                     'forged context',0,570,570
                 )",
                [],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "UPDATE conversation_turn_context SET content='forged context'
                 WHERE turn_id='security-turn-57'",
                [],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "DELETE FROM conversation_turn_context
                 WHERE turn_id='security-turn-57'",
                [],
            )
            .is_err()
    );
    let context_count: u32 = connection
        .query_row(
            "SELECT count(*) FROM conversation_turn_context
             WHERE turn_id='security-turn-57'",
            [],
            |row| row.get(0),
        )
        .expect("sealed context count");
    assert_eq!(
        context_count,
        u32::try_from(expected_context.len()).expect("bounded context count")
    );
    drop(connection);

    let reopened = open(&path, key).await;
    assert_eq!(
        reopened
            .load_turn_context(&cancelled.turn.id)
            .await
            .expect("context after rejected raw forgeries"),
        expected_context
    );
}

#[tokio::test]
async fn retry_reservation_rejects_forged_or_non_latest_source_material_atomically() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory.path().join("retry-rejections.db"),
        Arc::new(EphemeralKeyProvider::new([97; 32])),
    )
    .await;
    let (project_id, thread_id) = seed_workspace(&store, "retry-rejections").await;
    let reserved = reserve_turn(&store, project_id, thread_id.clone(), 42).await;
    let source = store
        .commit_terminal(cancelled_commit(&reserved.snapshot))
        .await
        .expect("cancel retry source");

    for case in ["revision", "prompt", "model", "binding", "rail", "depth"] {
        let mut candidate = retry_candidate(&source, 43);
        let mut expected_revision = source.turn.revision;
        match case {
            "revision" => expected_revision = expected_revision.saturating_add(1),
            "prompt" => candidate.2.content = "forged prompt".into(),
            "model" => candidate.0.model_id = "forged-model".into(),
            "binding" => candidate.1.credential_binding_id = Some("forged-binding".into()),
            "rail" => candidate.1.rail = ChatRail::SuperGrokApi,
            "depth" => candidate.1.retry_depth = 2,
            _ => unreachable!(),
        }
        assert!(
            matches!(
                store
                    .reserve_turn(
                        candidate.0,
                        candidate.1,
                        ConversationTurnReservationSource::Retry {
                            source_turn_id: source.turn.id.clone(),
                            expected_source_revision: expected_revision,
                        },
                        candidate.2,
                        candidate.3,
                        candidate.4,
                        ConversationTurnEventKind::Created,
                    )
                    .await,
                Err(StoreError::Conflict)
            ),
            "forged retry case was accepted: {case}"
        );
    }

    let workspace = WorkspaceService::new(
        store.clone(),
        Arc::new(FixedClock::new(source.turn.updated_at + 1)),
        Arc::new(SequentialIdGenerator::new()),
    );
    workspace
        .create_message(
            CreateMessage {
                thread_id: thread_id.to_string(),
                role: MessageRole::System,
                content: "Later canonical message".into(),
            },
            "later-retry-message",
        )
        .await
        .expect("append later message");
    assert!(
        !store
            .retry_source_is_latest(&source.turn.id)
            .await
            .expect("stale retry source")
    );
    let candidate = retry_candidate(&source, 43);
    assert!(matches!(
        store
            .reserve_turn(
                candidate.0,
                candidate.1,
                ConversationTurnReservationSource::Retry {
                    source_turn_id: source.turn.id.clone(),
                    expected_source_revision: source.turn.revision,
                },
                candidate.2,
                candidate.3,
                candidate.4,
                ConversationTurnEventKind::Created,
            )
            .await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(
        store
            .list_thread_turns(&thread_id, None, 10)
            .await
            .expect("turns after rejected retries")
            .len(),
        1
    );
}

#[tokio::test]
async fn thread_generation_binds_once_and_rejects_key_replacement_across_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("thread-generation-binding.db");
    let key = Arc::new(EphemeralKeyProvider::new([98; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "thread-generation").await;
    assert_eq!(
        store
            .thread_credential_binding(&thread_id)
            .await
            .expect("empty thread binding"),
        ConversationThreadCredentialBinding::UnboundEmpty
    );

    let first = reserve_turn(&store, project_id.clone(), thread_id.clone(), 50).await;
    let first = store
        .commit_terminal(cancelled_commit(&first.snapshot))
        .await
        .expect("finish first bound turn");
    assert_eq!(
        store
            .thread_credential_binding(&thread_id)
            .await
            .expect("claimed thread binding"),
        ConversationThreadCredentialBinding::Bound(TEST_CREDENTIAL_BINDING.into())
    );
    let replacement = original_candidate(
        project_id.clone(),
        thread_id.clone(),
        51,
        "replacement-local-generation",
    );
    assert!(matches!(
        store
            .reserve_turn(
                replacement.0,
                replacement.1,
                ConversationTurnReservationSource::CurrentThread,
                replacement.2,
                replacement.3,
                replacement.4,
                ConversationTurnEventKind::Created,
            )
            .await,
        Err(StoreError::Conflict)
    ));
    drop(store);

    let connection = raw_connection(&path, [98; 32]);
    let binding: Option<String> = connection
        .query_row(
            "SELECT credential_binding_id FROM conversation_thread_identity
             WHERE thread_id=?1",
            [thread_id.as_str()],
            |row| row.get(0),
        )
        .expect("persisted thread binding");
    assert_eq!(binding.as_deref(), Some(TEST_CREDENTIAL_BINDING));
    assert!(
        connection
            .execute(
                "UPDATE conversation_thread_identity
                 SET credential_binding_id='forged-replacement'
                 WHERE thread_id=?1",
                [thread_id.as_str()],
            )
            .is_err()
    );
    drop(connection);

    let reopened = open(&path, key).await;
    assert_eq!(
        reopened
            .load_turn(&first.turn.id)
            .await
            .expect("restart first turn"),
        Some(first)
    );
    let matching = original_candidate(project_id, thread_id.clone(), 52, TEST_CREDENTIAL_BINDING);
    assert!(
        reopened
            .reserve_turn(
                matching.0,
                matching.1,
                ConversationTurnReservationSource::CurrentThread,
                matching.2,
                matching.3,
                matching.4,
                ConversationTurnEventKind::Created,
            )
            .await
            .expect("matching generation after restart")
            .created
    );
}

#[tokio::test]
async fn lineage_seal_failure_rolls_back_context_and_the_whole_reservation() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("thread-binding-rollback.db");
    let key = Arc::new(EphemeralKeyProvider::new([102; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "thread-binding-rollback").await;
    drop(store);

    let connection = raw_connection(&path, [102; 32]);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_conversation_lineage_seal
             BEFORE INSERT ON conversation_turn_lineage BEGIN
                 SELECT RAISE(ABORT, 'injected lineage seal failure');
             END;",
        )
        .expect("install reservation fault");
    drop(connection);

    let reopened = open(&path, key).await;
    let candidate = original_candidate(project_id, thread_id.clone(), 56, TEST_CREDENTIAL_BINDING);
    assert!(matches!(
        reopened
            .reserve_turn(
                candidate.0,
                candidate.1,
                ConversationTurnReservationSource::CurrentThread,
                candidate.2,
                candidate.3,
                candidate.4,
                ConversationTurnEventKind::Created,
            )
            .await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(
        reopened
            .thread_credential_binding(&thread_id)
            .await
            .expect("binding after rolled-back reservation"),
        ConversationThreadCredentialBinding::UnboundEmpty
    );
    assert!(
        reopened
            .list_thread_turns(&thread_id, None, 10)
            .await
            .expect("turns after rolled-back reservation")
            .is_empty()
    );
    drop(reopened);

    let connection = raw_connection(&path, [102; 32]);
    let partial_rows: (u32, u32, u32, u32, u32, u32) = connection
        .query_row(
            "SELECT
                 (SELECT count(*) FROM messages),
                 (SELECT count(*) FROM runs),
                 (SELECT count(*) FROM conversation_turns),
                 (SELECT count(*) FROM conversation_turn_context),
                 (SELECT count(*) FROM conversation_turn_lineage),
                 (SELECT count(*) FROM conversation_turn_events)",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .expect("partial reservation row counts");
    assert_eq!(partial_rows, (0, 0, 0, 0, 0, 0));
}

#[tokio::test]
async fn forged_thread_generation_fails_closed_on_snapshot_load() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("forged-thread-generation.db");
    let key = Arc::new(EphemeralKeyProvider::new([99; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "forged-thread-generation").await;
    let reserved = reserve_turn(&store, project_id, thread_id.clone(), 53).await;
    drop(store);

    let connection = raw_connection(&path, [99; 32]);
    connection
        .execute_batch(
            "DROP TRIGGER conversation_thread_identity_bind_once;
             UPDATE conversation_thread_identity
             SET credential_binding_id='forged-local-generation';",
        )
        .expect("forge thread generation");
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened.load_turn(&reserved.snapshot.turn.id).await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
    assert!(matches!(
        reopened.list_thread_turns(&thread_id, None, 10).await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
}

#[tokio::test]
async fn thread_binding_lookup_distinguishes_missing_thread_from_missing_identity() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("missing-thread-identity.db");
    let key = Arc::new(EphemeralKeyProvider::new([101; 32]));
    let store = open(&path, key.clone()).await;
    let (_, thread_id) = seed_workspace(&store, "missing-thread-identity").await;
    let missing = ThreadId::new("missing-thread").expect("missing thread ID");
    assert!(matches!(
        store.thread_credential_binding(&missing).await,
        Err(StoreError::NotFound)
    ));
    drop(store);

    let connection = raw_connection(&path, [101; 32]);
    connection
        .execute_batch(
            "DROP TRIGGER conversation_thread_identity_immutable_delete;
             DELETE FROM conversation_thread_identity;",
        )
        .expect("remove required thread identity");
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened.thread_credential_binding(&thread_id).await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
}

#[tokio::test]
async fn legacy_unbound_thread_replays_reads_but_rejects_new_turns() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("legacy-unbound-thread.db");
    let key = Arc::new(EphemeralKeyProvider::new([100; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "legacy-unbound").await;
    let candidate = original_candidate(
        project_id.clone(),
        thread_id.clone(),
        54,
        TEST_CREDENTIAL_BINDING,
    );
    let reserved = store
        .reserve_turn(
            candidate.0.clone(),
            candidate.1,
            ConversationTurnReservationSource::CurrentThread,
            candidate.2.clone(),
            candidate.3.clone(),
            candidate.4.clone(),
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("reserve pre-legacy turn");
    drop(store);

    let connection = raw_connection(&path, [100; 32]);
    connection
        .execute_batch(
            "DROP TRIGGER conversation_thread_identity_bind_once;
             DROP TRIGGER conversation_turn_lineage_immutable_update;
             UPDATE conversation_thread_identity SET credential_binding_id=NULL;
             UPDATE conversation_turn_lineage SET credential_binding_id=NULL;",
        )
        .expect("simulate migrated legacy thread");
    drop(connection);

    let reopened = open(&path, key).await;
    let legacy = reopened
        .load_turn(&reserved.snapshot.turn.id)
        .await
        .expect("load legacy turn")
        .expect("legacy turn");
    assert_eq!(legacy.lineage.credential_binding_id, None);
    assert_eq!(
        reopened
            .thread_credential_binding(&thread_id)
            .await
            .expect("legacy thread binding"),
        ConversationThreadCredentialBinding::LegacyUnbound
    );
    let replay = reopened
        .reserve_turn(
            candidate.0,
            legacy.lineage,
            ConversationTurnReservationSource::CurrentThread,
            candidate.2,
            candidate.3,
            candidate.4,
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("exact legacy command replay");
    assert!(!replay.created);

    let next = original_candidate(project_id, thread_id.clone(), 55, TEST_CREDENTIAL_BINDING);
    assert!(matches!(
        reopened
            .reserve_turn(
                next.0,
                next.1,
                ConversationTurnReservationSource::CurrentThread,
                next.2,
                next.3,
                next.4,
                ConversationTurnEventKind::Created,
            )
            .await,
        Err(StoreError::Conflict)
    ));
    assert_eq!(
        reopened
            .list_thread_turns(&thread_id, None, 10)
            .await
            .expect("legacy history")
            .len(),
        1
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn conversation_forks_persist_replay_and_restore_nested_lineage() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("conversation-forks.db");
    let key = Arc::new(EphemeralKeyProvider::new([103; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "forks").await;
    let reserved = reserve_turn(&store, project_id, thread_id.clone(), 60).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 60))
        .await
        .expect("source provider start");
    append_completed_text(&store, &started, 60).await;
    let source = store
        .commit_terminal(completed_commit(&started, 60))
        .await
        .expect("source completion");
    let parent = store.get_thread(&thread_id).await.expect("source thread");

    let branch = fork_plan(&store, &source, &parent, ConversationForkKind::Branch, 61).await;
    let branch_command = branch.command.clone();
    let created = store
        .reserve_conversation_fork(branch.clone())
        .await
        .expect("persist branch");
    assert!(created.created);
    assert!(created.context.is_none());
    assert!(created.snapshot.started_turn.is_none());
    assert_eq!(created.snapshot.messages.len(), 2);
    assert_eq!(
        store
            .get_thread(&created.snapshot.child_thread.id)
            .await
            .expect("generic child thread load"),
        created.snapshot.child_thread
    );
    assert_eq!(
        store
            .list_messages(&created.snapshot.child_thread.id, None, 10)
            .await
            .expect("generic derived message load"),
        created.snapshot.messages
    );
    let replay = store
        .reserve_conversation_fork(branch)
        .await
        .expect("exact branch replay");
    assert!(!replay.created);
    assert_eq!(replay.snapshot, created.snapshot);

    let edit = fork_plan(
        &store,
        &source,
        &parent,
        ConversationForkKind::EditAndBranch,
        62,
    )
    .await;
    let edit_command = edit.command.clone();
    let edited = store
        .reserve_conversation_fork(edit)
        .await
        .expect("persist edit-and-branch");
    assert!(edited.created);
    assert_eq!(edited.context.as_ref().map(Vec::len), Some(1));
    assert!(matches!(
        edited
            .snapshot
            .started_turn
            .as_ref()
            .map(|snapshot| &snapshot.lineage.origin),
        Some(ConversationTurnOrigin::EditAndBranch { source_turn_id })
            if source_turn_id == &source.turn.id
    ));

    let regenerate = fork_plan(
        &store,
        &source,
        &parent,
        ConversationForkKind::Regenerate,
        63,
    )
    .await;
    let regenerate_command = regenerate.command.clone();
    let regenerated = store
        .reserve_conversation_fork(regenerate)
        .await
        .expect("persist regenerate");
    let regenerated_turn = regenerated
        .snapshot
        .started_turn
        .clone()
        .expect("regenerate turn");
    assert!(matches!(
        regenerated_turn.lineage.origin,
        ConversationTurnOrigin::Regenerate { ref source_turn_id }
            if source_turn_id == &source.turn.id
    ));
    assert_eq!(
        regenerated.context,
        Some(regenerated.snapshot.messages.clone())
    );
    let regenerated_started = store
        .commit_provider_start(provider_start_commit(&regenerated_turn, 63))
        .await
        .expect("regenerate provider start");
    append_completed_text(&store, &regenerated_started, 63).await;
    let regenerated_completed = store
        .commit_terminal(completed_commit(&regenerated_started, 63))
        .await
        .expect("regenerate completion");

    let nested_parent = regenerated.snapshot.child_thread.clone();
    let nested = fork_plan(
        &store,
        &regenerated_completed,
        &nested_parent,
        ConversationForkKind::Branch,
        64,
    )
    .await;
    let nested_command = nested.command.clone();
    let nested = store
        .reserve_conversation_fork(nested)
        .await
        .expect("persist nested branch");
    assert_eq!(nested.snapshot.child_thread.lineage.fork_depth, 2);
    assert_eq!(
        nested.snapshot.child_thread.lineage.root_thread_id,
        parent.id
    );
    let nested_metadata = store
        .load_conversation_fork_metadata(&nested.snapshot.child_thread.id)
        .await
        .expect("nested metadata");
    assert_eq!(nested_metadata.family_threads.len(), 5);
    assert_eq!(nested_metadata.inherited_assistant_outcomes.len(), 1);

    assert_eq!(
        store
            .load_turn_by_command(&edit_command)
            .await
            .expect("edit turn command"),
        edited.snapshot.started_turn
    );
    assert_eq!(
        store
            .load_turn_by_command(&regenerate_command)
            .await
            .expect("regenerate turn command")
            .expect("regenerate turn")
            .turn
            .state,
        ConversationTurnState::Completed
    );
    let mut conflicting = branch_command.clone();
    conflicting.fingerprint = [255; 32];
    assert!(matches!(
        store.load_conversation_fork_by_command(&conflicting).await,
        Err(StoreError::Conflict)
    ));
    drop(store);

    let reopened = open(&path, key).await;
    assert_eq!(
        reopened
            .load_conversation_fork_by_command(&branch_command)
            .await
            .expect("restarted branch replay"),
        Some(created.snapshot)
    );
    assert_eq!(
        reopened
            .load_conversation_fork_by_command(&nested_command)
            .await
            .expect("restarted nested replay"),
        Some(nested.snapshot)
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn pending_fork_delivery_reconciles_across_restart_and_acknowledges_exactly() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("fork-delivery-reconciliation.db");
    let key_bytes = [111; 32];
    let key = Arc::new(EphemeralKeyProvider::new(key_bytes));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "fork-delivery").await;
    let reserved = reserve_turn(&store, project_id, thread_id.clone(), 111).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 111))
        .await
        .expect("source provider start");
    append_completed_text(&store, &started, 111).await;
    let source = store
        .commit_terminal(completed_commit(&started, 111))
        .await
        .expect("source completion");
    let parent = store.get_thread(&thread_id).await.expect("source thread");

    let plan = fork_plan(&store, &source, &parent, ConversationForkKind::Branch, 112).await;
    let canonical_command = plan.command.clone();
    let created = store
        .reserve_conversation_fork(plan.clone())
        .await
        .expect("create pending fork delivery");
    assert!(created.created);
    assert!(!created.reconciled_pending_delivery);
    assert_eq!(
        created.snapshot.delivery.state,
        ConversationForkDeliveryState::Pending
    );
    assert_eq!(created.snapshot.delivery.revision, 0);
    assert_eq!(
        created.snapshot.delivery.child_thread_id,
        created.snapshot.child_thread.id
    );
    let exact = store
        .reserve_conversation_fork(plan)
        .await
        .expect("exact canonical replay");
    assert!(!exact.created);
    assert!(!exact.reconciled_pending_delivery);
    assert_eq!(exact.snapshot, created.snapshot);

    let mut first_alias = canonical_command.clone();
    first_alias.key = "security-fork-delivery-alias-first".into();
    let reconciled = store
        .resolve_conversation_fork_command(&first_alias)
        .await
        .expect("resolve first alias")
        .expect("pending fork");
    assert!(reconciled.reconciled_pending_delivery);
    assert_eq!(reconciled.snapshot, created.snapshot);
    let exact_alias = store
        .resolve_conversation_fork_command(&first_alias)
        .await
        .expect("replay first alias")
        .expect("aliased fork");
    assert!(!exact_alias.reconciled_pending_delivery);
    assert_eq!(exact_alias.snapshot, created.snapshot);

    drop(store);
    let reopened = open(&path, key.clone()).await;
    let mut restart_alias = canonical_command.clone();
    restart_alias.key = "security-fork-delivery-alias-after-restart".into();
    let restart_reconciliation = reopened
        .resolve_conversation_fork_command(&restart_alias)
        .await
        .expect("reconcile after restart")
        .expect("restart-pending fork");
    assert!(restart_reconciliation.reconciled_pending_delivery);
    assert_eq!(restart_reconciliation.snapshot, created.snapshot);

    let acknowledgement = fork_delivery_ack_command("primary", [201; 32]);
    let acknowledged = reopened
        .acknowledge_conversation_fork_delivery(
            acknowledgement.clone(),
            created.snapshot.child_thread.id.clone(),
            0,
        )
        .await
        .expect("acknowledge pending delivery");
    assert_eq!(
        acknowledged.state,
        ConversationForkDeliveryState::Acknowledged
    );
    assert_eq!(acknowledged.revision, 1);
    assert_eq!(
        reopened
            .acknowledge_conversation_fork_delivery(
                acknowledgement.clone(),
                created.snapshot.child_thread.id.clone(),
                0,
            )
            .await
            .expect("exact acknowledgement replay"),
        acknowledged
    );

    let mut conflicting_fingerprint = acknowledgement.clone();
    conflicting_fingerprint.fingerprint = [202; 32];
    assert!(matches!(
        reopened
            .acknowledge_conversation_fork_delivery(
                conflicting_fingerprint,
                created.snapshot.child_thread.id.clone(),
                0,
            )
            .await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        reopened
            .acknowledge_conversation_fork_delivery(acknowledgement, parent.id.clone(), 0,)
            .await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        reopened
            .acknowledge_conversation_fork_delivery(
                fork_delivery_ack_command("second-key", [203; 32]),
                created.snapshot.child_thread.id.clone(),
                0,
            )
            .await,
        Err(StoreError::Conflict)
    ));
    let exact_alias_after_ack = reopened
        .load_conversation_fork_by_command(&restart_alias)
        .await
        .expect("load exact alias after acknowledgement")
        .expect("durable alias");
    assert_eq!(
        exact_alias_after_ack.child_thread.id,
        created.snapshot.child_thread.id
    );
    assert_eq!(
        exact_alias_after_ack.delivery.state,
        ConversationForkDeliveryState::Acknowledged
    );
    let mut ack_first_new_key = canonical_command.clone();
    ack_first_new_key.key = "security-fork-delivery-key-after-ack".into();
    assert_eq!(
        reopened
            .resolve_conversation_fork_command(&ack_first_new_key)
            .await
            .expect("ack-first resolution"),
        None
    );

    let mut later_plan = fork_plan(
        &reopened,
        &source,
        &parent,
        ConversationForkKind::Branch,
        113,
    )
    .await;
    let later_key = later_plan.command.key.clone();
    later_plan.command.key = first_alias.key.clone();
    assert!(matches!(
        reopened.reserve_conversation_fork(later_plan.clone()).await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        reopened.get_thread(&later_plan.child_thread.id).await,
        Err(StoreError::NotFound)
    ));
    later_plan.command.key = later_key;
    later_plan.command.fingerprint = canonical_command.fingerprint;
    let later = reopened
        .reserve_conversation_fork(later_plan)
        .await
        .expect("acknowledged fingerprint may create a new fork");
    assert!(later.created);
    assert_ne!(
        later.snapshot.child_thread.id,
        created.snapshot.child_thread.id
    );
    assert_eq!(
        later.snapshot.delivery.state,
        ConversationForkDeliveryState::Pending
    );
    drop(reopened);

    let connection = raw_connection(&path, key_bytes);
    let delivery_counts: (u32, u32) = connection
        .query_row(
            "SELECT sum(state=0),sum(state=1) FROM conversation_fork_deliveries
             WHERE command_scope=?1 AND request_fingerprint=?2",
            rusqlite::params![
                canonical_command.scope,
                canonical_command.fingerprint.as_slice()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("partial pending delivery index projection");
    assert_eq!(delivery_counts, (1, 1));
    let acknowledgement_rows: u32 = connection
        .query_row(
            "SELECT count(*) FROM conversation_fork_delivery_ack_commands",
            [],
            |row| row.get(0),
        )
        .expect("bounded acknowledgement commands");
    assert_eq!(acknowledgement_rows, 1);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn fork_delivery_aliases_are_bounded_without_creating_an_extra_child() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("fork-delivery-alias-bound.db");
    let key_bytes = [114; 32];
    let key = Arc::new(EphemeralKeyProvider::new(key_bytes));
    let store = open(&path, key).await;
    let (project_id, thread_id) = seed_workspace(&store, "fork-alias-bound").await;
    let reserved = reserve_turn(&store, project_id, thread_id.clone(), 114).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 114))
        .await
        .expect("source provider start");
    append_completed_text(&store, &started, 114).await;
    let source = store
        .commit_terminal(completed_commit(&started, 114))
        .await
        .expect("source completion");
    let parent = store.get_thread(&thread_id).await.expect("source thread");
    let plan = fork_plan(&store, &source, &parent, ConversationForkKind::Branch, 115).await;
    let command = plan.command.clone();
    let created = store
        .reserve_conversation_fork(plan)
        .await
        .expect("pending fork");

    for index in 0..MAX_CONVERSATION_FORK_DELIVERY_ALIASES {
        let mut alias = command.clone();
        alias.key = format!("security-fork-delivery-bounded-alias-{index}");
        let resolution = store
            .resolve_conversation_fork_command(&alias)
            .await
            .expect("bounded alias")
            .expect("pending resolution");
        assert!(resolution.reconciled_pending_delivery);
        assert_eq!(
            resolution.snapshot.child_thread.id,
            created.snapshot.child_thread.id
        );
    }
    let mut exhausted = command;
    exhausted.key = "security-fork-delivery-bounded-alias-exhausted".into();
    assert!(matches!(
        store.resolve_conversation_fork_command(&exhausted).await,
        Err(StoreError::Conflict)
    ));
    let acknowledged = store
        .acknowledge_conversation_fork_delivery(
            fork_delivery_ack_command("after-alias-bound", [204; 32]),
            created.snapshot.child_thread.id.clone(),
            0,
        )
        .await
        .expect("alias bound does not block acknowledgement");
    assert_eq!(
        acknowledged.state,
        ConversationForkDeliveryState::Acknowledged
    );
    let mut first_alias = exhausted;
    first_alias.key = "security-fork-delivery-bounded-alias-0".into();
    let exact_after_ack = store
        .load_conversation_fork_by_command(&first_alias)
        .await
        .expect("exact bounded alias after acknowledgement")
        .expect("persisted alias");
    assert_eq!(
        exact_after_ack.delivery.state,
        ConversationForkDeliveryState::Acknowledged
    );
    drop(store);

    let connection = raw_connection(&path, key_bytes);
    let aliases: usize = connection
        .query_row(
            "SELECT count(*) FROM conversation_fork_delivery_aliases",
            [],
            |row| row.get::<_, i64>(0),
        )
        .expect("alias count")
        .try_into()
        .expect("bounded count");
    assert_eq!(aliases, MAX_CONVERSATION_FORK_DELIVERY_ALIASES);
    let child_deliveries: u32 = connection
        .query_row(
            "SELECT count(*) FROM conversation_fork_deliveries",
            [],
            |row| row.get(0),
        )
        .expect("delivery count");
    assert_eq!(child_deliveries, 1);
    let reverse_collision = connection
        .execute(
            "INSERT INTO conversation_fork_commands(
                 command_scope,idempotency_key,request_fingerprint,source_turn_id,
                 expected_source_revision,child_thread_id,started_turn_id
             )
             SELECT command_scope,?1,request_fingerprint,source_turn_id,
                    expected_source_revision,child_thread_id,started_turn_id
             FROM conversation_fork_commands LIMIT 1",
            [first_alias.key.as_str()],
        )
        .expect_err("canonical command cannot reuse a durable alias key");
    assert!(
        reverse_collision
            .to_string()
            .contains("conversation fork command key collides with a delivery alias")
    );
    connection
        .execute_batch("DROP TRIGGER conversation_fork_delivery_aliases_validate_insert;")
        .expect("disable alias bound for corruption fixture");
    connection
        .execute(
            "INSERT INTO conversation_fork_delivery_aliases(
                 command_scope,idempotency_key,request_fingerprint,child_thread_id
             ) VALUES (?1,?2,?3,?4)",
            rusqlite::params![
                first_alias.scope,
                "security-fork-delivery-bounded-alias-corrupt",
                first_alias.fingerprint.as_slice(),
                created.snapshot.child_thread.id.as_str(),
            ],
        )
        .expect("inject alias beyond bound");
    drop(connection);

    let corrupted = open(&path, Arc::new(EphemeralKeyProvider::new(key_bytes))).await;
    assert!(matches!(
        corrupted
            .load_conversation_fork_by_command(&first_alias)
            .await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
}

#[tokio::test]
async fn fork_delivery_acknowledgement_failure_rolls_back_the_state_transition() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("fork-delivery-ack-rollback.db");
    let key_bytes = [116; 32];
    let key = Arc::new(EphemeralKeyProvider::new(key_bytes));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "fork-ack-rollback").await;
    let reserved = reserve_turn(&store, project_id, thread_id.clone(), 116).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 116))
        .await
        .expect("source provider start");
    append_completed_text(&store, &started, 116).await;
    let source = store
        .commit_terminal(completed_commit(&started, 116))
        .await
        .expect("source completion");
    let parent = store.get_thread(&thread_id).await.expect("source thread");
    let created = store
        .reserve_conversation_fork(
            fork_plan(&store, &source, &parent, ConversationForkKind::Branch, 117).await,
        )
        .await
        .expect("pending fork");
    let child_thread_id = created.snapshot.child_thread.id;
    drop(store);

    let connection = raw_connection(&path, key_bytes);
    connection
        .execute_batch(
            "CREATE TRIGGER test_reject_fork_delivery_ack
             BEFORE INSERT ON conversation_fork_delivery_ack_commands BEGIN
                 SELECT RAISE(ABORT, 'injected acknowledgement journal failure');
             END;",
        )
        .expect("install acknowledgement fault");
    drop(connection);

    let reopened = open(&path, key.clone()).await;
    let command = fork_delivery_ack_command("rollback", [205; 32]);
    assert!(matches!(
        reopened
            .acknowledge_conversation_fork_delivery(command.clone(), child_thread_id.clone(), 0,)
            .await,
        Err(StoreError::Conflict)
    ));
    drop(reopened);

    let connection = raw_connection(&path, key_bytes);
    let state: (i64, i64, u32) = connection
        .query_row(
            "SELECT delivery.state,delivery.revision,
                    (SELECT count(*) FROM conversation_fork_delivery_ack_commands)
             FROM conversation_fork_deliveries delivery
             WHERE delivery.child_thread_id=?1",
            [child_thread_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .expect("rolled-back acknowledgement state");
    assert_eq!(state, (0, 0, 0));
    connection
        .execute_batch("DROP TRIGGER test_reject_fork_delivery_ack;")
        .expect("remove acknowledgement fault");
    drop(connection);

    let restarted = open(&path, key).await;
    let acknowledged = restarted
        .acknowledge_conversation_fork_delivery(command, child_thread_id, 0)
        .await
        .expect("acknowledge after rollback");
    assert_eq!(
        acknowledged.state,
        ConversationForkDeliveryState::Acknowledged
    );
    assert_eq!(acknowledged.revision, 1);
}

#[tokio::test]
async fn corrupt_fork_delivery_alias_and_canonical_correlations_fail_closed() {
    for corruption in ["alias", "delivery"] {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory
            .path()
            .join(format!("fork-delivery-corrupt-{corruption}.db"));
        let key_bytes = [118; 32];
        let key = Arc::new(EphemeralKeyProvider::new(key_bytes));
        let store = open(&path, key.clone()).await;
        let (project_id, thread_id) = seed_workspace(&store, corruption).await;
        let reserved = reserve_turn(&store, project_id, thread_id.clone(), 118).await;
        let started = store
            .commit_provider_start(provider_start_commit(&reserved.snapshot, 118))
            .await
            .expect("source provider start");
        append_completed_text(&store, &started, 118).await;
        let source = store
            .commit_terminal(completed_commit(&started, 118))
            .await
            .expect("source completion");
        let parent = store.get_thread(&thread_id).await.expect("source thread");
        let plan = fork_plan(&store, &source, &parent, ConversationForkKind::Branch, 119).await;
        let canonical = plan.command.clone();
        store
            .reserve_conversation_fork(plan)
            .await
            .expect("pending fork");
        let mut alias = canonical.clone();
        alias.key = "security-corrupt-fork-delivery-alias".into();
        store
            .resolve_conversation_fork_command(&alias)
            .await
            .expect("alias resolution")
            .expect("pending fork");
        drop(store);

        let connection = raw_connection(&path, key_bytes);
        match corruption {
            "alias" => connection
                .execute_batch(
                    "DROP TRIGGER conversation_fork_delivery_aliases_immutable_update;
                     UPDATE conversation_fork_delivery_aliases
                     SET request_fingerprint=zeroblob(32);",
                )
                .expect("corrupt alias fingerprint"),
            "delivery" => connection
                .execute_batch(
                    "DROP TRIGGER conversation_fork_deliveries_validate_update;
                     UPDATE conversation_fork_deliveries
                     SET request_fingerprint=zeroblob(32);",
                )
                .expect("corrupt delivery fingerprint"),
            _ => unreachable!(),
        }
        drop(connection);

        let reopened = open(&path, key).await;
        assert!(matches!(
            reopened
                .load_conversation_fork_by_command(&canonical)
                .await,
            Err(StoreError::Internal(message))
                if message == "invalid persisted conversation aggregate"
        ));
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn schema_fourteen_forks_migrate_acknowledged_and_restart_after_a_fault() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("fork-delivery-schema-fourteen.db");
    let key_bytes = [120; 32];
    let key = Arc::new(EphemeralKeyProvider::new(key_bytes));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "fork-schema-fourteen").await;
    let reserved = reserve_turn(&store, project_id, thread_id.clone(), 120).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 120))
        .await
        .expect("source provider start");
    append_completed_text(&store, &started, 120).await;
    let source = store
        .commit_terminal(completed_commit(&started, 120))
        .await
        .expect("source completion");
    let parent = store.get_thread(&thread_id).await.expect("source thread");

    let first_plan = fork_plan(&store, &source, &parent, ConversationForkKind::Branch, 121).await;
    let shared_fingerprint = first_plan.command.fingerprint;
    let first_command = first_plan.command.clone();
    let first = store
        .reserve_conversation_fork(first_plan)
        .await
        .expect("first legacy fork");
    store
        .acknowledge_conversation_fork_delivery(
            fork_delivery_ack_command("legacy-first", [206; 32]),
            first.snapshot.child_thread.id.clone(),
            0,
        )
        .await
        .expect("release first fingerprint");

    let mut second_plan =
        fork_plan(&store, &source, &parent, ConversationForkKind::Branch, 122).await;
    second_plan.command.fingerprint = shared_fingerprint;
    let second_command = second_plan.command.clone();
    let second = store
        .reserve_conversation_fork(second_plan)
        .await
        .expect("second same-fingerprint legacy fork");
    drop(store);

    let connection = raw_connection(&path, key_bytes);
    connection
        .execute_batch(
            "DROP TRIGGER conversation_fork_commands_create_pending_delivery;
             DROP TRIGGER conversation_fork_commands_reject_delivery_alias_key;
             DROP TABLE conversation_fork_delivery_ack_commands;
             DROP TABLE conversation_fork_delivery_aliases;
             DROP TABLE conversation_fork_deliveries;
             DROP TRIGGER managed_integration_journal_no_delete;
             DROP TRIGGER managed_integration_journal_immutable;
             DROP TRIGGER managed_integration_lifecycle_no_delete;
             DROP TABLE managed_integration_lifecycle_journal;
             DROP TABLE managed_integration_lifecycles;
             DROP TRIGGER automation_occurrence_prompt_immutable_delete;
             DROP TRIGGER automation_occurrence_prompt_immutable_update;
             DROP TRIGGER automation_occurrence_dispatches_immutable_delete;
             DROP TRIGGER automation_occurrence_dispatches_immutable_update;
             DROP TRIGGER automation_occurrence_dispatches_validate_insert;
             DROP TABLE automation_occurrence_dispatches;
             DROP TRIGGER automations_require_scheduler_rebase;
             DROP TRIGGER automation_occurrence_claim_attempts_immutable_delete;
             DROP TRIGGER automation_occurrence_claim_attempts_validate_update;
             DROP TRIGGER automation_occurrence_claim_attempts_validate_insert;
             DROP TRIGGER automation_occurrences_immutable_delete;
             DROP TRIGGER automation_occurrences_validate_update;
             DROP TRIGGER automation_occurrences_validate_insert;
             DROP TRIGGER automation_schedule_cursors_immutable_delete;
             DROP TRIGGER automation_schedule_cursors_validate_update;
             DROP TRIGGER automation_schedule_evaluation_commands_immutable_delete;
             DROP TRIGGER automation_schedule_evaluation_commands_immutable_update;
             DROP TRIGGER automation_schedule_evaluation_commands_validate_insert;
             DROP TRIGGER automation_schedule_cursors_validate_insert;
             DROP TRIGGER automation_scheduler_lease_immutable_delete;
             DROP TRIGGER automation_scheduler_lease_validate_update;
             DROP TRIGGER automation_scheduler_lease_validate_insert;
             DROP TRIGGER automation_history_immutable_delete;
             DROP TRIGGER automation_history_immutable_update;
             DROP TRIGGER automation_history_validate_insert;
             DROP TABLE automation_occurrence_claim_attempts;
             DROP TABLE automation_occurrences;
             DROP TABLE automation_schedule_evaluation_commands;
             DROP TABLE automation_schedule_cursors;
             DROP TABLE automation_scheduler_lease;
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
             DELETE FROM schema_migrations WHERE version IN (15,16,17,18,19,20,21,22);
             PRAGMA user_version=14;
             CREATE TABLE conversation_fork_delivery_ack_commands(blocker INTEGER) STRICT;",
        )
        .expect("construct schema fourteen migration fixture and blocker");
    drop(connection);

    assert!(SqlCipherStore::open(&path, key.clone()).await.is_err());
    let connection = raw_connection(&path, key_bytes);
    let version: u32 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .expect("rolled-back schema version");
    assert_eq!(version, 14);
    let migration_rows: u32 = connection
        .query_row(
            "SELECT count(*) FROM schema_migrations WHERE version=15",
            [],
            |row| row.get(0),
        )
        .expect("rolled-back migration history");
    assert_eq!(migration_rows, 0);
    let later_migration_rows: u32 = connection
        .query_row(
            "SELECT count(*) FROM schema_migrations WHERE version=16",
            [],
            |row| row.get(0),
        )
        .expect("rolled-back later migration history");
    assert_eq!(later_migration_rows, 0);
    let partial_objects: u32 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master
             WHERE name IN (
                 'conversation_fork_deliveries',
                 'conversation_fork_delivery_aliases',
                 'conversation_fork_deliveries_one_pending_request'
             )",
            [],
            |row| row.get(0),
        )
        .expect("rolled-back schema objects");
    assert_eq!(partial_objects, 0);
    connection
        .execute_batch("DROP TABLE conversation_fork_delivery_ack_commands;")
        .expect("remove schema migration blocker");
    drop(connection);

    let migrated = open(&path, key).await;
    for (command, expected_child) in [
        (&first_command, &first.snapshot.child_thread.id),
        (&second_command, &second.snapshot.child_thread.id),
    ] {
        let snapshot = migrated
            .load_conversation_fork_by_command(command)
            .await
            .expect("load migrated legacy fork")
            .expect("legacy command");
        assert_eq!(&snapshot.child_thread.id, expected_child);
        assert_eq!(
            snapshot.delivery.state,
            ConversationForkDeliveryState::Acknowledged
        );
        assert_eq!(snapshot.delivery.revision, 1);
    }
    let mut fresh_key = first_command.clone();
    fresh_key.key = "security-fork-schema-fourteen-fresh-key".into();
    assert_eq!(
        migrated
            .resolve_conversation_fork_command(&fresh_key)
            .await
            .expect("legacy deliveries are not pending"),
        None
    );
    assert!(matches!(
        migrated
            .acknowledge_conversation_fork_delivery(
                fork_delivery_ack_command("legacy-after-migration", [207; 32]),
                first.snapshot.child_thread.id.clone(),
                0,
            )
            .await,
        Err(StoreError::Conflict)
    ));

    let mut fresh_plan = fork_plan(
        &migrated,
        &source,
        &parent,
        ConversationForkKind::Branch,
        123,
    )
    .await;
    fresh_plan.command.fingerprint = shared_fingerprint;
    let fresh = migrated
        .reserve_conversation_fork(fresh_plan)
        .await
        .expect("new pending fork after legacy acknowledgement backfill");
    assert!(fresh.created);
    assert_eq!(
        fresh.snapshot.delivery.state,
        ConversationForkDeliveryState::Pending
    );
    drop(migrated);

    let connection = raw_connection(&path, key_bytes);
    let legacy_deliveries: Vec<(i64, i64)> = {
        let mut statement = connection
            .prepare(
                "SELECT state,revision FROM conversation_fork_deliveries
                 WHERE child_thread_id IN (?1,?2) ORDER BY child_thread_id",
            )
            .expect("legacy delivery statement");
        statement
            .query_map(
                rusqlite::params![
                    first.snapshot.child_thread.id.as_str(),
                    second.snapshot.child_thread.id.as_str()
                ],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("legacy delivery rows")
            .collect::<Result<Vec<_>, _>>()
            .expect("legacy delivery projection")
    };
    assert_eq!(legacy_deliveries, vec![(1, 1), (1, 1)]);
    let aliases_and_acknowledgements: (u32, u32) = connection
        .query_row(
            "SELECT
                 (SELECT count(*) FROM conversation_fork_delivery_aliases),
                 (SELECT count(*) FROM conversation_fork_delivery_ack_commands)",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("legacy delivery journals");
    assert_eq!(aliases_and_acknowledgements, (0, 0));
    connection
        .execute(
            "DROP INDEX conversation_fork_deliveries_one_pending_request",
            [],
        )
        .expect("disable pending uniqueness for corruption fixture");
    connection
        .execute_batch("DROP TRIGGER conversation_fork_deliveries_validate_update;")
        .expect("disable delivery transition validation for corruption fixture");
    connection
        .execute(
            "UPDATE conversation_fork_deliveries SET state=0,revision=0
             WHERE child_thread_id=?1",
            [first.snapshot.child_thread.id.as_str()],
        )
        .expect("inject a second matching pending delivery");
    drop(connection);

    let corrupted = open(&path, Arc::new(EphemeralKeyProvider::new(key_bytes))).await;
    let mut ambiguous = first_command;
    ambiguous.key = "security-fork-schema-fourteen-ambiguous-pending".into();
    assert!(matches!(
        corrupted.resolve_conversation_fork_command(&ambiguous).await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
}

#[tokio::test]
async fn completed_retry_with_gapped_message_sequences_can_be_forked() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory.path().join("retry-fork-context-ordinal.db"),
        Arc::new(EphemeralKeyProvider::new([105; 32])),
    )
    .await;
    let (project_id, thread_id) = seed_workspace(&store, "retry-fork-ordinal").await;

    let first = reserve_turn(&store, project_id.clone(), thread_id.clone(), 67).await;
    let first = store
        .commit_provider_start(provider_start_commit(&first.snapshot, 67))
        .await
        .expect("first provider start");
    append_completed_text(&store, &first, 67).await;
    store
        .commit_terminal(completed_commit(&first, 67))
        .await
        .expect("first completion");

    let failed_attempt = reserve_turn(&store, project_id, thread_id.clone(), 68).await;
    let failed_attempt = store
        .commit_terminal(cancelled_commit(&failed_attempt.snapshot))
        .await
        .expect("cancel source attempt");
    let retry = retry_candidate(&failed_attempt, 69);
    let retry = store
        .reserve_turn(
            retry.0,
            retry.1,
            ConversationTurnReservationSource::Retry {
                source_turn_id: failed_attempt.turn.id.clone(),
                expected_source_revision: failed_attempt.turn.revision,
            },
            retry.2,
            retry.3,
            retry.4,
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("reserve retry");
    assert_eq!(
        retry
            .context
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 4]
    );
    let retry = store
        .commit_provider_start(provider_start_commit(&retry.snapshot, 69))
        .await
        .expect("retry provider start");
    append_completed_text(&store, &retry, 69).await;
    let retry = store
        .commit_terminal(completed_commit(&retry, 69))
        .await
        .expect("retry completion");
    let parent = store.get_thread(&thread_id).await.expect("parent");

    let branch = store
        .reserve_conversation_fork(
            fork_plan(&store, &retry, &parent, ConversationForkKind::Branch, 70).await,
        )
        .await
        .expect("branch completed retry");
    assert_eq!(
        branch
            .snapshot
            .messages
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert!(matches!(
        &branch.snapshot.messages[2].derivation,
        grok_domain::ConversationMessageDerivation::Fork {
            source_context_sequence: Some(3),
            ..
        }
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn failed_retry_with_leading_sequence_gap_can_be_edited_into_branch() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory
            .path()
            .join("failed-retry-edit-context-ordinal.db"),
        Arc::new(EphemeralKeyProvider::new([109; 32])),
    )
    .await;
    let (project_id, thread_id) = seed_workspace(&store, "failed-retry-edit-ordinal").await;

    let first = reserve_turn(&store, project_id.clone(), thread_id.clone(), 86).await;
    let first = store
        .commit_terminal(cancelled_commit(&first.snapshot))
        .await
        .expect("cancel first attempt");
    let retry = retry_candidate(&first, 87);
    let retry = store
        .reserve_turn(
            retry.0,
            retry.1,
            ConversationTurnReservationSource::Retry {
                source_turn_id: first.turn.id.clone(),
                expected_source_revision: first.turn.revision,
            },
            retry.2,
            retry.3,
            retry.4,
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("reserve first retry");
    let retry = store
        .commit_provider_start(provider_start_commit(&retry.snapshot, 87))
        .await
        .expect("first retry provider start");
    append_completed_text(&store, &retry, 87).await;
    store
        .commit_terminal(completed_commit(&retry, 87))
        .await
        .expect("first retry completion");

    let second = reserve_turn(&store, project_id, thread_id.clone(), 88).await;
    let second = store
        .commit_terminal(cancelled_commit(&second.snapshot))
        .await
        .expect("cancel second attempt");
    let retry = retry_candidate(&second, 89);
    let retry = store
        .reserve_turn(
            retry.0,
            retry.1,
            ConversationTurnReservationSource::Retry {
                source_turn_id: second.turn.id.clone(),
                expected_source_revision: second.turn.revision,
            },
            retry.2,
            retry.3,
            retry.4,
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("reserve failed retry");
    assert_eq!(
        retry
            .context
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3, 5]
    );
    let retry = store
        .commit_provider_start(provider_start_commit(&retry.snapshot, 89))
        .await
        .expect("failed retry provider start");
    let retry = store
        .commit_terminal(failed_commit(&retry))
        .await
        .expect("failed retry terminal state");
    let parent = store.get_thread(&thread_id).await.expect("parent");

    let edited = store
        .reserve_conversation_fork(
            fork_plan(
                &store,
                &retry,
                &parent,
                ConversationForkKind::EditAndBranch,
                90,
            )
            .await,
        )
        .await
        .expect("edit failed retry");
    assert_eq!(
        edited
            .snapshot
            .messages
            .iter()
            .map(|message| message.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert!(matches!(
        &edited.snapshot.messages[0].derivation,
        grok_domain::ConversationMessageDerivation::Fork {
            source_context_sequence: Some(1),
            ..
        }
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn nested_inherited_outcomes_resolve_iteratively_and_reject_same_content_reattribution() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("nested-inherited-provenance.db");
    let key = Arc::new(EphemeralKeyProvider::new([106; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, root_thread_id) = seed_workspace(&store, "nested-provenance").await;

    let first = reserve_turn(&store, project_id.clone(), root_thread_id.clone(), 71).await;
    let first = store
        .commit_provider_start(provider_start_commit(&first.snapshot, 71))
        .await
        .expect("first provider start");
    append_text(&store, &first, "identical answer").await;
    let first = store
        .commit_terminal(completed_commit_with_text(
            &first,
            71,
            "identical answer".into(),
        ))
        .await
        .expect("first completion");

    let second = reserve_turn(&store, project_id.clone(), root_thread_id.clone(), 72).await;
    let second = store
        .commit_provider_start(provider_start_commit(&second.snapshot, 72))
        .await
        .expect("second provider start");
    append_text(&store, &second, "identical answer").await;
    let second = store
        .commit_terminal(completed_commit_with_text(
            &second,
            72,
            "identical answer".into(),
        ))
        .await
        .expect("second completion");
    let root = store
        .get_thread(&root_thread_id)
        .await
        .expect("root thread");
    let branch = store
        .reserve_conversation_fork(
            fork_plan(&store, &second, &root, ConversationForkKind::Branch, 73).await,
        )
        .await
        .expect("first branch");

    let first_copy_id = branch.snapshot.messages[1].id.clone();
    let connection = raw_connection(&path, [106; 32]);
    connection
        .execute_batch("DROP TRIGGER conversation_inherited_assistant_outcomes_immutable_delete;")
        .expect("enable trigger provenance fixture");
    connection
        .execute(
            "DELETE FROM conversation_inherited_assistant_outcomes
             WHERE child_assistant_message_id=?1",
            [first_copy_id.as_str()],
        )
        .expect("remove copied outcome for trigger fixture");
    assert!(
        connection
            .execute(
                "INSERT INTO conversation_inherited_assistant_outcomes(
                     child_assistant_message_id,source_turn_id
                 ) VALUES (?1,?2)",
                [first_copy_id.as_str(), second.turn.id.as_str()],
            )
            .is_err(),
        "same-content unrelated source turn passed the insert trigger"
    );
    connection
        .execute(
            "INSERT INTO conversation_inherited_assistant_outcomes(
                 child_assistant_message_id,source_turn_id
             ) VALUES (?1,?2)",
            [first_copy_id.as_str(), first.turn.id.as_str()],
        )
        .expect("restore exact inherited outcome");
    drop(connection);

    let child_turn = reserve_turn(
        &store,
        project_id,
        branch.snapshot.child_thread.id.clone(),
        74,
    )
    .await;
    let child_turn = store
        .commit_provider_start(provider_start_commit(&child_turn.snapshot, 74))
        .await
        .expect("child provider start");
    append_completed_text(&store, &child_turn, 74).await;
    let child_turn = store
        .commit_terminal(completed_commit(&child_turn, 74))
        .await
        .expect("child completion");
    let nested_plan = fork_plan(
        &store,
        &child_turn,
        &branch.snapshot.child_thread,
        ConversationForkKind::Branch,
        75,
    )
    .await;
    let nested_command = nested_plan.command.clone();
    let nested = store
        .reserve_conversation_fork(nested_plan)
        .await
        .expect("nested branch with copied copies");
    let metadata = store
        .load_conversation_fork_metadata(&nested.snapshot.child_thread.id)
        .await
        .expect("nested inherited metadata");
    assert_eq!(metadata.inherited_assistant_outcomes.len(), 3);
    assert!(
        metadata
            .inherited_assistant_outcomes
            .iter()
            .any(|outcome| outcome.source_turn_id == first.turn.id)
    );
    assert!(
        metadata
            .inherited_assistant_outcomes
            .iter()
            .any(|outcome| outcome.source_turn_id == second.turn.id)
    );

    let copied_first = nested
        .snapshot
        .messages
        .iter()
        .find(|message| {
            matches!(
                &message.derivation,
                grok_domain::ConversationMessageDerivation::Fork {
                    source_message_id,
                    ..
                } if source_message_id == &branch.snapshot.messages[1].id
            )
        })
        .expect("nested copy of first assistant")
        .id
        .clone();
    drop(store);

    let connection = raw_connection(&path, [106; 32]);
    connection
        .execute_batch("DROP TRIGGER conversation_inherited_assistant_outcomes_immutable_update;")
        .expect("disable immutable outcome update for corruption fixture");
    connection
        .execute(
            "UPDATE conversation_inherited_assistant_outcomes
             SET source_turn_id=?1 WHERE child_assistant_message_id=?2",
            [second.turn.id.as_str(), copied_first.as_str()],
        )
        .expect("reattribute same-content outcome");
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened
            .load_conversation_fork_by_command(&nested_command)
            .await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
    assert!(matches!(
        reopened
            .load_conversation_fork_metadata(&nested.snapshot.child_thread.id)
            .await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
}

#[tokio::test]
async fn oversized_inherited_metadata_rejects_fork_without_partial_child() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("fork-metadata-budget.db");
    let key = Arc::new(EphemeralKeyProvider::new([107; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "metadata-budget").await;
    let citations = large_citations();
    let mut latest = None;
    for index in 76..80 {
        let turn = reserve_turn(&store, project_id.clone(), thread_id.clone(), index).await;
        let turn = store
            .commit_provider_start(provider_start_commit(&turn.snapshot, index))
            .await
            .expect("provider start");
        append_completed_text(&store, &turn, index).await;
        latest = Some(
            store
                .commit_terminal(completed_commit_with_citations(
                    &turn,
                    index,
                    citations.clone(),
                ))
                .await
                .expect("citation-rich completion"),
        );
    }
    let latest = latest.expect("latest source");
    let parent = store.get_thread(&thread_id).await.expect("parent thread");
    let plan = fork_plan(&store, &latest, &parent, ConversationForkKind::Branch, 80).await;
    let child_id = plan.child_thread.id.clone();
    assert!(matches!(
        store.reserve_conversation_fork(plan).await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        store.get_thread(&child_id).await,
        Err(StoreError::NotFound)
    ));
    drop(store);

    let connection = raw_connection(&path, [107; 32]);
    let partial_rows: u32 = connection
        .query_row(
            "SELECT
                (SELECT count(*) FROM threads WHERE id=?1) +
                (SELECT count(*) FROM messages WHERE thread_id=?1) +
                (SELECT count(*) FROM conversation_thread_forks WHERE child_thread_id=?1) +
                (SELECT count(*) FROM conversation_fork_commands WHERE child_thread_id=?1) +
                (SELECT count(*) FROM conversation_fork_deliveries WHERE child_thread_id=?1)",
            [child_id.as_str()],
            |row| row.get(0),
        )
        .expect("partial fork rows");
    assert_eq!(partial_rows, 0);
}

#[tokio::test]
async fn persisted_metadata_budget_is_checked_before_materializing_citations() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("persisted-fork-metadata-budget.db");
    let key = Arc::new(EphemeralKeyProvider::new([108; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "persisted-metadata-budget").await;
    let mut sources = Vec::new();
    for index in 81..85 {
        let turn = reserve_turn(&store, project_id.clone(), thread_id.clone(), index).await;
        let turn = store
            .commit_provider_start(provider_start_commit(&turn.snapshot, index))
            .await
            .expect("provider start");
        append_completed_text(&store, &turn, index).await;
        sources.push(
            store
                .commit_terminal(completed_commit(&turn, index))
                .await
                .expect("completion"),
        );
    }
    let parent = store.get_thread(&thread_id).await.expect("parent thread");
    let branch = store
        .reserve_conversation_fork(
            fork_plan(
                &store,
                sources.last().expect("source"),
                &parent,
                ConversationForkKind::Branch,
                85,
            )
            .await,
        )
        .await
        .expect("initial bounded branch");
    let child_id = branch.snapshot.child_thread.id.clone();
    drop(store);

    let citations_json = serde_json::to_string(
        &large_citations()
            .into_iter()
            .map(|citation| serde_json::json!({"title": citation.title, "url": citation.url}))
            .collect::<Vec<_>>(),
    )
    .expect("encode citations");
    let mut connection = raw_connection(&path, [108; 32]);
    let transaction = connection.transaction().expect("raw transaction");
    for source in &sources {
        transaction
            .execute(
                "UPDATE conversation_turns SET citations_json=?1 WHERE id=?2",
                [citations_json.as_str(), source.turn.id.as_str()],
            )
            .expect("inflate persisted citations");
    }
    transaction.commit().expect("commit corruption fixture");
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened.load_conversation_fork_metadata(&child_id).await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn deep_regenerate_lineage_rehydrates_with_bounded_validation() {
    const TEST_DEPTH: u8 = 24;
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("maximum-depth-regenerate.db");
    let key = Arc::new(EphemeralKeyProvider::new([110; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, root_thread_id) = seed_workspace(&store, "maximum-depth").await;
    let root = reserve_turn(&store, project_id, root_thread_id.clone(), 99).await;
    let root = store
        .commit_provider_start(provider_start_commit(&root.snapshot, 99))
        .await
        .expect("root provider start");
    append_completed_text(&store, &root, 99).await;
    let mut source = store
        .commit_terminal(completed_commit(&root, 99))
        .await
        .expect("root completion");
    let mut parent = store
        .get_thread(&root_thread_id)
        .await
        .expect("root thread");
    let mut final_command = None;
    let mut final_snapshot = None;
    let mut first_regenerate_turn_id = None;

    for depth in 1_u8..=TEST_DEPTH {
        let index = 99_u8.checked_add(depth).expect("test index");
        let plan = fork_plan(
            &store,
            &source,
            &parent,
            ConversationForkKind::Regenerate,
            index,
        )
        .await;
        final_command = Some(plan.command.clone());
        let fork = store
            .reserve_conversation_fork(plan)
            .await
            .expect("reserve regenerate");
        assert_eq!(fork.snapshot.child_thread.lineage.fork_depth, depth);
        let started = fork.snapshot.started_turn.clone().expect("regenerate turn");
        if first_regenerate_turn_id.is_none() {
            first_regenerate_turn_id = Some(started.turn.id.clone());
        }
        let started = store
            .commit_provider_start(provider_start_commit(&started, index))
            .await
            .expect("regenerate provider start");
        append_completed_text(&store, &started, index).await;
        source = store
            .commit_terminal(completed_commit(&started, index))
            .await
            .expect("regenerate completion");
        parent = fork.snapshot.child_thread.clone();
        final_snapshot = Some(fork.snapshot);
    }
    let final_command = final_command.expect("final command");
    let final_snapshot = final_snapshot.expect("final snapshot");
    let first_regenerate_turn_id = first_regenerate_turn_id.expect("first regenerate turn");
    let final_regenerate_turn_id = source.turn.id.clone();
    drop(store);

    let reopened = open(&path, key).await;
    let rehydrated = reopened
        .load_conversation_fork_by_command(&final_command)
        .await
        .expect("bounded maximum-depth load")
        .expect("final fork");
    assert_eq!(rehydrated.child_thread.lineage.fork_depth, TEST_DEPTH);
    assert_eq!(rehydrated.child_thread.id, final_snapshot.child_thread.id);
    assert_eq!(
        reopened
            .load_conversation_fork_metadata(&rehydrated.child_thread.id)
            .await
            .expect("deep lineage metadata")
            .family_threads
            .len(),
        usize::from(TEST_DEPTH) + 1
    );
    drop(reopened);

    let connection = raw_connection(&path, [110; 32]);
    connection
        .execute_batch("DROP TRIGGER conversation_turn_lineage_immutable_update;")
        .expect("enable lineage cycle fixture");
    connection
        .execute(
            "UPDATE conversation_turn_lineage SET source_turn_id=?1 WHERE turn_id=?2",
            [
                final_regenerate_turn_id.as_str(),
                first_regenerate_turn_id.as_str(),
            ],
        )
        .expect("inject fork turn cycle");
    drop(connection);

    let corrupted = open(&path, Arc::new(EphemeralKeyProvider::new([110; 32]))).await;
    assert!(matches!(
        corrupted
            .load_conversation_fork_by_command(&final_command)
            .await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn conversation_fork_fault_rolls_back_every_child_row_and_lineage_is_immutable() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("conversation-fork-rollback.db");
    let key = Arc::new(EphemeralKeyProvider::new([104; 32]));
    let store = open(&path, key.clone()).await;
    let (project_id, thread_id) = seed_workspace(&store, "fork-rollback").await;
    let reserved = reserve_turn(&store, project_id, thread_id.clone(), 65).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, 65))
        .await
        .expect("source provider start");
    append_completed_text(&store, &started, 65).await;
    let source = store
        .commit_terminal(completed_commit(&started, 65))
        .await
        .expect("source completion");
    let parent = store.get_thread(&thread_id).await.expect("parent thread");
    let branch = fork_plan(&store, &source, &parent, ConversationForkKind::Branch, 66).await;
    let branch_command = branch.command.clone();
    let child_id = branch.child_thread.id.clone();
    drop(store);

    let connection = raw_connection(&path, [104; 32]);
    connection
        .execute_batch(
            "CREATE TRIGGER reject_conversation_fork_command
             BEFORE INSERT ON conversation_fork_commands BEGIN
                 SELECT RAISE(ABORT, 'injected fork command failure');
             END;",
        )
        .expect("install fork fault");
    drop(connection);
    let reopened = open(&path, key.clone()).await;
    assert!(matches!(
        reopened.reserve_conversation_fork(branch.clone()).await,
        Err(StoreError::Conflict)
    ));
    assert!(matches!(
        reopened.get_thread(&child_id).await,
        Err(StoreError::NotFound)
    ));
    drop(reopened);

    let connection = raw_connection(&path, [104; 32]);
    let partial_rows: u32 = connection
        .query_row(
            "SELECT
                (SELECT count(*) FROM threads WHERE id=?1) +
                (SELECT count(*) FROM messages WHERE thread_id=?1) +
                (SELECT count(*) FROM conversation_thread_forks WHERE child_thread_id=?1) +
                (SELECT count(*) FROM conversation_fork_commands WHERE child_thread_id=?1) +
                (SELECT count(*) FROM conversation_fork_deliveries WHERE child_thread_id=?1)",
            [child_id.as_str()],
            |row| row.get(0),
        )
        .expect("partial fork rows");
    assert_eq!(partial_rows, 0);
    connection
        .execute_batch("DROP TRIGGER reject_conversation_fork_command;")
        .expect("remove fork fault");
    drop(connection);

    let reopened = open(&path, key.clone()).await;
    let created = reopened
        .reserve_conversation_fork(branch)
        .await
        .expect("fork after rollback");
    drop(reopened);
    let connection = raw_connection(&path, [104; 32]);
    assert!(
        connection
            .execute(
                "UPDATE conversation_thread_forks SET fork_depth=2 WHERE child_thread_id=?1",
                [created.snapshot.child_thread.id.as_str()],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "UPDATE conversation_fork_deliveries SET revision=1
                 WHERE child_thread_id=?1",
                [created.snapshot.child_thread.id.as_str()],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "DELETE FROM conversation_fork_deliveries WHERE child_thread_id=?1",
                [created.snapshot.child_thread.id.as_str()],
            )
            .is_err()
    );
    assert!(
        connection
            .execute(
                "UPDATE conversation_message_derivations SET kind=2
                 WHERE child_message_id=?1",
                [created.snapshot.messages[0].id.as_str()],
            )
            .is_err()
    );
    connection
        .execute_batch(
            "DROP TRIGGER conversation_inherited_assistant_outcomes_immutable_delete;
             DELETE FROM conversation_inherited_assistant_outcomes;",
        )
        .expect("inject missing inherited outcome");
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened
            .load_conversation_fork_by_command(&branch_command)
            .await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted conversation aggregate"
    ));
}

#[allow(clippy::too_many_lines)]
async fn fork_plan(
    store: &Arc<SqlCipherStore>,
    source: &ConversationTurnSnapshot,
    parent: &Thread,
    kind: ConversationForkKind,
    index: u8,
) -> ConversationForkPlan {
    let now = source.turn.updated_at + 1;
    let source_context = store
        .load_turn_context(&source.turn.id)
        .await
        .expect("source context");
    let source_message = match kind {
        ConversationForkKind::EditAndBranch => &source.user_message,
        ConversationForkKind::Branch | ConversationForkKind::Regenerate => source
            .assistant_message
            .as_ref()
            .expect("completed source assistant"),
    };
    let child = Thread::new_fork(
        ThreadId::new(format!("security-fork-thread-{index}")).expect("child thread ID"),
        parent.project_id.clone(),
        parent.title.clone(),
        parent.id.clone(),
        &parent.lineage,
        source.turn.id.clone(),
        source_message.id.clone(),
        source_message.role,
        kind,
        now,
    )
    .expect("child thread");
    let context_copy_count = if kind == ConversationForkKind::EditAndBranch {
        source_context.len() - 1
    } else {
        source_context.len()
    };
    let mut messages = source_context
        .iter()
        .take(context_copy_count)
        .enumerate()
        .map(|(position, message)| {
            Message::new_derived(
                MessageId::new(format!("security-fork-message-{index}-{}", position + 1))
                    .expect("copied message ID"),
                child.id.clone(),
                u64::try_from(position + 1).expect("sequence"),
                message.role,
                message.content.clone(),
                message.id.clone(),
                source.turn.id.clone(),
                Some(u32::try_from(position + 1).expect("context position")),
                ConversationMessageDerivationKind::ContextCopy,
                now,
            )
            .expect("context copy")
        })
        .collect::<Vec<_>>();
    match kind {
        ConversationForkKind::Branch => messages.push(
            Message::new_derived(
                MessageId::new(format!("security-fork-message-{index}-assistant"))
                    .expect("assistant copy ID"),
                child.id.clone(),
                u64::try_from(messages.len() + 1).expect("assistant sequence"),
                MessageRole::Assistant,
                source_message.content.clone(),
                source_message.id.clone(),
                source.turn.id.clone(),
                None,
                ConversationMessageDerivationKind::SourceAssistantCopy,
                now,
            )
            .expect("assistant copy"),
        ),
        ConversationForkKind::EditAndBranch => messages.push(
            Message::new_derived(
                MessageId::new(format!("security-fork-message-{index}-edited"))
                    .expect("edited message ID"),
                child.id.clone(),
                u64::try_from(messages.len() + 1).expect("edited sequence"),
                MessageRole::User,
                format!("Edited prompt {index}"),
                source.user_message.id.clone(),
                source.turn.id.clone(),
                Some(u32::try_from(source_context.len()).expect("edited context position")),
                ConversationMessageDerivationKind::EditedUser,
                now,
            )
            .expect("edited message"),
        ),
        ConversationForkKind::Regenerate => {}
    }
    let scope = match kind {
        ConversationForkKind::Branch => "branch_conversation_thread",
        ConversationForkKind::EditAndBranch => "edit_and_branch_conversation_turn",
        ConversationForkKind::Regenerate => "regenerate_conversation_turn",
    };
    let command = MutationCommand {
        scope: scope.into(),
        key: format!("security-fork-command-{index}"),
        fingerprint: [index; 32],
    };
    let started_turn = if kind == ConversationForkKind::Branch {
        None
    } else {
        let user = messages.last().expect("fork user");
        let run = Run::queued(
            RunId::new(format!("security-fork-run-{index}")).expect("fork run ID"),
            child.project_id.clone(),
            child.id.clone(),
            now,
        );
        let turn = ConversationTurn::reserve(
            ConversationTurnId::new(format!("security-fork-turn-{index}")).expect("fork turn ID"),
            command.key.clone(),
            command.fingerprint,
            child.project_id.clone(),
            child.id.clone(),
            user.id.clone(),
            run.id.clone(),
            source.turn.model_id.clone(),
            now,
        )
        .expect("fork turn");
        let binding = source
            .lineage
            .credential_binding_id
            .clone()
            .expect("source binding");
        let lineage = match kind {
            ConversationForkKind::EditAndBranch => {
                ConversationTurnLineage::edit_and_branch(source.turn.id.clone(), binding)
                    .expect("edit lineage")
            }
            ConversationForkKind::Regenerate => {
                ConversationTurnLineage::regenerate(source.turn.id.clone(), binding)
                    .expect("regenerate lineage")
            }
            ConversationForkKind::Branch => unreachable!("handled above"),
        };
        Some(ConversationForkTurnPlan {
            turn,
            lineage,
            run,
            run_event: NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::Created,
            },
            turn_event: ConversationTurnEventKind::Created,
        })
    };
    ConversationForkPlan {
        command,
        source_turn_id: source.turn.id.clone(),
        expected_source_revision: source.turn.revision,
        child_thread: child,
        messages,
        started_turn,
    }
}

async fn open(path: &Path, key: Arc<EphemeralKeyProvider>) -> Arc<SqlCipherStore> {
    Arc::new(
        SqlCipherStore::open(path, key)
            .await
            .expect("open encrypted store"),
    )
}

fn raw_connection(path: &Path, key: [u8; 32]) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(path).expect("raw encrypted connection");
    connection
        .execute_batch(&format!("PRAGMA key = \"x'{}'\";", hex::encode(key)))
        .expect("unlock encrypted fixture");
    connection
}

async fn seed_workspace(store: &Arc<SqlCipherStore>, suffix: &str) -> (ProjectId, ThreadId) {
    let workspace = WorkspaceService::new(
        store.clone(),
        Arc::new(FixedClock::new(1)),
        Arc::new(SequentialIdGenerator::new()),
    );
    let project = workspace
        .create_project(
            CreateProject {
                name: format!("Conversation {suffix}"),
                description: String::new(),
            },
            &format!("{suffix}-project"),
        )
        .await
        .expect("project");
    let thread = workspace
        .create_thread(
            CreateThread {
                project_id: project.id.to_string(),
                title: format!("Conversation {suffix}"),
            },
            &format!("{suffix}-thread"),
        )
        .await
        .expect("thread");
    (project.id, thread.id)
}

async fn reserve_turn(
    store: &Arc<SqlCipherStore>,
    project_id: ProjectId,
    thread_id: ThreadId,
    index: u8,
) -> ConversationTurnReservation {
    let candidate = original_candidate(project_id, thread_id, index, TEST_CREDENTIAL_BINDING);
    store
        .reserve_turn(
            candidate.0,
            candidate.1,
            ConversationTurnReservationSource::CurrentThread,
            candidate.2,
            candidate.3,
            candidate.4,
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("reserve turn")
}

fn original_candidate(
    project_id: ProjectId,
    thread_id: ThreadId,
    index: u8,
    credential_binding_id: &str,
) -> (
    ConversationTurn,
    ConversationTurnLineage,
    Message,
    Run,
    NewRunEvent,
) {
    let now = u64::from(index) * 10;
    let user = Message::new(
        MessageId::new(format!("security-user-{index}")).expect("message id"),
        thread_id.clone(),
        MessageRole::User,
        format!("Prompt {index}"),
        now,
    )
    .expect("user message");
    let run = Run::queued(
        RunId::new(format!("security-run-{index}")).expect("run id"),
        project_id.clone(),
        thread_id.clone(),
        now,
    );
    let turn = ConversationTurn::reserve(
        ConversationTurnId::new(format!("security-turn-{index}")).expect("turn id"),
        format!("security-command-{index}"),
        [index; 32],
        project_id,
        thread_id,
        user.id.clone(),
        run.id.clone(),
        "grok-4.3".into(),
        now,
    )
    .expect("turn");
    (
        turn,
        ConversationTurnLineage::original(credential_binding_id.into()).expect("original lineage"),
        user,
        run,
        NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::Created,
        },
    )
}

fn retry_candidate(
    source: &ConversationTurnSnapshot,
    index: u8,
) -> (
    ConversationTurn,
    ConversationTurnLineage,
    Message,
    Run,
    NewRunEvent,
) {
    let now = source.turn.updated_at + 1;
    let user = Message::new(
        MessageId::new(format!("security-retry-user-{index}")).expect("retry message id"),
        source.turn.thread_id.clone(),
        MessageRole::User,
        source.user_message.content.clone(),
        now,
    )
    .expect("retry user message");
    let run = Run::queued(
        RunId::new(format!("security-retry-run-{index}")).expect("retry run id"),
        source.turn.project_id.clone(),
        source.turn.thread_id.clone(),
        now,
    );
    let turn = ConversationTurn::reserve(
        ConversationTurnId::new(format!("security-retry-turn-{index}")).expect("retry turn id"),
        format!("security-retry-command-{index}"),
        [index; 32],
        source.turn.project_id.clone(),
        source.turn.thread_id.clone(),
        user.id.clone(),
        run.id.clone(),
        source.turn.model_id.clone(),
        now,
    )
    .expect("retry turn");
    let lineage = ConversationTurnLineage::retry(
        source.turn.id.clone(),
        source
            .lineage
            .credential_binding_id
            .clone()
            .expect("bound source"),
        source.lineage.retry_depth,
    )
    .expect("retry lineage");
    (
        turn,
        lineage,
        user,
        run,
        NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::Created,
        },
    )
}

fn provider_start_commit(snapshot: &ConversationTurnSnapshot, index: u8) -> ProviderStartCommit {
    let now = snapshot.turn.created_at + 1;
    let mut turn = snapshot.turn.clone();
    let mut run = snapshot.run.clone();
    let mut effect = SideEffect::prepare(
        EffectId::new(format!("security-effect-{index}")).expect("effect id"),
        run.id.clone(),
        EffectKind::ExternalMutation,
        format!("official xAI Responses API model {}", turn.model_id),
        Idempotency::NonIdempotent,
        now,
    );
    effect.start(now).expect("start effect");
    turn.start_provider(effect.id.clone(), [index.saturating_add(1); 32], now)
        .expect("start turn");
    run.transition(RunState::Planning, now).expect("planning");
    run.transition(RunState::Running, now).expect("running");
    ProviderStartCommit {
        turn,
        expected_turn_revision: snapshot.turn.revision,
        run,
        expected_run_revision: snapshot.run.revision,
        effect: effect.clone(),
        events: vec![
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
                    effect_id: effect.id,
                },
            },
        ],
        turn_event: ConversationTurnEventKind::StateChanged {
            from: ConversationTurnState::Reserved,
            to: ConversationTurnState::ProviderStarted,
        },
    }
}

fn completed_commit(snapshot: &ConversationTurnSnapshot, index: u8) -> TerminalTurnCommit {
    completed_commit_with_text(snapshot, index, format!("Answer {index}"))
}

fn completed_commit_with_text(
    snapshot: &ConversationTurnSnapshot,
    index: u8,
    text: String,
) -> TerminalTurnCommit {
    completed_commit_with_metadata(snapshot, index, text, Vec::new())
}

fn completed_commit_with_citations(
    snapshot: &ConversationTurnSnapshot,
    index: u8,
    citations: Vec<ConversationCitation>,
) -> TerminalTurnCommit {
    completed_commit_with_metadata(snapshot, index, format!("Answer {index}"), citations)
}

fn completed_commit_with_metadata(
    snapshot: &ConversationTurnSnapshot,
    index: u8,
    text: String,
    citations: Vec<ConversationCitation>,
) -> TerminalTurnCommit {
    let now = snapshot.turn.updated_at + 1;
    let mut turn = snapshot.turn.clone();
    let mut run = snapshot.run.clone();
    let mut effect = snapshot.effect.clone().expect("effect");
    let assistant = Message::new(
        MessageId::new(format!("security-assistant-{index}")).expect("assistant id"),
        turn.thread_id.clone(),
        MessageRole::Assistant,
        text,
        now,
    )
    .expect("assistant");
    turn.complete(
        assistant.id.clone(),
        Some(format!("response-{index}")),
        citations,
        ConversationUsage::default(),
        Some(true),
        now,
    )
    .expect("complete turn");
    effect.finish(true, now).expect("complete effect");
    run.transition(RunState::Completed, now)
        .expect("complete run");
    terminal_commit(
        snapshot,
        turn,
        run,
        Some(effect),
        Some(assistant),
        vec![NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::StateChanged {
                from: RunState::Running,
                to: RunState::Completed,
            },
        }],
    )
}

fn large_citations() -> Vec<ConversationCitation> {
    (0..128)
        .map(|index| {
            let prefix = format!("https://example.test/{index}/");
            ConversationCitation {
                title: Some(format!("Citation {index}")),
                url: format!("{prefix}{}", "a".repeat(7_000 - prefix.len())),
            }
        })
        .collect()
}

fn failed_commit(snapshot: &ConversationTurnSnapshot) -> TerminalTurnCommit {
    let now = snapshot.turn.updated_at + 1;
    let mut turn = snapshot.turn.clone();
    let mut run = snapshot.run.clone();
    let mut effect = snapshot.effect.clone().expect("effect");
    turn.fail(
        ConversationFailure {
            kind: ConversationFailureKind::Unavailable,
            message: "provider unavailable".into(),
            retryable: true,
        },
        now,
    )
    .expect("fail turn");
    effect.finish(false, now).expect("fail effect");
    run.transition(RunState::Failed, now).expect("fail run");
    terminal_commit(
        snapshot,
        turn,
        run,
        Some(effect),
        None,
        vec![NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::StateChanged {
                from: RunState::Running,
                to: RunState::Failed,
            },
        }],
    )
}

fn interrupted_commit(snapshot: &ConversationTurnSnapshot) -> TerminalTurnCommit {
    let now = snapshot.turn.updated_at + 1;
    let mut turn = snapshot.turn.clone();
    let mut run = snapshot.run.clone();
    let mut effect = snapshot.effect.clone().expect("effect");
    turn.interrupt(now).expect("interrupt turn");
    effect.interrupt(now).expect("interrupt effect");
    run.transition(RunState::InterruptedNeedsReview, now)
        .expect("interrupt run");
    terminal_commit(
        snapshot,
        turn,
        run,
        Some(effect.clone()),
        None,
        vec![
            NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::EffectNeedsReview {
                    effect_id: effect.id,
                },
            },
            NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::StateChanged {
                    from: RunState::Running,
                    to: RunState::InterruptedNeedsReview,
                },
            },
        ],
    )
}

fn cancelled_commit(snapshot: &ConversationTurnSnapshot) -> TerminalTurnCommit {
    let now = snapshot.turn.updated_at + 1;
    let mut turn = snapshot.turn.clone();
    let mut run = snapshot.run.clone();
    turn.cancel(now).expect("cancel turn");
    run.transition(RunState::Cancelled, now)
        .expect("cancel run");
    terminal_commit(
        snapshot,
        turn,
        run,
        None,
        None,
        vec![NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::StateChanged {
                from: RunState::Queued,
                to: RunState::Cancelled,
            },
        }],
    )
}

fn terminal_commit(
    snapshot: &ConversationTurnSnapshot,
    turn: ConversationTurn,
    run: Run,
    effect: Option<SideEffect>,
    assistant_message: Option<Message>,
    events: Vec<NewRunEvent>,
) -> TerminalTurnCommit {
    let turn_event = ConversationTurnEventKind::StateChanged {
        from: snapshot.turn.state,
        to: turn.state,
    };
    TerminalTurnCommit {
        turn,
        expected_turn_revision: snapshot.turn.revision,
        run,
        expected_run_revision: snapshot.run.revision,
        expected_effect_revision: snapshot.effect.as_ref().map(|effect| effect.revision),
        effect,
        assistant_message,
        events,
        turn_event,
    }
}

async fn append_completed_text(
    store: &Arc<SqlCipherStore>,
    snapshot: &ConversationTurnSnapshot,
    index: u8,
) {
    store
        .append_turn_text(
            &snapshot.turn.id,
            snapshot.turn.revision,
            0,
            format!("Answer {index}"),
        )
        .await
        .expect("append completed text");
}

async fn append_text(store: &Arc<SqlCipherStore>, snapshot: &ConversationTurnSnapshot, text: &str) {
    store
        .append_turn_text(&snapshot.turn.id, snapshot.turn.revision, 0, text.into())
        .await
        .expect("append text");
}

fn command(index: u8) -> MutationCommand {
    MutationCommand {
        scope: "execute_conversation_turn".into(),
        key: format!("security-command-{index}"),
        fingerprint: [index; 32],
    }
}

fn fork_delivery_ack_command(key: &str, fingerprint: [u8; 32]) -> MutationCommand {
    MutationCommand {
        scope: "acknowledge_conversation_fork_delivery".into(),
        key: format!("security-fork-delivery-ack-{key}"),
        fingerprint,
    }
}

fn cancel_command(key: &str, fingerprint: [u8; 32]) -> MutationCommand {
    scoped_cancel_command("cancel_conversation_turn", key, fingerprint)
}

fn reconciliation_command(key: &str, fingerprint: [u8; 32]) -> MutationCommand {
    scoped_cancel_command("reconcile_conversation_dispatch_exit", key, fingerprint)
}

fn scoped_cancel_command(scope: &str, key: &str, fingerprint: [u8; 32]) -> MutationCommand {
    MutationCommand {
        scope: scope.into(),
        key: format!("security-cancel-{key}"),
        fingerprint,
    }
}

async fn load_turn(store: &Arc<SqlCipherStore>, index: u8) -> ConversationTurnSnapshot {
    store
        .load_turn_by_command(&command(index))
        .await
        .expect("load command")
        .expect("turn")
}
