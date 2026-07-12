//! `SQLCipher` coverage for provider outcomes at durable representation bounds.

use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use async_trait::async_trait;
use futures_util::stream;
use grok_application::{
    Citation, ConversationEvent, ConversationModel, ConversationModelFactory, ConversationRequest,
    ConversationService, ConversationStream, ConversationTurnSnapshot, CreateProject, CreateThread,
    CredentialService, DEFAULT_XAI_CHAT_MODEL_ID, ExecuteConversationTurn, ModelDescriptor,
    ModelError, SecretName, SecretValue, SecretVault, Usage, WorkspaceService, XaiApiKeyValidation,
    XaiApiKeyValidationError, XaiApiKeyValidator,
};
use grok_domain::{
    ConversationTurnState, ConversationUsage, EffectState, MAX_CONVERSATION_USAGE_VALUE, RunState,
    ThreadId,
};
use grok_memory::{EphemeralKeyProvider, FixedClock, InMemorySecretVault, SequentialIdGenerator};
use grok_sqlcipher::SqlCipherStore;

const CITATION_TOTAL_BYTES: usize = 1_000_000;
const CITATION_COUNT: usize = 125;
const CITATION_URL_BYTES_AT_BOUND: usize = CITATION_TOTAL_BYTES / CITATION_COUNT;

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
struct ScriptedModel {
    events: Vec<Result<ConversationEvent, ModelError>>,
    list_calls: AtomicUsize,
    stream_calls: AtomicUsize,
}

impl ScriptedModel {
    fn new(events: Vec<Result<ConversationEvent, ModelError>>) -> Self {
        Self {
            events,
            list_calls: AtomicUsize::new(0),
            stream_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl ConversationModel for ScriptedModel {
    async fn list_models(&self) -> Result<Vec<ModelDescriptor>, ModelError> {
        self.list_calls.fetch_add(1, Ordering::SeqCst);
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
        Ok(Box::pin(stream::iter(self.events.clone())))
    }
}

#[derive(Debug)]
struct ScriptedFactory(Arc<ScriptedModel>);

impl ConversationModelFactory for ScriptedFactory {
    fn create(&self, _api_key: SecretValue) -> Result<Arc<dyn ConversationModel>, ModelError> {
        Ok(self.0.clone())
    }
}

#[tokio::test]
async fn unrepresentable_provider_outcomes_replay_exact_needs_review_after_restart() {
    let usage_overflow = completion_events(
        Vec::new(),
        Usage {
            input_tokens: MAX_CONVERSATION_USAGE_VALUE + 1,
            output_tokens: 1,
            cost_in_usd_ticks: 1,
        },
    );
    let citations_over_raw_bound = completion_events(
        citations(CITATION_COUNT, CITATION_URL_BYTES_AT_BOUND + 1),
        Usage::default(),
    );

    for (case, events) in [
        ("usage-overflow", usage_overflow),
        ("citations-over-raw-bound", citations_over_raw_bound),
    ] {
        Box::pin(exercise_needs_review_restart(case, events)).await;
    }
}

#[tokio::test]
async fn provider_outcome_at_durable_bounds_completes_and_replays_after_restart() {
    let bounded_citations = citations(CITATION_COUNT, CITATION_URL_BYTES_AT_BOUND);
    assert_eq!(
        bounded_citations
            .iter()
            .map(|citation| citation.url.len())
            .sum::<usize>(),
        CITATION_TOTAL_BYTES
    );
    let maximum = MAX_CONVERSATION_USAGE_VALUE;
    let events = completion_events(
        bounded_citations,
        Usage {
            input_tokens: maximum,
            output_tokens: maximum,
            cost_in_usd_ticks: maximum,
        },
    );
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("near-bound-conversation.db");
    let key = Arc::new(EphemeralKeyProvider::new([73; 32]));
    let model = Arc::new(ScriptedModel::new(events));
    let store = open(&path, key.clone()).await;
    let (service, thread_id) = seeded_service(store.clone(), model.clone(), "near-bound").await;
    let input = conversation_input(&thread_id);

    let committed = Box::pin(service.execute(
        input.clone(),
        "near-bound-command",
        Box::pin(std::future::pending()),
    ))
    .await
    .expect("near-bound provider outcome");
    assert_eq!(committed.turn.state, ConversationTurnState::Completed);
    assert_eq!(committed.run.state, RunState::Completed);
    assert_eq!(
        committed.effect.as_ref().expect("provider effect").state,
        EffectState::Succeeded
    );
    assert_eq!(committed.turn.citations.len(), CITATION_COUNT);
    assert_eq!(
        committed.turn.usage,
        ConversationUsage {
            input_tokens: maximum,
            output_tokens: maximum,
            cost_in_usd_ticks: maximum,
        }
    );
    assert_eq!(
        committed
            .assistant_message
            .as_ref()
            .expect("assistant message")
            .content,
        "bounded answer"
    );
    assert_eq!(model.list_calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);

    drop(service);
    drop(store);

    let reopened = open(&path, key).await;
    let replay_service = replay_service(reopened, model.clone());
    let replay = Box::pin(replay_service.execute(
        input,
        "near-bound-command",
        Box::pin(std::future::pending()),
    ))
    .await
    .expect("exact restart replay");
    assert_eq!(replay, committed);
    assert_eq!(model.list_calls.load(Ordering::SeqCst), 1);
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1);
}

async fn exercise_needs_review_restart(
    case: &str,
    events: Vec<Result<ConversationEvent, ModelError>>,
) {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join(format!("{case}.db"));
    let key = Arc::new(EphemeralKeyProvider::new([72; 32]));
    let model = Arc::new(ScriptedModel::new(events));
    let store = open(&path, key.clone()).await;
    let (service, thread_id) = seeded_service(store.clone(), model.clone(), case).await;
    let input = conversation_input(&thread_id);
    let command = format!("{case}-command");

    let interrupted =
        Box::pin(service.execute(input.clone(), &command, Box::pin(std::future::pending())))
            .await
            .expect("unsafe provider representation becomes reviewable");
    assert_needs_review_without_provider_outcome(&interrupted, case);
    assert_eq!(model.list_calls.load(Ordering::SeqCst), 1, "{case}");
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1, "{case}");

    drop(service);
    drop(store);

    let reopened = open(&path, key).await;
    let replay_service = replay_service(reopened, model.clone());
    let replay =
        Box::pin(replay_service.execute(input, &command, Box::pin(std::future::pending())))
            .await
            .expect("exact restart replay");
    assert_eq!(replay, interrupted, "{case}");
    assert_needs_review_without_provider_outcome(&replay, case);
    assert_eq!(model.list_calls.load(Ordering::SeqCst), 1, "{case}");
    assert_eq!(model.stream_calls.load(Ordering::SeqCst), 1, "{case}");
}

async fn open(path: &Path, key: Arc<EphemeralKeyProvider>) -> Arc<SqlCipherStore> {
    Arc::new(
        SqlCipherStore::open(path, key)
            .await
            .expect("open encrypted store"),
    )
}

async fn seeded_service(
    store: Arc<SqlCipherStore>,
    model: Arc<ScriptedModel>,
    case: &str,
) -> (ConversationService, ThreadId) {
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
                name: format!("Conversation {case}"),
                description: String::new(),
            },
            &format!("{case}-project"),
        )
        .await
        .expect("project");
    let thread = workspace
        .create_thread(
            CreateThread {
                project_id: project.id.to_string(),
                title: format!("Conversation {case}"),
            },
            &format!("{case}-thread"),
        )
        .await
        .expect("thread");
    let service = service(store, workspace, clock, ids, model);
    (service, thread.id)
}

fn replay_service(store: Arc<SqlCipherStore>, model: Arc<ScriptedModel>) -> ConversationService {
    let clock = Arc::new(FixedClock::new(20));
    let ids = Arc::new(SequentialIdGenerator::new());
    let workspace = Arc::new(WorkspaceService::new(
        store.clone(),
        clock.clone(),
        ids.clone(),
    ));
    service(store, workspace, clock, ids, model)
}

fn service(
    store: Arc<SqlCipherStore>,
    workspace: Arc<WorkspaceService>,
    clock: Arc<FixedClock>,
    ids: Arc<SequentialIdGenerator>,
    model: Arc<ScriptedModel>,
) -> ConversationService {
    let vault = Arc::new(InMemorySecretVault::new());
    vault
        .set(
            &SecretName::new("xai.api-key.primary").expect("secret name"),
            &SecretValue::new(b"xai-user-key".to_vec()).expect("secret"),
        )
        .expect("configured key");
    vault
        .set(
            &SecretName::new("xai.api-key.local-binding").expect("binding name"),
            &SecretValue::new(format!("xai-binding-{}", "1".repeat(64)).into_bytes())
                .expect("binding"),
        )
        .expect("configured key binding");
    let credentials = Arc::new(CredentialService::new(
        vault,
        store.clone(),
        Arc::new(AcceptXaiKey),
    ));
    ConversationService::new(
        store.clone(),
        workspace,
        credentials,
        Arc::new(ScriptedFactory(model)),
        clock,
        ids,
        store,
    )
}

fn conversation_input(thread_id: &ThreadId) -> ExecuteConversationTurn {
    ExecuteConversationTurn {
        thread_id: thread_id.to_string(),
        content: "Bounded provider result".into(),
        model_id: None,
    }
}

fn completion_events(
    citations: Vec<Citation>,
    usage: Usage,
) -> Vec<Result<ConversationEvent, ModelError>> {
    let mut events = vec![
        Ok(ConversationEvent::Started {
            continuation: Some("response-boundary".into()),
        }),
        Ok(ConversationEvent::TextDelta("bounded answer".into())),
    ];
    events.extend(
        citations
            .into_iter()
            .map(ConversationEvent::Citation)
            .map(Ok),
    );
    events.extend([
        Ok(ConversationEvent::Usage(usage)),
        Ok(ConversationEvent::RetentionObserved {
            zero_data_retention: true,
        }),
        Ok(ConversationEvent::Completed { continuation: None }),
    ]);
    events
}

fn citations(count: usize, url_bytes: usize) -> Vec<Citation> {
    (0..count)
        .map(|index| {
            let prefix = format!("https://example.test/{index}/");
            assert!(prefix.len() <= url_bytes);
            Citation {
                title: None,
                url: format!("{prefix}{}", "a".repeat(url_bytes - prefix.len())),
            }
        })
        .collect()
}

fn assert_needs_review_without_provider_outcome(snapshot: &ConversationTurnSnapshot, case: &str) {
    assert_eq!(
        snapshot.turn.state,
        ConversationTurnState::InterruptedNeedsReview,
        "{case}"
    );
    assert_eq!(
        snapshot.run.state,
        RunState::InterruptedNeedsReview,
        "{case}"
    );
    assert_eq!(
        snapshot.effect.as_ref().expect("provider effect").state,
        EffectState::NeedsReview,
        "{case}"
    );
    assert!(snapshot.assistant_message.is_none(), "{case}");
    assert!(snapshot.turn.assistant_message_id.is_none(), "{case}");
    assert!(snapshot.turn.failure.is_none(), "{case}");
    assert!(snapshot.turn.provider_response_id.is_none(), "{case}");
    assert!(snapshot.turn.citations.is_empty(), "{case}");
    assert_eq!(snapshot.turn.usage, ConversationUsage::default(), "{case}");
    assert_eq!(snapshot.turn.zero_data_retention, None, "{case}");
}
