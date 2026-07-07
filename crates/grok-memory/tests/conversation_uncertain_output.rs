//! Cross-layer regressions for untrusted post-dispatch conversation outcomes.

use std::time::Duration;
use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use async_trait::async_trait;
use futures_util::{StreamExt, stream};
use grok_application::{
    CancelConversationTurnCommit, Citation, ConversationEvent, ConversationModel,
    ConversationModelFactory, ConversationRequest, ConversationService, ConversationStream,
    ConversationThreadCredentialBinding, ConversationTurnEventPage, ConversationTurnReservation,
    ConversationTurnReservationSource, ConversationTurnSnapshot, ConversationTurnStore,
    CreateProject, CreateThread, CredentialService, DEFAULT_XAI_CHAT_MODEL_ID,
    ExecuteConversationTurn, ModelDescriptor, ModelError, ModelErrorKind, ModelFailureCertainty,
    MutationCommand, NewRunEvent, ProviderStartCommit, SecretName, SecretValue, SecretVault,
    StartConversationTurn, StoreError, TerminalTurnCommit, Usage, WorkspaceService,
    XaiApiKeyValidation, XaiApiKeyValidationError, XaiApiKeyValidator,
};
use grok_domain::{
    ConversationTurn, ConversationTurnEvent, ConversationTurnEventKind, ConversationTurnId,
    ConversationTurnLineage, ConversationTurnState, EffectState, MAX_CONVERSATION_TEXT_CHUNK_BYTES,
    MAX_CONVERSATION_USAGE_VALUE, MAX_MESSAGE_BYTES, Message, Run, RunState, ThreadId,
};
use grok_memory::{FixedClock, InMemoryExecutionStore, InMemorySecretVault, SequentialIdGenerator};

#[derive(Debug)]
struct AcceptXaiKey;

#[async_trait]
impl XaiApiKeyValidator for AcceptXaiKey {
    async fn validate(
        &self,
        _api_key: &SecretValue,
    ) -> Result<XaiApiKeyValidation, XaiApiKeyValidationError> {
        Ok(XaiApiKeyValidation::CapabilitiesResolved)
    }
}

#[derive(Debug)]
struct EventModel {
    events: Vec<Result<ConversationEvent, ModelError>>,
    rollback_clock: Option<Arc<FixedClock>>,
    pending_stream: bool,
    stream_calls: Arc<AtomicUsize>,
}

#[async_trait]
impl ConversationModel for EventModel {
    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
        Ok(vec![ModelDescriptor {
            id: DEFAULT_XAI_CHAT_MODEL_ID.into(),
            aliases: Vec::new(),
            input_modalities: vec!["text".into()],
            output_modalities: vec!["text".into()],
        }])
    }

    async fn stream(
        &self,
        _request: ConversationRequest,
    ) -> Result<ConversationStream, ModelError> {
        self.stream_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(clock) = &self.rollback_clock {
            clock.set(1);
        }
        if self.pending_stream {
            Ok(Box::pin(
                stream::iter(self.events.clone()).chain(stream::pending()),
            ))
        } else {
            Ok(Box::pin(stream::iter(self.events.clone())))
        }
    }
}

struct FaultingTerminalCommitStore {
    inner: Arc<InMemoryExecutionStore>,
    reject_once: AtomicBool,
    fault: TerminalCommitFault,
    append_calls: Arc<AtomicUsize>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TerminalCommitFault {
    None,
    BeforeCompleted,
    AfterCompleted,
    AfterInterrupted,
}

#[async_trait]
impl ConversationTurnStore for FaultingTerminalCommitStore {
    async fn reserve_turn(
        &self,
        turn: ConversationTurn,
        lineage: ConversationTurnLineage,
        source: ConversationTurnReservationSource,
        user_message: Message,
        run: Run,
        event: NewRunEvent,
        turn_event: ConversationTurnEventKind,
    ) -> Result<ConversationTurnReservation, StoreError> {
        self.inner
            .reserve_turn(turn, lineage, source, user_message, run, event, turn_event)
            .await
    }

    async fn load_turn_by_command(
        &self,
        command: &MutationCommand,
    ) -> Result<Option<ConversationTurnSnapshot>, StoreError> {
        self.inner.load_turn_by_command(command).await
    }

    async fn load_turn(
        &self,
        id: &ConversationTurnId,
    ) -> Result<Option<ConversationTurnSnapshot>, StoreError> {
        self.inner.load_turn(id).await
    }

    async fn load_turn_context(&self, id: &ConversationTurnId) -> Result<Vec<Message>, StoreError> {
        self.inner.load_turn_context(id).await
    }

    async fn commit_provider_start(
        &self,
        commit: ProviderStartCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        self.inner.commit_provider_start(commit).await
    }

    async fn commit_terminal(
        &self,
        commit: TerminalTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        let should_fault = matches!(
            (self.fault, commit.turn.state),
            (
                TerminalCommitFault::BeforeCompleted | TerminalCommitFault::AfterCompleted,
                ConversationTurnState::Completed
            ) | (
                TerminalCommitFault::AfterInterrupted,
                ConversationTurnState::InterruptedNeedsReview
            )
        );
        if should_fault && self.reject_once.swap(false, Ordering::SeqCst) {
            if matches!(
                self.fault,
                TerminalCommitFault::AfterCompleted | TerminalCommitFault::AfterInterrupted
            ) {
                self.inner.commit_terminal(commit).await?;
            }
            return Err(StoreError::Internal(
                "injected terminal-turn persistence failure".into(),
            ));
        }
        self.inner.commit_terminal(commit).await
    }

    async fn commit_cancellation(
        &self,
        commit: CancelConversationTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        self.inner.commit_cancellation(commit).await
    }

    async fn commit_dispatch_exit_reconciliation(
        &self,
        commit: CancelConversationTurnCommit,
    ) -> Result<ConversationTurnSnapshot, StoreError> {
        self.inner.commit_dispatch_exit_reconciliation(commit).await
    }

    async fn append_turn_text(
        &self,
        turn_id: &ConversationTurnId,
        expected_turn_revision: u64,
        start_utf8_offset: u64,
        text: String,
    ) -> Result<Vec<ConversationTurnEvent>, StoreError> {
        self.append_calls.fetch_add(1, Ordering::SeqCst);
        self.inner
            .append_turn_text(turn_id, expected_turn_revision, start_utf8_offset, text)
            .await
    }

    async fn list_turn_events_since(
        &self,
        turn_id: &ConversationTurnId,
        after_sequence: u64,
        limit: usize,
    ) -> Result<ConversationTurnEventPage, StoreError> {
        self.inner
            .list_turn_events_since(turn_id, after_sequence, limit)
            .await
    }

    async fn list_incomplete_turns_for_recovery(
        &self,
        limit: usize,
    ) -> Result<Vec<ConversationTurnSnapshot>, StoreError> {
        self.inner.list_incomplete_turns_for_recovery(limit).await
    }

    async fn list_thread_turns(
        &self,
        thread_id: &ThreadId,
        after: Option<&ConversationTurnId>,
        limit: usize,
    ) -> Result<Vec<ConversationTurnSnapshot>, StoreError> {
        self.inner.list_thread_turns(thread_id, after, limit).await
    }

    async fn retry_source_is_latest(&self, id: &ConversationTurnId) -> Result<bool, StoreError> {
        self.inner.retry_source_is_latest(id).await
    }

    async fn thread_credential_binding(
        &self,
        thread_id: &ThreadId,
    ) -> Result<ConversationThreadCredentialBinding, StoreError> {
        self.inner.thread_credential_binding(thread_id).await
    }
}

#[derive(Debug)]
struct EventFactory(Arc<EventModel>);

impl ConversationModelFactory for EventFactory {
    fn create(&self, _api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
        Ok(self.0.clone())
    }
}

async fn fixture(
    events: Vec<Result<ConversationEvent, ModelError>>,
) -> (ConversationService, String) {
    fixture_with_options(events, false, TerminalCommitFault::None, false).await
}

async fn fixture_with_options(
    events: Vec<Result<ConversationEvent, ModelError>>,
    rollback_clock: bool,
    terminal_commit_fault: TerminalCommitFault,
    pending_stream: bool,
) -> (ConversationService, String) {
    let (service, thread_id, _, _, _) = fixture_with_store(
        events,
        rollback_clock,
        terminal_commit_fault,
        pending_stream,
    )
    .await;
    (service, thread_id)
}

async fn fixture_with_store(
    events: Vec<Result<ConversationEvent, ModelError>>,
    rollback_clock: bool,
    terminal_commit_fault: TerminalCommitFault,
    pending_stream: bool,
) -> (
    ConversationService,
    String,
    Arc<InMemoryExecutionStore>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let store = Arc::new(InMemoryExecutionStore::new());
    let vault = Arc::new(InMemorySecretVault::new());
    vault
        .set(
            &SecretName::new("xai.api-key.primary").expect("secret name"),
            &SecretValue::new(b"xai-user-key".to_vec()).expect("test secret"),
        )
        .expect("configured test key");
    vault
        .set(
            &SecretName::new("xai.api-key.local-binding").expect("binding name"),
            &SecretValue::new(format!("xai-binding-{}", "1".repeat(64)).into_bytes())
                .expect("test binding"),
        )
        .expect("configured test binding");
    let credentials = Arc::new(CredentialService::new(
        vault,
        store.clone(),
        Arc::new(AcceptXaiKey),
    ));
    let clock = Arc::new(FixedClock::new(10));
    let ids = Arc::new(SequentialIdGenerator::new());
    let workspace = Arc::new(WorkspaceService::new(
        store.clone(),
        clock.clone(),
        ids.clone(),
    ));
    let project = workspace
        .create_project(
            CreateProject {
                name: "Provider validation".into(),
                description: String::new(),
            },
            "provider-validation-project",
        )
        .await
        .expect("project");
    let thread = workspace
        .create_thread(
            CreateThread {
                project_id: project.id.to_string(),
                title: "Provider validation".into(),
            },
            "provider-validation-thread",
        )
        .await
        .expect("thread");
    let append_calls = Arc::new(AtomicUsize::new(0));
    let turn_store: Arc<dyn ConversationTurnStore> = Arc::new(FaultingTerminalCommitStore {
        inner: store.clone(),
        reject_once: AtomicBool::new(true),
        fault: terminal_commit_fault,
        append_calls: append_calls.clone(),
    });
    let stream_calls = Arc::new(AtomicUsize::new(0));
    let service = ConversationService::new(
        turn_store,
        workspace,
        credentials,
        Arc::new(EventFactory(Arc::new(EventModel {
            events,
            rollback_clock: rollback_clock.then(|| clock.clone()),
            pending_stream,
            stream_calls: stream_calls.clone(),
        }))),
        clock,
        ids,
        store.clone(),
    );
    (
        service,
        thread.id.to_string(),
        store,
        stream_calls,
        append_calls,
    )
}

async fn execute(
    service: &ConversationService,
    thread_id: String,
    key: &str,
) -> grok_application::ConversationTurnSnapshot {
    Box::pin(service.execute(
        ExecuteConversationTurn {
            thread_id,
            content: "Hello".into(),
        },
        key,
        Box::pin(std::future::pending()),
    ))
    .await
    .expect("post-dispatch outcome is durably classified")
}

fn assert_needs_review(snapshot: &grok_application::ConversationTurnSnapshot) {
    assert_eq!(
        snapshot.turn.state,
        ConversationTurnState::InterruptedNeedsReview
    );
    assert_eq!(snapshot.run.state, RunState::InterruptedNeedsReview);
    assert_eq!(
        snapshot.effect.as_ref().expect("provider effect").state,
        EffectState::NeedsReview
    );
    assert!(snapshot.assistant_message.is_none());
}

struct CancelAfterFirstAppend {
    append_calls: Arc<AtomicUsize>,
}

impl Future for CancelAfterFirstAppend {
    type Output = ();

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        if self.append_calls.load(Ordering::SeqCst) > 0 {
            Poll::Ready(())
        } else {
            context.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

#[tokio::test]
async fn maximum_output_flush_is_bounded_and_checks_cancellation_between_batches() {
    let maximum_output = "x".repeat(MAX_MESSAGE_BYTES);
    let (service, thread_id, store, _, append_calls) = fixture_with_store(
        vec![
            Ok(ConversationEvent::TextDelta(maximum_output)),
            Ok(ConversationEvent::Completed { continuation: None }),
        ],
        false,
        TerminalCommitFault::None,
        false,
    )
    .await;
    let snapshot = tokio::time::timeout(
        Duration::from_secs(2),
        Box::pin(service.execute(
            ExecuteConversationTurn {
                thread_id,
                content: "Hello".into(),
            },
            "maximum-output-cancellation",
            Box::pin(CancelAfterFirstAppend {
                append_calls: append_calls.clone(),
            }),
        )),
    )
    .await
    .expect("bounded cancellation response")
    .expect("durable review classification");
    assert_needs_review(&snapshot);
    assert_eq!(append_calls.load(Ordering::SeqCst), 1);

    let events = store
        .list_turn_events_since(&snapshot.turn.id, 0, 100)
        .await
        .expect("bounded event log");
    assert_eq!(events.events.len(), 4);
    let text = events
        .events
        .iter()
        .find_map(|event| match &event.kind {
            ConversationTurnEventKind::TextAppended { text, .. } => Some(text),
            _ => None,
        })
        .expect("one progressive text event");
    assert_eq!(text.len(), MAX_CONVERSATION_TEXT_CHUNK_BYTES);
}

#[tokio::test]
async fn cancellation_after_final_text_flush_precedes_terminal_commit() {
    let (service, thread_id, _, _, append_calls) = fixture_with_store(
        vec![
            Ok(ConversationEvent::TextDelta("Final tail".into())),
            Ok(ConversationEvent::Usage(Usage::default())),
            Ok(ConversationEvent::Completed { continuation: None }),
        ],
        false,
        TerminalCommitFault::None,
        false,
    )
    .await;
    let snapshot = Box::pin(service.execute(
        ExecuteConversationTurn {
            thread_id,
            content: "Hello".into(),
        },
        "final-tail-cancellation",
        Box::pin(CancelAfterFirstAppend {
            append_calls: append_calls.clone(),
        }),
    ))
    .await
    .expect("durable cancellation classification");
    assert_needs_review(&snapshot);
    assert_eq!(append_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn start_returns_a_durable_reservation_before_provider_dispatch() {
    let (service, thread_id, _, stream_calls, _) = fixture_with_store(
        vec![
            Ok(ConversationEvent::TextDelta("Safe answer".into())),
            Ok(ConversationEvent::Usage(Usage::default())),
            Ok(ConversationEvent::Completed { continuation: None }),
        ],
        false,
        TerminalCommitFault::None,
        false,
    )
    .await;

    let started = service
        .start(
            StartConversationTurn {
                thread_id,
                content: "Hello".into(),
            },
            "async-start",
            Box::pin(std::future::pending()),
        )
        .await
        .expect("durable start");
    assert_eq!(started.snapshot.turn.state, ConversationTurnState::Reserved);
    assert_eq!(stream_calls.load(Ordering::SeqCst), 0);
    let initial_events = service
        .events_since(&started.snapshot.turn.id, 0, 100)
        .await
        .expect("created event");
    assert_eq!(initial_events.events.len(), 1);
    assert_eq!(
        initial_events.events[0].kind,
        ConversationTurnEventKind::Created
    );

    let completed = service
        .dispatch(
            started.dispatch.expect("new dispatch plan"),
            Box::pin(std::future::pending()),
        )
        .await
        .expect("background dispatch");
    assert_eq!(completed.turn.state, ConversationTurnState::Completed);
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);
    let final_events = service
        .events_since(&completed.turn.id, 0, 100)
        .await
        .expect("complete event stream");
    assert_eq!(final_events.events.len(), 4);
    assert!(matches!(
        final_events.events.last().map(|event| &event.kind),
        Some(ConversationTurnEventKind::StateChanged {
            from: ConversationTurnState::ProviderStarted,
            to: ConversationTurnState::Completed,
        })
    ));
}

#[tokio::test]
async fn reserved_cancel_is_exact_and_prevents_late_provider_dispatch() {
    let (service, thread_id, _, stream_calls, _) = fixture_with_store(
        vec![Ok(ConversationEvent::Completed { continuation: None })],
        false,
        TerminalCommitFault::None,
        false,
    )
    .await;
    let started = service
        .start(
            StartConversationTurn {
                thread_id,
                content: "Hello".into(),
            },
            "cancel-before-dispatch",
            Box::pin(std::future::pending()),
        )
        .await
        .expect("durable start");
    let turn_id = started.snapshot.turn.id.clone();
    let revision = started.snapshot.turn.revision;

    let cancelled = service
        .cancel(&turn_id, revision, "cancel-reserved-command")
        .await
        .expect("durable reserved cancellation");
    assert_eq!(cancelled.turn.state, ConversationTurnState::Cancelled);
    assert_eq!(stream_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        service
            .cancel(&turn_id, revision, "cancel-reserved-command")
            .await
            .expect("exact cancellation replay"),
        cancelled
    );
    assert!(matches!(
        service
            .cancel(&turn_id, revision + 1, "cancel-reserved-command")
            .await,
        Err(grok_application::ApplicationError::Conflict)
    ));

    let reconciled = service
        .dispatch(
            started.dispatch.expect("unclaimed dispatch plan"),
            Box::pin(std::future::pending()),
        )
        .await
        .expect("late dispatch observes cancellation");
    assert_eq!(reconciled, cancelled);
    assert_eq!(stream_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn provider_started_cancel_is_durable_before_the_dispatch_signal() {
    let (service, thread_id, store, stream_calls, _) =
        fixture_with_store(Vec::new(), false, TerminalCommitFault::None, true).await;
    let service = Arc::new(service);
    let started = service
        .start(
            StartConversationTurn {
                thread_id,
                content: "Hello".into(),
            },
            "cancel-provider-started",
            Box::pin(std::future::pending()),
        )
        .await
        .expect("durable start");
    let turn_id = started.snapshot.turn.id.clone();
    let (cancel_sender, cancel_receiver) = tokio::sync::oneshot::channel();
    let dispatch_service = service.clone();
    let dispatch = tokio::spawn(async move {
        dispatch_service
            .dispatch(
                started.dispatch.expect("dispatch plan"),
                Box::pin(async move {
                    let _ = cancel_receiver.await;
                }),
            )
            .await
    });

    let provider_started = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let snapshot = store
                .load_turn(&turn_id)
                .await
                .expect("turn load")
                .expect("turn");
            if snapshot.turn.state == ConversationTurnState::ProviderStarted {
                break snapshot;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("provider-started transition");
    assert_eq!(stream_calls.load(Ordering::SeqCst), 1);

    let classified = service
        .cancel(
            &turn_id,
            provider_started.turn.revision,
            "cancel-provider-command",
        )
        .await
        .expect("durable needs-review classification");
    assert_needs_review(&classified);
    assert!(!dispatch.is_finished());

    cancel_sender.send(()).expect("signal provider task");
    assert_eq!(
        dispatch
            .await
            .expect("dispatch task")
            .expect("terminal reconciliation"),
        classified
    );
}

#[tokio::test]
async fn cancel_binds_a_terminal_winner_one_revision_after_the_observation() {
    let (service, thread_id) = fixture(vec![
        Ok(ConversationEvent::TextDelta("Safe answer".into())),
        Ok(ConversationEvent::Usage(Usage::default())),
        Ok(ConversationEvent::Completed { continuation: None }),
    ])
    .await;
    let completed = execute(&service, thread_id, "terminal-cancel-race").await;
    assert_eq!(completed.turn.state, ConversationTurnState::Completed);

    let winner = service
        .cancel(
            &completed.turn.id,
            completed.turn.revision - 1,
            "terminal-winner-command",
        )
        .await
        .expect("terminal winner");
    assert_eq!(winner, completed);
    assert_eq!(
        service
            .cancel(
                &completed.turn.id,
                completed.turn.revision - 1,
                "terminal-winner-command",
            )
            .await
            .expect("terminal winner replay"),
        completed
    );
}

#[tokio::test]
async fn invalid_provider_text_becomes_review_instead_of_stranding_an_active_turn() {
    let (service, thread_id) = fixture(vec![
        Ok(ConversationEvent::TextDelta("unsafe\0text".into())),
        Ok(ConversationEvent::Completed { continuation: None }),
    ])
    .await;

    let snapshot = execute(&service, thread_id, "invalid-provider-text").await;
    assert_needs_review(&snapshot);
}

#[tokio::test]
async fn invalid_known_failure_becomes_review_without_retaining_untrusted_detail() {
    let (service, thread_id) = fixture(vec![Err(ModelError {
        kind: ModelErrorKind::Protocol,
        message: "unsafe\0failure".into(),
        retryable: false,
        certainty: ModelFailureCertainty::KnownFailure,
    })])
    .await;

    let snapshot = execute(&service, thread_id, "invalid-provider-failure").await;
    assert_needs_review(&snapshot);
    assert!(snapshot.turn.failure.is_none());
}

#[tokio::test]
async fn valid_provider_text_still_commits_one_canonical_assistant_message() {
    let (service, thread_id) = fixture(vec![
        Ok(ConversationEvent::TextDelta("Safe answer".into())),
        Ok(ConversationEvent::Usage(Usage::default())),
        Ok(ConversationEvent::Completed { continuation: None }),
    ])
    .await;

    let snapshot = execute(&service, thread_id, "valid-provider-text").await;
    assert_eq!(snapshot.turn.state, ConversationTurnState::Completed);
    assert_eq!(snapshot.run.state, RunState::Completed);
    assert_eq!(
        snapshot
            .assistant_message
            .as_ref()
            .map(|message| message.content.as_str()),
        Some("Safe answer")
    );
    assert_eq!(
        snapshot.effect.as_ref().expect("provider effect").state,
        EffectState::Succeeded
    );
}

#[tokio::test]
async fn completed_without_explicit_usage_accounting_becomes_review() {
    let (service, thread_id) = fixture(vec![
        Ok(ConversationEvent::TextDelta("Answer".into())),
        Ok(ConversationEvent::Completed { continuation: None }),
    ])
    .await;

    let snapshot = execute(&service, thread_id, "missing-provider-usage").await;
    assert_needs_review(&snapshot);
}

#[tokio::test]
async fn duplicate_citation_at_the_unique_capacity_does_not_become_review() {
    let mut events = (0..256)
        .map(|index| {
            Ok(ConversationEvent::Citation(Citation {
                title: None,
                url: format!("https://example.test/{index}"),
            }))
        })
        .collect::<Vec<Result<ConversationEvent, ModelError>>>();
    events.push(Ok(ConversationEvent::Citation(Citation {
        title: Some("duplicate metadata is ignored".into()),
        url: "https://example.test/0".into(),
    })));
    events.extend([
        Ok(ConversationEvent::TextDelta("Answer".into())),
        Ok(ConversationEvent::Usage(Usage::default())),
        Ok(ConversationEvent::Completed { continuation: None }),
    ]);
    let (service, thread_id) = fixture(events).await;

    let snapshot = execute(&service, thread_id, "duplicate-citation-at-capacity").await;
    assert_eq!(snapshot.turn.state, ConversationTurnState::Completed);
    assert_eq!(snapshot.turn.citations.len(), 256);
}

#[tokio::test]
async fn provider_usage_outside_the_durable_integer_range_becomes_review() {
    let (service, thread_id) = fixture(vec![
        Ok(ConversationEvent::TextDelta("Answer".into())),
        Ok(ConversationEvent::Usage(Usage {
            input_tokens: MAX_CONVERSATION_USAGE_VALUE + 1,
            output_tokens: 1,
            cost_in_usd_ticks: 1,
        })),
        Ok(ConversationEvent::Completed { continuation: None }),
    ])
    .await;

    let snapshot = execute(&service, thread_id, "oversized-provider-usage").await;
    assert_needs_review(&snapshot);
}

#[tokio::test]
async fn aggregate_citation_storage_bound_is_enforced_before_terminal_commit() {
    let mut events = Vec::new();
    for index in 0..130 {
        let prefix = format!("https://example.test/source-{index}/");
        let url = format!("{prefix}{}", "x".repeat(8_192 - prefix.len()));
        events.push(Ok(ConversationEvent::Citation(Citation {
            title: None,
            url,
        })));
    }
    events.push(Ok(ConversationEvent::TextDelta("Answer".into())));
    events.push(Ok(ConversationEvent::Completed { continuation: None }));
    let (service, thread_id) = fixture(events).await;

    let snapshot = execute(&service, thread_id, "oversized-provider-citations").await;
    assert_needs_review(&snapshot);
}

#[tokio::test]
async fn wall_clock_rollback_after_dispatch_uses_the_durable_timestamp_floor() {
    let (service, thread_id) = fixture_with_options(
        vec![
            Ok(ConversationEvent::TextDelta("Safe answer".into())),
            Ok(ConversationEvent::Usage(Usage::default())),
            Ok(ConversationEvent::Completed { continuation: None }),
        ],
        true,
        TerminalCommitFault::None,
        false,
    )
    .await;

    let snapshot = execute(&service, thread_id, "provider-clock-rollback").await;
    assert_eq!(snapshot.turn.state, ConversationTurnState::Completed);
    assert_eq!(snapshot.turn.updated_at, 10);
    assert_eq!(snapshot.run.state, RunState::Completed);
}

#[tokio::test]
async fn atomic_terminal_store_rejection_falls_back_to_review_and_replays_exactly() {
    let (service, thread_id) = fixture_with_options(
        vec![
            Ok(ConversationEvent::TextDelta("Safe answer".into())),
            Ok(ConversationEvent::Usage(Usage::default())),
            Ok(ConversationEvent::Completed { continuation: None }),
        ],
        false,
        TerminalCommitFault::BeforeCompleted,
        false,
    )
    .await;

    let snapshot = execute(
        &service,
        thread_id.clone(),
        "rejected-completion-persistence",
    )
    .await;
    assert_needs_review(&snapshot);
    assert_eq!(
        execute(&service, thread_id, "rejected-completion-persistence",).await,
        snapshot
    );
}

#[tokio::test]
async fn provider_events_after_completion_are_always_classified_as_uncertain() {
    let contradictory_tails = vec![
        vec![Ok(ConversationEvent::TextDelta("late delta".into()))],
        vec![Ok(ConversationEvent::Completed { continuation: None })],
        vec![Err(ModelError {
            kind: ModelErrorKind::Protocol,
            message: "late known failure".into(),
            retryable: false,
            certainty: ModelFailureCertainty::KnownFailure,
        })],
    ];
    for (index, tail) in contradictory_tails.into_iter().enumerate() {
        let mut events = vec![
            Ok(ConversationEvent::TextDelta("Answer".into())),
            Ok(ConversationEvent::Completed { continuation: None }),
        ];
        events.extend(tail);
        let (service, thread_id) = fixture(events).await;
        let snapshot = execute(
            &service,
            thread_id,
            &format!("post-completion-event-{index}"),
        )
        .await;
        assert_needs_review(&snapshot);
    }
}

#[tokio::test]
async fn ambiguous_success_response_reloads_the_already_committed_terminal_winner() {
    let (service, thread_id) = fixture_with_options(
        vec![
            Ok(ConversationEvent::TextDelta("Safe answer".into())),
            Ok(ConversationEvent::Usage(Usage::default())),
            Ok(ConversationEvent::Completed { continuation: None }),
        ],
        false,
        TerminalCommitFault::AfterCompleted,
        false,
    )
    .await;

    let snapshot = execute(&service, thread_id.clone(), "ambiguous-success-response").await;
    assert_eq!(snapshot.turn.state, ConversationTurnState::Completed);
    assert_eq!(
        snapshot
            .assistant_message
            .as_ref()
            .map(|message| message.content.as_str()),
        Some("Safe answer")
    );
    assert_eq!(
        execute(&service, thread_id, "ambiguous-success-response").await,
        snapshot
    );
}

#[tokio::test]
async fn ambiguous_cancellation_reloads_the_already_committed_review_winner() {
    let (service, thread_id) = fixture_with_options(
        Vec::new(),
        false,
        TerminalCommitFault::AfterInterrupted,
        true,
    )
    .await;
    let input = ExecuteConversationTurn {
        thread_id: thread_id.clone(),
        content: "Hello".into(),
    };

    let snapshot = Box::pin(service.execute(
        input,
        "ambiguous-cancellation-response",
        Box::pin(std::future::ready(())),
    ))
    .await
    .expect("durably committed review winner");
    assert_needs_review(&snapshot);
    assert_eq!(
        execute(&service, thread_id, "ambiguous-cancellation-response").await,
        snapshot
    );
}

#[tokio::test]
async fn normalized_text_is_durable_before_provider_completion() {
    let progressive_text = format!(
        "{}🙂tail",
        "a".repeat(MAX_CONVERSATION_TEXT_CHUNK_BYTES + 50)
    );
    let (service, thread_id, store, _, _) = fixture_with_store(
        vec![Ok(ConversationEvent::TextDelta(progressive_text))],
        false,
        TerminalCommitFault::None,
        true,
    )
    .await;
    let (cancel_sender, cancel_receiver) = tokio::sync::oneshot::channel();
    let execution = tokio::spawn(async move {
        Box::pin(service.execute(
            ExecuteConversationTurn {
                thread_id,
                content: "Hello".into(),
            },
            "progressive-durable-text",
            Box::pin(async move {
                let _ = cancel_receiver.await;
            }),
        ))
        .await
    });

    let (turn_id, progressive_event) = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(snapshot) = store
                .list_incomplete_turns_for_recovery(1)
                .await
                .expect("incomplete turn query")
                .into_iter()
                .next()
            {
                let page = store
                    .list_turn_events_since(&snapshot.turn.id, 0, 100)
                    .await
                    .expect("turn events");
                if let Some(event) = page.events.into_iter().find(|event| {
                    matches!(event.kind, ConversationTurnEventKind::TextAppended { .. })
                }) {
                    break (snapshot.turn.id, event);
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("progressive event before completion");
    assert_eq!(progressive_event.sequence, 3);
    assert!(matches!(
        progressive_event.kind,
        ConversationTurnEventKind::TextAppended {
            start_utf8_offset: 0,
            ref text,
        } if text.len() == MAX_CONVERSATION_TEXT_CHUNK_BYTES
    ));

    cancel_sender.send(()).expect("cancel execution");
    let snapshot = execution
        .await
        .expect("execution task")
        .expect("review outcome");
    assert_needs_review(&snapshot);
    let final_events = store
        .list_turn_events_since(&turn_id, 0, 100)
        .await
        .expect("final events");
    assert!(matches!(
        final_events.events.last().map(|event| &event.kind),
        Some(ConversationTurnEventKind::StateChanged {
            from: ConversationTurnState::ProviderStarted,
            to: ConversationTurnState::InterruptedNeedsReview,
        })
    ));
}
