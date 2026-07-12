//! Real `ConversationTurnStore::summarize_usage` coverage on `SQLCipher`.

use std::{path::Path, sync::Arc};

use grok_application::{
    ConversationTurnReservation, ConversationTurnReservationSource, ConversationTurnSnapshot,
    ConversationTurnStore, CreateProject, CreateThread, NewRunEvent, ProviderStartCommit,
    StoreError, TerminalTurnCommit, UsageScope, UsageWindow, WorkspaceService,
};
use grok_domain::{
    ConversationTurn, ConversationTurnEventKind, ConversationTurnId, ConversationTurnLineage,
    ConversationTurnState, ConversationUsage, EffectId, EffectKind, Idempotency, Message,
    MessageId, MessageRole, ProjectId, Run, RunEventKind, RunId, RunState, SideEffect, ThreadId,
};
use grok_memory::{EphemeralKeyProvider, FixedClock, SequentialIdGenerator};
use grok_sqlcipher::SqlCipherStore;

const BINDING: &str = "xai-binding-usage-test";

#[tokio::test]
async fn summarize_usage_scopes_windows_and_completed_only() {
    let directory = tempfile::tempdir().expect("tempdir");
    let store = open(
        &directory.path().join("usage-summary.db"),
        Arc::new(EphemeralKeyProvider::new([91; 32])),
    )
    .await;
    let ids = Arc::new(SequentialIdGenerator::new());
    let (project_a, thread_a) = seed_workspace(&store, ids.clone(), "a").await;
    let (project_b, thread_b) = seed_workspace(&store, ids.clone(), "b").await;

    complete_turn(
        &store,
        project_a.clone(),
        thread_a.clone(),
        1,
        usage(10, 4, 100),
    )
    .await;
    complete_turn(
        &store,
        project_a.clone(),
        thread_a.clone(),
        2,
        usage(20, 6, 200),
    )
    .await;
    complete_turn(
        &store,
        project_b.clone(),
        thread_b.clone(),
        3,
        usage(5, 1, 50),
    )
    .await;
    let _reserved = reserve_turn(&store, project_a.clone(), thread_a.clone(), 4).await;

    let as_of = 1_000_u64;
    let workspace = store
        .summarize_usage(UsageScope::Workspace, UsageWindow::AllTime, as_of)
        .await
        .expect("workspace all-time");
    assert_eq!(workspace.input_tokens, 35);
    assert_eq!(workspace.output_tokens, 11);
    assert_eq!(workspace.cost_in_usd_ticks, 350);
    assert_eq!(workspace.turn_count, 3);

    let project = store
        .summarize_usage(
            UsageScope::Project(project_a.clone()),
            UsageWindow::AllTime,
            as_of,
        )
        .await
        .expect("project summary");
    assert_eq!(project.input_tokens, 30);
    assert_eq!(project.output_tokens, 10);
    assert_eq!(project.turn_count, 2);

    let thread = store
        .summarize_usage(
            UsageScope::Thread(thread_b.clone()),
            UsageWindow::AllTime,
            as_of,
        )
        .await
        .expect("thread summary");
    assert_eq!(thread.input_tokens, 5);
    assert_eq!(thread.output_tokens, 1);
    assert_eq!(thread.turn_count, 1);

    // created_at uses index * 10. With as_of = 7d + 25, lower bound is 25 → only turn 3.
    let seven_days = 7 * 86_400_000_u64;
    let windowed = store
        .summarize_usage(
            UsageScope::Workspace,
            UsageWindow::Last7Days,
            seven_days + 25,
        )
        .await
        .expect("windowed summary");
    assert_eq!(windowed.input_tokens, 5);
    assert_eq!(windowed.turn_count, 1);

    assert!(matches!(
        store
            .summarize_usage(
                UsageScope::Project(ProjectId::new("missing-project").expect("id")),
                UsageWindow::AllTime,
                as_of,
            )
            .await,
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        store
            .summarize_usage(
                UsageScope::Thread(ThreadId::new("missing-thread").expect("id")),
                UsageWindow::AllTime,
                as_of,
            )
            .await,
        Err(StoreError::NotFound)
    ));
}

fn usage(input: u64, output: u64, cost: u64) -> ConversationUsage {
    ConversationUsage {
        input_tokens: input,
        output_tokens: output,
        cost_in_usd_ticks: cost,
    }
}

async fn complete_turn(
    store: &Arc<SqlCipherStore>,
    project_id: ProjectId,
    thread_id: ThreadId,
    index: u8,
    usage: ConversationUsage,
) {
    let reserved = reserve_turn(store, project_id, thread_id, index).await;
    let started = store
        .commit_provider_start(provider_start_commit(&reserved.snapshot, index))
        .await
        .expect("provider start");
    store
        .append_turn_text(
            &started.turn.id,
            started.turn.revision,
            0,
            format!("Answer {index}"),
        )
        .await
        .expect("append assistant text");
    store
        .commit_terminal(completed_commit(&started, index, usage))
        .await
        .expect("complete turn");
}

async fn open(path: &Path, key: Arc<EphemeralKeyProvider>) -> Arc<SqlCipherStore> {
    Arc::new(
        SqlCipherStore::open(path, key)
            .await
            .expect("open encrypted store"),
    )
}

async fn seed_workspace(
    store: &Arc<SqlCipherStore>,
    ids: Arc<SequentialIdGenerator>,
    suffix: &str,
) -> (ProjectId, ThreadId) {
    let workspace = WorkspaceService::new(store.clone(), Arc::new(FixedClock::new(1)), ids);
    let project = workspace
        .create_project(
            CreateProject {
                name: format!("Usage {suffix}"),
                description: String::new(),
            },
            &format!("usage-{suffix}-project"),
        )
        .await
        .expect("project");
    let thread = workspace
        .create_thread(
            CreateThread {
                project_id: project.id.to_string(),
                title: format!("Usage {suffix}"),
            },
            &format!("usage-{suffix}-thread"),
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
    let now = u64::from(index) * 10;
    let user = Message::new(
        MessageId::new(format!("usage-user-{index}")).expect("message id"),
        thread_id.clone(),
        MessageRole::User,
        format!("Prompt {index}"),
        now,
    )
    .expect("user message");
    let run = Run::queued(
        RunId::new(format!("usage-run-{index}")).expect("run id"),
        project_id.clone(),
        thread_id.clone(),
        now,
    );
    let turn = ConversationTurn::reserve(
        ConversationTurnId::new(format!("usage-turn-{index}")).expect("turn id"),
        format!("usage-command-{index}"),
        [index; 32],
        project_id,
        thread_id,
        user.id.clone(),
        run.id.clone(),
        "grok-4.3".into(),
        false,
        now,
    )
    .expect("turn");
    store
        .reserve_turn(
            turn,
            ConversationTurnLineage::original(BINDING.into()).expect("lineage"),
            ConversationTurnReservationSource::CurrentThread,
            user,
            run,
            NewRunEvent {
                occurred_at: now,
                kind: RunEventKind::Created,
            },
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("reserve")
}

fn provider_start_commit(snapshot: &ConversationTurnSnapshot, index: u8) -> ProviderStartCommit {
    let now = snapshot.turn.created_at + 1;
    let mut turn = snapshot.turn.clone();
    let mut run = snapshot.run.clone();
    let mut effect = SideEffect::prepare(
        EffectId::new(format!("usage-effect-{index}")).expect("effect id"),
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

fn completed_commit(
    snapshot: &ConversationTurnSnapshot,
    index: u8,
    usage: ConversationUsage,
) -> TerminalTurnCommit {
    let now = snapshot.turn.updated_at + 1;
    let mut turn = snapshot.turn.clone();
    let mut run = snapshot.run.clone();
    let mut effect = snapshot.effect.clone().expect("effect");
    let assistant = Message::new(
        MessageId::new(format!("usage-assistant-{index}")).expect("assistant id"),
        turn.thread_id.clone(),
        MessageRole::Assistant,
        format!("Answer {index}"),
        now,
    )
    .expect("assistant");
    turn.complete(
        assistant.id.clone(),
        Some(format!("response-{index}")),
        Vec::new(),
        usage,
        Some(true),
        now,
    )
    .expect("complete turn");
    effect.finish(true, now).expect("complete effect");
    run.transition(RunState::Completed, now)
        .expect("complete run");
    TerminalTurnCommit {
        turn,
        expected_turn_revision: snapshot.turn.revision,
        run,
        expected_run_revision: snapshot.run.revision,
        expected_effect_revision: snapshot.effect.as_ref().map(|item| item.revision),
        effect: Some(effect),
        assistant_message: Some(assistant),
        events: vec![NewRunEvent {
            occurred_at: now,
            kind: RunEventKind::StateChanged {
                from: RunState::Running,
                to: RunState::Completed,
            },
        }],
        turn_event: ConversationTurnEventKind::StateChanged {
            from: ConversationTurnState::ProviderStarted,
            to: ConversationTurnState::Completed,
        },
    }
}
