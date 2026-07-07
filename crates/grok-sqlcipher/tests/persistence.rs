//! Restart, concurrency, ordering, recovery, and interrupted-effect coverage.

use std::sync::Arc;

use grok_application::{
    AccountState, ApplicationError, ApprovalService, ArtifactContentReadyResult,
    ArtifactImportReservation, ArtifactStore, BeginPrivilegedDispatch, ChatModelPreferenceStore,
    ConversationTurnReservation, ConversationTurnReservationSource, ConversationTurnStore,
    CreateAutomation, CreateMessage, CreateProject, CreateRun, CreateThread,
    CredentialMutationReservation, CredentialMutationStore, DesktopPreferencesService,
    ExecutionStore, IdGenerator, MutationCommand, NewRunEvent, PrepareEffect,
    PreparePrivilegedOperation, PrivilegedOperationService, PrivilegedOperationStore,
    RequestApproval, RunService, SideEffectService, StoreError, TerminalTurnCommit,
    UpdateDesktopPreferences, UpdateMessage, UpdateProject, WorkspaceSearchHit,
    WorkspaceSearchKind, WorkspaceService, WorkspaceStore,
};
use grok_domain::{
    Approval, ApprovalDecision, ApprovalRisk, ApprovalScope, ApprovalStatus, Artifact, ArtifactId,
    ArtifactVersion, AuthorityGrantId, AutomationHistoryStatus, ChatModelPreference,
    ConversationTurn, ConversationTurnEventKind, ConversationTurnId, ConversationTurnLineage,
    ConversationTurnState, EffectKind, EffectState, Idempotency, Message, MessageId, MessageRole,
    MissedRunPolicy, OverlapPolicy, PayloadDigest, PrivilegedAuthority, PrivilegedIdempotency,
    PrivilegedIdempotencyKey, PrivilegedOperationIntent, PrivilegedOperationKind,
    PrivilegedOperationLinks, PrivilegedOperationState, PrivilegedOperationTarget,
    PrivilegedResourceId, ProjectId, RequestDigest, RequestedAction, Run, RunId, RunState, Thread,
    ThreadId,
};
use grok_memory::{
    EphemeralKeyProvider, FixedClock, InMemoryExecutionStore, SequentialIdGenerator,
};
use grok_sqlcipher::SqlCipherStore;
use sha2::{Digest, Sha256};

async fn open(path: &std::path::Path, key: Arc<EphemeralKeyProvider>) -> Arc<SqlCipherStore> {
    Arc::new(
        SqlCipherStore::open(path, key)
            .await
            .expect("open encrypted store"),
    )
}

async fn commit_test_artifact(
    store: &dyn ArtifactStore,
    id: &str,
    project_id: ProjectId,
    thread_id: Option<ThreadId>,
    name: &str,
    byte_size: u64,
    now: u64,
) -> Artifact {
    let artifact = Artifact::new_unavailable(
        ArtifactId::new(id).expect("artifact id"),
        project_id,
        thread_id,
        name.into(),
        now,
    )
    .expect("unavailable artifact");
    let command = MutationCommand {
        scope: "import_artifact".into(),
        key: format!("import-{id}"),
        fingerprint: Sha256::digest(id.as_bytes()).into(),
    };
    let prepared = match store
        .reserve_import(artifact.clone(), &command)
        .await
        .expect("reserve artifact")
    {
        ArtifactImportReservation::NewlyPrepared(plan) => plan,
        ArtifactImportReservation::ExactReplay(_) => panic!("unexpected artifact replay"),
    };
    let content = ArtifactVersion::new(
        artifact.id.clone(),
        1,
        Sha256::digest(format!("content-{id}").as_bytes()).into(),
        "text/plain".into(),
        byte_size,
        now,
    )
    .expect("artifact content");
    let ready = store
        .mark_content_ready(&artifact.id, prepared.revision, content.clone(), now)
        .await
        .expect("content ready");
    let ArtifactContentReadyResult::ContentReady(ready) = ready else {
        panic!("unexpected artifact quota failure");
    };
    let mut available = artifact;
    available
        .record_content(content.summary(), now)
        .expect("available artifact");
    store
        .commit_import(available, 0, ready.revision, content, now)
        .await
        .expect("commit artifact")
        .artifact
}

type SearchRoute = (String, &'static str, String, Option<String>);

fn search_kind_name(kind: WorkspaceSearchKind) -> &'static str {
    match kind {
        WorkspaceSearchKind::Project => "project",
        WorkspaceSearchKind::Thread => "thread",
        WorkspaceSearchKind::Message => "message",
        WorkspaceSearchKind::Artifact => "artifact",
        WorkspaceSearchKind::Automation => "automation",
    }
}

fn sorted_search_routes(hits: Vec<WorkspaceSearchHit>) -> Vec<SearchRoute> {
    let mut routes = hits
        .into_iter()
        .map(|hit| {
            (
                hit.id,
                search_kind_name(hit.kind),
                hit.project_id.to_string(),
                hit.thread_id.map(|id| id.to_string()),
            )
        })
        .collect::<Vec<_>>();
    routes.sort();
    routes
}

#[allow(clippy::too_many_lines)]
async fn exercise_workspace_search_conformance(
    repository: Arc<dyn WorkspaceStore>,
    artifacts: Arc<dyn ArtifactStore>,
) -> Vec<SearchRoute> {
    let clock = Arc::new(FixedClock::new(10));
    let workspace = WorkspaceService::new(
        repository.clone(),
        clock.clone(),
        Arc::new(SequentialIdGenerator::new()),
    );
    let project = workspace
        .create_project(
            CreateProject {
                name: "Rendezvous Café project".into(),
                description: "Primary orbital study".into(),
            },
            "search-project-primary",
        )
        .await
        .expect("primary project");
    clock.set(20);
    let thread = workspace
        .create_thread(
            CreateThread {
                project_id: project.id.to_string(),
                title: "Rendezvous flight thread".into(),
            },
            "search-thread",
        )
        .await
        .expect("thread");
    clock.set(30);
    let message = workspace
        .create_message(
            CreateMessage {
                thread_id: thread.id.to_string(),
                role: MessageRole::User,
                content: format!(
                    "Rendezvous launch notes {} tailbodymarker",
                    "extended canonical body ".repeat(16)
                ),
            },
            "search-message",
        )
        .await
        .expect("message");
    assert!(message.content.find("tailbodymarker").expect("tail marker") > 240);
    clock.set(40);
    let artifact = commit_test_artifact(
        artifacts.as_ref(),
        "search-artifact",
        project.id.clone(),
        Some(thread.id.clone()),
        "Rendezvous manifest",
        128,
        40,
    )
    .await;
    clock.set(50);
    let automation = workspace
        .create_automation(
            CreateAutomation {
                project_id: project.id.to_string(),
                title: "Rendezvous readiness".into(),
                prompt: "Prepare an orbital status brief".into(),
                schedule: "0 9 * * *".into(),
                timezone: "UTC".into(),
                missed_run_policy: MissedRunPolicy::Skip,
                overlap_policy: OverlapPolicy::Skip,
                enabled: false,
            },
            "search-automation",
        )
        .await
        .expect("automation");

    clock.set(60);
    let deleted_message = workspace
        .create_message(
            CreateMessage {
                thread_id: thread.id.to_string(),
                role: MessageRole::User,
                content: "deletedmessagemarker".into(),
            },
            "search-deleted-message",
        )
        .await
        .expect("deleted message fixture");
    clock.set(61);
    workspace
        .delete_message(&deleted_message.id, 0, "search-delete-message")
        .await
        .expect("delete message");
    clock.set(80);
    let other_project = workspace
        .create_project(
            CreateProject {
                name: "Rendezvous other project".into(),
                description: "Scope isolation".into(),
            },
            "search-project-other",
        )
        .await
        .expect("other project");

    let tail = repository
        .search(Some(&project.id), "tailbodymarker", 0, 10)
        .await
        .expect("tail-body search");
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0].id, message.id.as_str());
    let multi_term = repository
        .search(Some(&project.id), "RENDEZVOUS tailbodymarker", 0, 10)
        .await
        .expect("multi-term AND search");
    assert_eq!(multi_term.len(), 1);
    assert_eq!(multi_term[0].id, message.id.as_str());
    assert!(
        repository
            .search(Some(&project.id), "rendezvous absentterm", 0, 10)
            .await
            .expect("missing AND term")
            .is_empty()
    );
    assert!(
        repository
            .search(Some(&project.id), "vous", 0, 10)
            .await
            .expect("whole-token search")
            .is_empty()
    );
    let diacritic = repository
        .search(Some(&project.id), "CAFE", 0, 10)
        .await
        .expect("case and diacritic folding");
    assert_eq!(diacritic.len(), 1);
    assert_eq!(diacritic[0].id, project.id.as_str());
    assert!(
        repository
            .search(Some(&project.id), "privatecachetoken", 0, 10)
            .await
            .expect("artifact storage path is not searchable")
            .is_empty()
    );
    for deleted_query in ["deletedmessagemarker", "deletedartifactmarker"] {
        assert!(
            repository
                .search(Some(&project.id), deleted_query, 0, 10)
                .await
                .expect("deleted entity search")
                .is_empty()
        );
    }

    let scoped = repository
        .search(Some(&project.id), "rendezvous", 0, 10)
        .await
        .expect("project-scoped search");
    assert_eq!(scoped.len(), 5);
    assert!(scoped.iter().all(|hit| hit.project_id == project.id));
    let other_scoped = repository
        .search(Some(&other_project.id), "rendezvous", 0, 10)
        .await
        .expect("other project-scoped search");
    assert_eq!(other_scoped.len(), 1);
    assert_eq!(other_scoped[0].id, other_project.id.as_str());

    let project_hit = scoped
        .iter()
        .find(|hit| hit.kind == WorkspaceSearchKind::Project)
        .expect("project route");
    assert_eq!(project_hit.id, project.id.as_str());
    assert_eq!(project_hit.thread_id, None);
    let thread_hit = scoped
        .iter()
        .find(|hit| hit.kind == WorkspaceSearchKind::Thread)
        .expect("thread route");
    assert_eq!(thread_hit.id, thread.id.as_str());
    assert_eq!(thread_hit.thread_id.as_ref(), Some(&thread.id));
    let message_hit = scoped
        .iter()
        .find(|hit| hit.kind == WorkspaceSearchKind::Message)
        .expect("message route");
    assert_eq!(message_hit.id, message.id.as_str());
    assert_eq!(message_hit.thread_id.as_ref(), Some(&thread.id));
    let artifact_hit = scoped
        .iter()
        .find(|hit| hit.kind == WorkspaceSearchKind::Artifact)
        .expect("artifact route");
    assert_eq!(artifact_hit.id, artifact.id.as_str());
    assert_eq!(artifact_hit.thread_id.as_ref(), Some(&thread.id));
    assert!(artifact_hit.snippet.is_empty());
    let automation_hit = scoped
        .iter()
        .find(|hit| hit.kind == WorkspaceSearchKind::Automation)
        .expect("automation route");
    assert_eq!(automation_hit.id, automation.id.as_str());
    assert_eq!(automation_hit.thread_id, None);

    let first_page = repository
        .search(Some(&project.id), "rendezvous", 0, 2)
        .await
        .expect("first search page");
    let repeated_first_page = repository
        .search(Some(&project.id), "rendezvous", 0, 2)
        .await
        .expect("stable first search page");
    assert_eq!(repeated_first_page, first_page);
    let second_page = repository
        .search(Some(&project.id), "rendezvous", 2, 2)
        .await
        .expect("second search page");
    let third_page = repository
        .search(Some(&project.id), "rendezvous", 4, 2)
        .await
        .expect("third search page");
    let paged = first_page
        .into_iter()
        .chain(second_page)
        .chain(third_page)
        .collect::<Vec<_>>();
    assert_eq!(paged.len(), 5);
    assert_eq!(
        sorted_search_routes(paged),
        sorted_search_routes(scoped.clone())
    );

    sorted_search_routes(scoped)
}

fn privileged_intent(
    kind: PrivilegedOperationKind,
    key: &str,
    request_digest: [u8; 32],
    payload: &[u8],
) -> PrivilegedOperationIntent {
    let vm_id = PrivilegedResourceId::new("work-vm").expect("vm id");
    let target = match kind {
        PrivilegedOperationKind::RunnerHealth => PrivilegedOperationTarget::Runner { vm_id },
        PrivilegedOperationKind::IntegrationStart => PrivilegedOperationTarget::IntegrationStart {
            vm_id,
            integration_id: PrivilegedResourceId::new("wisp").expect("integration id"),
        },
        _ => panic!("test helper supports recovery classifications"),
    };
    PrivilegedOperationIntent::new(
        kind,
        target,
        PayloadDigest::new(Sha256::digest(payload).into()),
        PrivilegedAuthority::new(
            AuthorityGrantId::new("authority-grant-0001").expect("grant id"),
            10_000,
        ),
        PrivilegedIdempotency::new(
            PrivilegedIdempotencyKey::new(key).expect("idempotency key"),
            RequestDigest::new(request_digest),
        ),
        PrivilegedOperationLinks::default(),
    )
}

async fn begin_restart_dispatch(
    service: &PrivilegedOperationService,
    operation: &grok_domain::PrivilegedOperation,
    index: u8,
) {
    service
        .begin_dispatch(BeginPrivilegedDispatch {
            operation_id: operation.id.clone(),
            expected_revision: 0,
            transport_operation_id: format!("restart-transport-{index:04}"),
            wire_digest: [index; 32],
            broker_boot_id: [3; 16],
            guest_boot_id: [4; 16],
            timeout_ms: 1_000,
        })
        .await
        .expect("commit dispatching and attempt");
}

async fn assert_restart_recovery(
    path: &std::path::Path,
    key: Arc<EphemeralKeyProvider>,
    clock: Arc<FixedClock>,
    retry_safe_id: grok_domain::PrivilegedOperationId,
    non_idempotent_id: grok_domain::PrivilegedOperationId,
    payload: Vec<u8>,
) {
    let reopened = open(path, key).await;
    clock.set(120);
    let recovery = PrivilegedOperationService::new(
        reopened.clone(),
        clock.clone(),
        Arc::new(SequentialIdGenerator::new()),
    );
    let first = recovery
        .recover_interrupted(1)
        .await
        .expect("bounded startup recovery");
    assert_eq!(first.retry_pending, 1);
    assert_eq!(first.interrupted_needs_review, 0);
    assert!(first.truncated);
    let second = recovery
        .recover_interrupted(1)
        .await
        .expect("continue bounded startup recovery");
    assert_eq!(second.retry_pending, 0);
    assert_eq!(second.interrupted_needs_review, 1);
    assert!(!second.truncated);
    assert_eq!(
        reopened
            .get_privileged_operation(&retry_safe_id)
            .await
            .expect("retry-safe operation")
            .state,
        PrivilegedOperationState::RetryPending
    );
    assert_eq!(
        reopened
            .get_privileged_operation(&non_idempotent_id)
            .await
            .expect("non-idempotent operation")
            .state,
        PrivilegedOperationState::InterruptedNeedsReview
    );
    assert_eq!(
        recovery
            .recover_interrupted(10)
            .await
            .expect("recovery is retry-safe")
            .recovered(),
        0
    );
    clock.set(20_000);
    recovery_clock_after_authority_expiry(&recovery, &payload, retry_safe_id).await;
}

async fn recovery_clock_after_authority_expiry(
    recovery: &PrivilegedOperationService,
    payload: &[u8],
    retry_safe_id: grok_domain::PrivilegedOperationId,
) {
    let replay = recovery
        .prepare(PreparePrivilegedOperation {
            intent: privileged_intent(
                PrivilegedOperationKind::RunnerHealth,
                "restart-runner-key-0001",
                [1; 32],
                payload,
            ),
            payload: payload.to_vec(),
        })
        .await
        .expect("durable exact replay");
    assert!(!replay.created);
    assert_eq!(replay.operation.id, retry_safe_id);
    assert_eq!(
        replay.operation.state,
        PrivilegedOperationState::RetryPending
    );
    assert!(matches!(
        recovery
            .prepare(PreparePrivilegedOperation {
                intent: privileged_intent(
                    PrivilegedOperationKind::RunnerHealth,
                    "restart-runner-key-0001",
                    [9; 32],
                    payload,
                ),
                payload: payload.to_vec(),
            })
            .await,
        Err(ApplicationError::Conflict)
    ));
}

#[tokio::test]
async fn privileged_dispatch_recovery_survives_sqlcipher_restart_without_replay() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("privileged-recovery.db");
    let key = Arc::new(EphemeralKeyProvider::new([61; 32]));
    let clock = Arc::new(FixedClock::new(100));
    let payload = b"{}".to_vec();

    let store = open(&path, key.clone()).await;
    let service = PrivilegedOperationService::new(
        store.clone(),
        clock.clone(),
        Arc::new(SequentialIdGenerator::new()),
    );
    let retry_safe = service
        .prepare(PreparePrivilegedOperation {
            intent: privileged_intent(
                PrivilegedOperationKind::RunnerHealth,
                "restart-runner-key-0001",
                [1; 32],
                &payload,
            ),
            payload: payload.clone(),
        })
        .await
        .expect("prepare retry-safe operation");
    let non_idempotent = service
        .prepare(PreparePrivilegedOperation {
            intent: privileged_intent(
                PrivilegedOperationKind::IntegrationStart,
                "restart-integration-key-0001",
                [2; 32],
                &payload,
            ),
            payload: payload.clone(),
        })
        .await
        .expect("prepare non-idempotent operation");
    clock.set(110);
    begin_restart_dispatch(&service, &retry_safe.operation, 0).await;
    begin_restart_dispatch(&service, &non_idempotent.operation, 1).await;
    drop(service);
    drop(store);
    assert_restart_recovery(
        &path,
        key,
        clock,
        retry_safe.operation.id,
        non_idempotent.operation.id,
        payload,
    )
    .await;
}

#[tokio::test]
async fn duplicate_transport_identity_rolls_back_dispatching_transition() {
    let directory = tempfile::tempdir().expect("temporary database directory");
    let store = open(
        &directory.path().join("privileged-attempt-atomicity.db"),
        Arc::new(EphemeralKeyProvider::new([62; 32])),
    )
    .await;
    let clock = Arc::new(FixedClock::new(100));
    let service = PrivilegedOperationService::new(
        store.clone(),
        clock.clone(),
        Arc::new(SequentialIdGenerator::new()),
    );
    let payload = b"{}".to_vec();
    let mut prepared = Vec::new();
    for (key, digest) in [
        ("atomic-runner-key-0001", [1; 32]),
        ("atomic-runner-key-0002", [2; 32]),
    ] {
        prepared.push(
            service
                .prepare(PreparePrivilegedOperation {
                    intent: privileged_intent(
                        PrivilegedOperationKind::RunnerHealth,
                        key,
                        digest,
                        &payload,
                    ),
                    payload: payload.clone(),
                })
                .await
                .expect("prepare operation")
                .operation,
        );
    }
    let mut cancelled = prepared[1].clone();
    cancelled.cancel(101).expect("cancel direct adapter input");
    assert!(
        store
            .prepare_with_payload(cancelled, payload.clone())
            .await
            .is_err()
    );
    clock.set(110);
    for (index, operation) in prepared.iter().enumerate() {
        let result = service
            .begin_dispatch(BeginPrivilegedDispatch {
                operation_id: operation.id.clone(),
                expected_revision: 0,
                transport_operation_id: "shared-transport-0001".into(),
                wire_digest: [u8::try_from(index).expect("small index"); 32],
                broker_boot_id: [3; 16],
                guest_boot_id: [4; 16],
                timeout_ms: 1_000,
            })
            .await;
        if index == 0 {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(ApplicationError::Conflict)));
        }
    }
    let rolled_back = store
        .get_privileged_operation(&prepared[1].id)
        .await
        .expect("second operation");
    assert_eq!(rolled_back.state, PrivilegedOperationState::Prepared);
    assert_eq!(rolled_back.revision, 0);
    assert_eq!(rolled_back.attempt_count, 0);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn database_lock_and_wal_sidecars_are_private() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("temporary database directory");
    let path = directory.path().join("workspace.db");
    let lock_path = directory.path().join("workspace.db.lock");
    for existing in [&path, &lock_path] {
        std::fs::write(existing, []).expect("existing database fixture");
        std::fs::set_permissions(existing, std::fs::Permissions::from_mode(0o666))
            .expect("make fixture permissive");
    }

    let store = open(&path, Arc::new(EphemeralKeyProvider::new([47; 32]))).await;
    let (runs, _) = services(store, Arc::new(FixedClock::new(10)));
    runs.create(
        CreateRun {
            project_id: "private-project".into(),
            thread_id: "private-thread".into(),
        },
        "private-database-files",
    )
    .await
    .expect("write through WAL");

    for candidate in [
        path.clone(),
        path.with_file_name("workspace.db-wal"),
        path.with_file_name("workspace.db-shm"),
        lock_path,
    ] {
        let mode = std::fs::metadata(&candidate)
            .unwrap_or_else(|error| panic!("{} must exist: {error}", candidate.display()))
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "{} is accessible outside its owner",
            candidate.display()
        );
    }
}

fn services(store: Arc<SqlCipherStore>, clock: Arc<FixedClock>) -> (RunService, SideEffectService) {
    let store: Arc<dyn ExecutionStore> = store;
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
    (
        RunService::new(store.clone(), clock.clone(), ids.clone()),
        SideEffectService::new(store, clock, ids),
    )
}

async fn running_run(runs: &RunService) -> grok_domain::Run {
    let mut run = runs
        .create(
            CreateRun {
                project_id: "project-1".into(),
                thread_id: "thread-1".into(),
            },
            "create-running-run",
        )
        .await
        .expect("create run");
    run = runs
        .transition(
            &run.id,
            run.revision,
            RunState::Planning,
            "plan-running-run",
        )
        .await
        .expect("planning");
    runs.transition(
        &run.id,
        run.revision,
        RunState::Running,
        "start-running-run",
    )
    .await
    .expect("running")
}

struct PendingApprovalFixture {
    execution: Arc<dyn ExecutionStore>,
    approvals: ApprovalService,
    running: Run,
    pending: Approval,
}

async fn pending_approval_fixture(
    store: Arc<SqlCipherStore>,
    clock: Arc<FixedClock>,
    expires_at: u64,
    request_key: &str,
) -> PendingApprovalFixture {
    let execution: Arc<dyn ExecutionStore> = store;
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
    let runs = RunService::new(execution.clone(), clock.clone(), ids.clone());
    let approvals = ApprovalService::new(execution.clone(), clock.clone(), ids);
    let running = running_run(&runs).await;
    clock.set(13);
    let pending = approvals
        .request(
            RequestApproval {
                run_id: running.id.clone(),
                expected_run_revision: running.revision,
                action: RequestedAction {
                    action: "filesystem.write".into(),
                    target: "report.md".into(),
                    data_summary: "generated report".into(),
                    risk: ApprovalRisk::Elevated,
                },
                scope: ApprovalScope::Once,
                expires_at,
            },
            request_key,
        )
        .await
        .expect("request approval");
    assert_eq!(
        execution
            .get_run(&running.id)
            .await
            .expect("awaiting approval run")
            .state,
        RunState::AwaitingApproval
    );
    PendingApprovalFixture {
        execution,
        approvals,
        running,
        pending,
    }
}

async fn assert_paused_approval_snapshot(
    execution: &Arc<dyn ExecutionStore>,
    running: &Run,
    approval: &Approval,
) {
    assert_eq!(
        execution
            .get_run(&running.id)
            .await
            .expect("persisted paused run")
            .state,
        RunState::Paused
    );
    assert_eq!(
        execution
            .get_approval(&approval.id)
            .await
            .expect("persisted decided approval"),
        *approval
    );
    assert_eq!(
        execution
            .events_since(&running.id, 0, 100)
            .await
            .expect("persisted decision events")
            .len(),
        6
    );
}

struct ExecutionSnapshots {
    create: CreateRun,
    created: Run,
    planned: Run,
    request: RequestApproval,
    pending: Approval,
    granted: Approval,
}

async fn commit_execution_mutations(
    store: Arc<SqlCipherStore>,
    clock: Arc<FixedClock>,
) -> ExecutionSnapshots {
    let execution: Arc<dyn ExecutionStore> = store;
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
    let runs = RunService::new(execution.clone(), clock.clone(), ids.clone());
    let approvals = ApprovalService::new(execution, clock.clone(), ids);
    let create = CreateRun {
        project_id: "project-1".into(),
        thread_id: "thread-1".into(),
    };
    let created = runs
        .create(create.clone(), "durable-create")
        .await
        .expect("create");
    clock.set(11);
    let planned = runs
        .transition(&created.id, 0, RunState::Planning, "durable-plan")
        .await
        .expect("plan");
    clock.set(12);
    let running = runs
        .transition(&created.id, 1, RunState::Running, "durable-start")
        .await
        .expect("start");
    clock.set(13);
    let request = RequestApproval {
        run_id: running.id.clone(),
        expected_run_revision: running.revision,
        action: RequestedAction {
            action: "filesystem.write".into(),
            target: "report.md".into(),
            data_summary: "generated report".into(),
            risk: ApprovalRisk::Elevated,
        },
        scope: ApprovalScope::Once,
        expires_at: 100,
    };
    let pending = approvals
        .request(request.clone(), "durable-approval-request")
        .await
        .expect("request approval");
    clock.set(14);
    let granted = approvals
        .decide(
            &pending.id,
            pending.revision,
            ApprovalDecision::Grant,
            "durable-approval-decision",
        )
        .await
        .expect("grant");
    ExecutionSnapshots {
        create,
        created,
        planned,
        request,
        pending,
        granted,
    }
}

async fn assert_execution_replays(
    store: Arc<SqlCipherStore>,
    clock: Arc<FixedClock>,
    snapshots: ExecutionSnapshots,
) {
    let execution: Arc<dyn ExecutionStore> = store.clone();
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
    let runs = RunService::new(execution.clone(), clock.clone(), ids.clone());
    let approvals = ApprovalService::new(execution, clock, ids);
    assert_eq!(
        runs.create(snapshots.create, "durable-create")
            .await
            .expect("replay create"),
        snapshots.created
    );
    assert_eq!(
        runs.transition(&snapshots.created.id, 0, RunState::Planning, "durable-plan",)
            .await
            .expect("replay plan"),
        snapshots.planned
    );
    assert_eq!(
        approvals
            .request(snapshots.request, "durable-approval-request")
            .await
            .expect("replay request"),
        snapshots.pending
    );
    assert_eq!(
        approvals
            .decide(
                &snapshots.pending.id,
                snapshots.pending.revision,
                ApprovalDecision::Grant,
                "durable-approval-decision",
            )
            .await
            .expect("replay decision"),
        snapshots.granted
    );
    assert!(matches!(
        runs.create(
            CreateRun {
                project_id: "project-1".into(),
                thread_id: "different-thread".into(),
            },
            "durable-create",
        )
        .await,
        Err(ApplicationError::Conflict)
    ));
    assert_eq!(
        store
            .events_since(&snapshots.created.id, 0, 100)
            .await
            .expect("events")
            .len(),
        6
    );
}

#[tokio::test]
async fn restart_preserves_state_and_ordered_event_cursor() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.db");
    let key = Arc::new(EphemeralKeyProvider::new([42; 32]));
    let clock = Arc::new(FixedClock::new(10));
    let store = open(&path, key.clone()).await;
    let (runs, _) = services(store.clone(), clock);
    let run = running_run(&runs).await;
    let events = runs.events_since(&run.id, 0, 100).await.expect("events");
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].sequence, 1);
    assert_eq!(events[2].sequence, 3);
    drop(runs);
    drop(store);

    let reopened = open(&path, key).await;
    let loaded = reopened.get_run(&run.id).await.expect("persisted run");
    assert_eq!(loaded.state, RunState::Running);
    let resumed = reopened
        .events_since(&run.id, 1, 100)
        .await
        .expect("resumed cursor");
    assert_eq!(
        resumed
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![2, 3]
    );
}

#[tokio::test]
async fn execution_mutations_replay_exact_committed_results_across_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.db");
    let key = Arc::new(EphemeralKeyProvider::new([43; 32]));
    let clock = Arc::new(FixedClock::new(10));
    let store = open(&path, key.clone()).await;
    let snapshots = commit_execution_mutations(store.clone(), clock.clone()).await;
    drop(store);

    let reopened = open(&path, key).await;
    assert_execution_replays(reopened, clock, snapshots).await;
}

#[tokio::test]
async fn denied_approval_pauses_its_run_and_replays_across_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.db");
    let key = Arc::new(EphemeralKeyProvider::new([46; 32]));
    let clock = Arc::new(FixedClock::new(10));
    let store = open(&path, key.clone()).await;
    let fixture =
        pending_approval_fixture(store.clone(), clock.clone(), 100, "durable-denial-request").await;
    clock.set(14);
    let denied = fixture
        .approvals
        .decide(
            &fixture.pending.id,
            fixture.pending.revision,
            ApprovalDecision::Deny,
            "durable-denial-decision",
        )
        .await
        .expect("deny approval");
    assert_eq!(denied.status, ApprovalStatus::Denied);
    assert_eq!(denied.revision, 1);
    assert_eq!(denied.decided_at, Some(14));
    assert_paused_approval_snapshot(&fixture.execution, &fixture.running, &denied).await;
    let pending = fixture.pending.clone();
    let running = fixture.running.clone();
    drop(fixture);
    drop(store);

    let reopened = open(&path, key).await;
    let execution: Arc<dyn ExecutionStore> = reopened.clone();
    let approvals = ApprovalService::new(
        execution.clone(),
        clock,
        Arc::new(SequentialIdGenerator::new()),
    );
    let replayed = approvals
        .decide(
            &pending.id,
            pending.revision,
            ApprovalDecision::Deny,
            "durable-denial-decision",
        )
        .await
        .expect("replay denial after restart");
    assert_eq!(replayed, denied);
    assert_paused_approval_snapshot(&execution, &running, &denied).await;
}

#[tokio::test]
async fn expired_approval_pauses_its_run_and_replays_error_across_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.db");
    let key = Arc::new(EphemeralKeyProvider::new([47; 32]));
    let clock = Arc::new(FixedClock::new(10));
    let store = open(&path, key.clone()).await;
    let fixture =
        pending_approval_fixture(store.clone(), clock.clone(), 20, "durable-expiry-request").await;
    clock.set(21);
    let first_error = fixture
        .approvals
        .decide(
            &fixture.pending.id,
            fixture.pending.revision,
            ApprovalDecision::Grant,
            "durable-expiry-decision",
        )
        .await
        .expect_err("expired decision must fail closed");
    assert!(matches!(
        &first_error,
        ApplicationError::InvalidState(message) if message == "approval expired at 20"
    ));
    let replay_error = fixture
        .approvals
        .decide(
            &fixture.pending.id,
            fixture.pending.revision,
            ApprovalDecision::Grant,
            "durable-expiry-decision",
        )
        .await
        .expect_err("expired decision replay must return its canonical error");
    assert_eq!(replay_error.to_string(), first_error.to_string());
    let expired = fixture
        .execution
        .get_approval(&fixture.pending.id)
        .await
        .expect("persisted expired approval");
    assert_eq!(expired.status, ApprovalStatus::Expired);
    assert_eq!(expired.revision, 1);
    assert_eq!(expired.decided_at, Some(21));
    assert_paused_approval_snapshot(&fixture.execution, &fixture.running, &expired).await;
    let pending = fixture.pending.clone();
    let running = fixture.running.clone();
    drop(fixture);
    drop(store);

    let reopened = open(&path, key).await;
    let execution: Arc<dyn ExecutionStore> = reopened.clone();
    let approvals = ApprovalService::new(
        execution.clone(),
        clock,
        Arc::new(SequentialIdGenerator::new()),
    );
    let restarted_error = approvals
        .decide(
            &pending.id,
            pending.revision,
            ApprovalDecision::Grant,
            "durable-expiry-decision",
        )
        .await
        .expect_err("replay expired decision after restart");
    assert_eq!(restarted_error.to_string(), first_error.to_string());
    assert_eq!(
        execution
            .get_approval(&pending.id)
            .await
            .expect("restarted expired approval"),
        expired
    );
    assert_paused_approval_snapshot(&execution, &running, &expired).await;
}

#[tokio::test]
async fn credential_mutation_reservations_survive_restart_and_reject_conflicts() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.db");
    let key = Arc::new(EphemeralKeyProvider::new([44; 32]));
    let store = open(&path, key.clone()).await;
    let command = MutationCommand {
        scope: "configure_xai_api_key".into(),
        key: "configure-key-1".into(),
        fingerprint: [7; 32],
    };
    assert_eq!(
        store
            .resolve_credential_mutation(&command)
            .await
            .expect("resolve missing"),
        None
    );
    assert_eq!(
        store
            .begin_credential_mutation(&command)
            .await
            .expect("reserve"),
        CredentialMutationReservation::NewlyReserved
    );
    drop(store);

    let reopened = open(&path, key).await;
    assert_eq!(
        reopened
            .resolve_credential_mutation(&command)
            .await
            .expect("resolve pending"),
        Some(CredentialMutationReservation::Pending)
    );
    assert_eq!(
        reopened
            .begin_credential_mutation(&command)
            .await
            .expect("resume"),
        CredentialMutationReservation::Pending
    );
    let outcome = AccountState {
        xai_api_key_configured: true,
        xai_capabilities_resolved: true,
    };
    reopened
        .complete_credential_mutation(&command, outcome)
        .await
        .expect("complete");
    assert_eq!(
        reopened
            .begin_credential_mutation(&command)
            .await
            .expect("replay"),
        CredentialMutationReservation::Completed(outcome)
    );
    assert_eq!(
        reopened
            .resolve_credential_mutation(&command)
            .await
            .expect("resolve completed"),
        Some(CredentialMutationReservation::Completed(outcome))
    );
    let conflicting = MutationCommand {
        fingerprint: [8; 32],
        ..command
    };
    assert!(matches!(
        reopened.begin_credential_mutation(&conflicting).await,
        Err(grok_application::StoreError::Conflict)
    ));
    assert!(matches!(
        reopened.resolve_credential_mutation(&conflicting).await,
        Err(grok_application::StoreError::Conflict)
    ));
}

#[tokio::test]
async fn conversation_turn_history_survives_restart_with_stable_cursor_order() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.db");
    let key = Arc::new(EphemeralKeyProvider::new([45; 32]));
    let store = open(&path, key.clone()).await;
    let workspace = WorkspaceService::new(
        store.clone(),
        Arc::new(FixedClock::new(1)),
        Arc::new(SequentialIdGenerator::new()),
    );
    let project = workspace
        .create_project(
            CreateProject {
                name: "Conversation history".into(),
                description: String::new(),
            },
            "conversation-history-project",
        )
        .await
        .expect("project");
    let thread = workspace
        .create_thread(
            CreateThread {
                project_id: project.id.to_string(),
                title: "History".into(),
            },
            "conversation-history-thread",
        )
        .await
        .expect("thread");
    let mut ids = Vec::new();
    for index in 1..=3_u8 {
        ids.push(
            reserve_cancelled_conversation_turn(
                &store,
                project.id.clone(),
                thread.id.clone(),
                index,
            )
            .await,
        );
    }
    assert_linked_message_is_immutable(&store, "conversation-history-message-1").await;
    drop(workspace);
    drop(store);

    let reopened = open(&path, key).await;
    let first = reopened
        .list_thread_turns(&thread.id, None, 2)
        .await
        .expect("first page");
    let second = reopened
        .list_thread_turns(&thread.id, Some(&first[1].turn.id), 2)
        .await
        .expect("second page");
    assert_eq!(
        first
            .iter()
            .chain(&second)
            .map(|snapshot| snapshot.turn.id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    assert_eq!(first[0].user_message.sequence, 1);
    assert_eq!(first[1].user_message.sequence, 2);
    assert_eq!(second[0].user_message.sequence, 3);

    assert!(
        reopened
            .load_turn_by_command(&MutationCommand {
                scope: "delete_xai_api_key".into(),
                key: "conversation-history-command-1".into(),
                fingerprint: [1; 32],
            })
            .await
            .expect("scope isolation")
            .is_none()
    );
}

#[tokio::test]
async fn corrupt_conversation_turn_combinations_fail_closed_on_rehydration() {
    let corruptions = [
        ("state without its transition", "state=4"),
        (
            "dispatch evidence in reserved state",
            "provider_request_fingerprint=zeroblob(32)",
        ),
        (
            "provider result in reserved state",
            "provider_response_id='forged-response'",
        ),
        ("revision without its transition", "revision=1"),
        (
            "reserved timestamp advanced without a revision",
            "updated_at=created_at+1",
        ),
    ];

    for (case, assignment) in corruptions {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("corrupt-conversation.db");
        let key = Arc::new(EphemeralKeyProvider::new([46; 32]));
        let store = open(&path, key.clone()).await;
        let workspace = WorkspaceService::new(
            store.clone(),
            Arc::new(FixedClock::new(1)),
            Arc::new(SequentialIdGenerator::new()),
        );
        let project = workspace
            .create_project(
                CreateProject {
                    name: "Conversation corruption fixture".into(),
                    description: String::new(),
                },
                "conversation-corruption-project",
            )
            .await
            .expect("project");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: "Corruption fixture".into(),
                },
                "conversation-corruption-thread",
            )
            .await
            .expect("thread");
        let reservation = reserve_conversation_turn(&store, project.id, thread.id.clone(), 7).await;
        let turn_id = reservation.snapshot.turn.id;
        drop(workspace);
        drop(store);

        let connection = rusqlite::Connection::open(&path).expect("raw encrypted connection");
        connection
            .execute_batch(&format!(
                "PRAGMA key = \"x'{}'\";",
                hex::encode([46_u8; 32])
            ))
            .expect("unlock encrypted fixture");
        connection
            .execute(
                &format!("UPDATE conversation_turns SET {assignment} WHERE id=?1"),
                [turn_id.as_str()],
            )
            .expect("inject structurally valid corrupt turn");
        drop(connection);

        let reopened = open(&path, key).await;
        let by_command = reopened
            .load_turn_by_command(&MutationCommand {
                scope: "execute_conversation_turn".into(),
                key: "conversation-history-command-7".into(),
                fingerprint: [7; 32],
            })
            .await;
        assert!(
            matches!(by_command, Err(StoreError::Internal(_))),
            "corrupt conversation turn was rehydrated by command: {case}"
        );
        let history = reopened.list_thread_turns(&thread.id, None, 10).await;
        assert!(
            matches!(history, Err(StoreError::Internal(_))),
            "corrupt conversation turn was rehydrated from history: {case}"
        );
    }
}

async fn reserve_conversation_turn(
    store: &Arc<SqlCipherStore>,
    project_id: ProjectId,
    thread_id: ThreadId,
    index: u8,
) -> ConversationTurnReservation {
    let created_at = u64::from(index) * 10;
    let user = Message::new(
        MessageId::new(format!("conversation-history-message-{index}")).expect("message id"),
        thread_id.clone(),
        MessageRole::User,
        format!("Message {index}"),
        created_at,
    )
    .expect("message");
    let run = Run::queued(
        RunId::new(format!("conversation-history-run-{index}")).expect("run id"),
        project_id.clone(),
        thread_id.clone(),
        created_at,
    );
    let turn = ConversationTurn::reserve(
        ConversationTurnId::new(format!("conversation-history-turn-{index}")).expect("turn id"),
        format!("conversation-history-command-{index}"),
        [index; 32],
        project_id,
        thread_id,
        user.id.clone(),
        run.id.clone(),
        "grok-4.3".into(),
        created_at,
    )
    .expect("turn");
    store
        .reserve_turn(
            turn,
            ConversationTurnLineage::original("xai-binding-test-generation".into())
                .expect("original lineage"),
            ConversationTurnReservationSource::CurrentThread,
            user,
            run,
            NewRunEvent {
                occurred_at: created_at,
                kind: grok_domain::RunEventKind::Created,
            },
            ConversationTurnEventKind::Created,
        )
        .await
        .expect("reserve turn")
}

async fn reserve_cancelled_conversation_turn(
    store: &Arc<SqlCipherStore>,
    project_id: ProjectId,
    thread_id: ThreadId,
    index: u8,
) -> ConversationTurnId {
    let created_at = u64::from(index) * 10;
    let reservation = reserve_conversation_turn(store, project_id, thread_id, index).await;
    let mut terminal_turn = reservation.snapshot.turn;
    let mut terminal_run = reservation.snapshot.run;
    terminal_turn.cancel(created_at + 1).expect("cancel turn");
    terminal_run
        .transition(RunState::Cancelled, created_at + 1)
        .expect("cancel run");
    let turn_id = terminal_turn.id.clone();
    store
        .commit_terminal(TerminalTurnCommit {
            turn: terminal_turn,
            expected_turn_revision: 0,
            run: terminal_run,
            expected_run_revision: 0,
            effect: None,
            expected_effect_revision: None,
            assistant_message: None,
            events: vec![NewRunEvent {
                occurred_at: created_at + 1,
                kind: grok_domain::RunEventKind::StateChanged {
                    from: RunState::Queued,
                    to: RunState::Cancelled,
                },
            }],
            turn_event: ConversationTurnEventKind::StateChanged {
                from: ConversationTurnState::Reserved,
                to: ConversationTurnState::Cancelled,
            },
        })
        .await
        .expect("commit cancelled turn");
    turn_id
}

async fn assert_linked_message_is_immutable(store: &Arc<SqlCipherStore>, message_id: &str) {
    let workspace = WorkspaceService::new(
        store.clone(),
        Arc::new(FixedClock::new(100)),
        Arc::new(SequentialIdGenerator::new()),
    );
    let message_id = MessageId::new(message_id).expect("message id");
    assert!(matches!(
        workspace
            .update_message(
                UpdateMessage {
                    id: message_id.to_string(),
                    expected_revision: 0,
                    content: "Rewritten history".into(),
                },
                "rewrite-linked-message",
            )
            .await,
        Err(ApplicationError::Conflict)
    ));
    assert!(matches!(
        workspace
            .delete_message(&message_id, 0, "delete-linked-message")
            .await,
        Err(ApplicationError::Conflict)
    ));
}

#[tokio::test]
async fn optimistic_conflict_rolls_back_event_append() {
    let directory = tempfile::tempdir().expect("tempdir");
    let key = Arc::new(EphemeralKeyProvider::new([21; 32]));
    let store = open(&directory.path().join("state.db"), key).await;
    let clock = Arc::new(FixedClock::new(10));
    let (runs, _) = services(store.clone(), clock);
    let run = runs
        .create(
            CreateRun {
                project_id: "project-1".into(),
                thread_id: "thread-1".into(),
            },
            "create-conflict-run",
        )
        .await
        .expect("create");
    runs.transition(&run.id, 0, RunState::Planning, "plan-conflict-run")
        .await
        .expect("first transition");
    assert!(matches!(
        runs.transition(&run.id, 0, RunState::Cancelled, "cancel-stale-run")
            .await,
        Err(ApplicationError::Conflict)
    ));
    let events = store.events_since(&run.id, 0, 100).await.expect("events");
    assert_eq!(events.len(), 2);
}

#[tokio::test]
async fn interrupted_effect_and_run_commit_atomically_across_restart() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.db");
    let key = Arc::new(EphemeralKeyProvider::new([7; 32]));
    let clock = Arc::new(FixedClock::new(10));
    let store = open(&path, key.clone()).await;
    let (runs, effects) = services(store.clone(), clock);
    let run = running_run(&runs).await;
    let effect = effects
        .prepare(PrepareEffect {
            run_id: run.id.clone(),
            kind: EffectKind::ExternalMutation,
            target: "publish report".into(),
            idempotency: Idempotency::NonIdempotent,
        })
        .await
        .expect("prepare");
    let effect = effects
        .start(&effect.id, effect.revision)
        .await
        .expect("start");
    let interrupted = effects
        .interrupt(&effect.id, effect.revision)
        .await
        .expect("interrupt");
    drop(runs);
    drop(effects);
    drop(store);

    let reopened = open(&path, key).await;
    assert_eq!(
        reopened
            .get_effect(&interrupted.id)
            .await
            .expect("effect")
            .state,
        EffectState::NeedsReview
    );
    assert_eq!(
        reopened.get_run(&run.id).await.expect("run").state,
        RunState::InterruptedNeedsReview
    );
    let events = reopened
        .events_since(&run.id, 0, 100)
        .await
        .expect("events");
    assert_eq!(events.len(), 6);
    assert!(
        events
            .windows(2)
            .all(|pair| pair[0].sequence < pair[1].sequence)
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn integrity_and_online_backup_produce_reopenable_snapshot() {
    use std::os::unix::fs::PermissionsExt as _;

    let directory = tempfile::tempdir().expect("tempdir");
    std::fs::set_permissions(directory.path(), std::fs::Permissions::from_mode(0o700))
        .expect("private backup directory");
    let path = directory.path().join("state.db");
    let backup = directory.path().join("backup.db");
    let key = Arc::new(EphemeralKeyProvider::new([99; 32]));
    let clock = Arc::new(FixedClock::new(10));
    let store = open(&path, key.clone()).await;
    let (runs, _) = services(store.clone(), clock);
    let run = running_run(&runs).await;
    assert!(
        store
            .verify_integrity()
            .await
            .expect("integrity")
            .is_healthy()
    );
    store.backup_to(&backup).await.expect("backup");

    let recovered = open(&backup, key).await;
    assert_eq!(
        recovered.get_run(&run.id).await.expect("backup run").state,
        RunState::Running
    );
}

#[tokio::test]
async fn incorrect_key_fails_closed() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("state.db");
    let key = Arc::new(EphemeralKeyProvider::new([11; 32]));
    let store = open(&path, key).await;
    let clock = Arc::new(FixedClock::new(10));
    let (runs, _) = services(store.clone(), clock);
    let _run = running_run(&runs).await;
    drop(runs);
    drop(store);

    let wrong_key = Arc::new(EphemeralKeyProvider::new([12; 32]));
    assert!(SqlCipherStore::open(&path, wrong_key).await.is_err());
}

#[tokio::test]
async fn desktop_preferences_survive_restart_with_exact_idempotent_replay() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("preferences.db");
    let key = Arc::new(EphemeralKeyProvider::new([41; 32]));
    let store = open(&path, key.clone()).await;
    let input = UpdateDesktopPreferences {
        expected_revision: 0,
        keep_running_in_notification_area: false,
    };
    let preferences = DesktopPreferencesService::new(store.clone(), Arc::new(FixedClock::new(10)));
    let updated = preferences
        .update(input, "desktop-preference-restart")
        .await
        .expect("update preference");
    drop(preferences);
    drop(store);

    let reopened = open(&path, key).await;
    let preferences = DesktopPreferencesService::new(reopened, Arc::new(FixedClock::new(20)));
    assert_eq!(
        preferences.get().await.expect("persisted preference"),
        updated
    );
    assert_eq!(
        preferences
            .update(input, "desktop-preference-restart")
            .await
            .expect("exact replay after restart"),
        updated
    );
    assert!(matches!(
        preferences
            .update(
                UpdateDesktopPreferences {
                    keep_running_in_notification_area: true,
                    ..input
                },
                "desktop-preference-restart",
            )
            .await,
        Err(ApplicationError::Conflict)
    ));
}

#[tokio::test]
async fn chat_model_preference_survives_restart_replay_and_conflict() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("chat-model.db");
    let key = Arc::new(EphemeralKeyProvider::new([42; 32]));
    let store = open(&path, key.clone()).await;
    let mut updated = store
        .get_chat_model_preference()
        .await
        .expect("default preference");
    updated
        .select_model("grok-canonical".into(), 10)
        .expect("valid selection");
    let command = MutationCommand {
        scope: "select_chat_model".into(),
        key: "chat-model-restart".into(),
        fingerprint: [1; 32],
    };
    assert_eq!(
        store
            .save_chat_model_preference(updated.clone(), 0, &command)
            .await
            .expect("saved preference"),
        updated
    );
    drop(store);

    let reopened = open(&path, key).await;
    assert_eq!(
        reopened
            .get_chat_model_preference()
            .await
            .expect("persisted preference"),
        updated
    );
    assert_eq!(
        reopened
            .save_chat_model_preference(updated.clone(), 0, &command)
            .await
            .expect("exact replay after restart"),
        updated
    );
    let conflicting_command = MutationCommand {
        fingerprint: [2; 32],
        ..command.clone()
    };
    assert!(matches!(
        reopened
            .resolve_chat_model_preference_mutation(&conflicting_command)
            .await,
        Err(grok_application::StoreError::Conflict)
    ));

    let mut stale = ChatModelPreference::default();
    stale
        .select_model("grok-stale".into(), 20)
        .expect("valid stale candidate");
    assert!(matches!(
        reopened
            .save_chat_model_preference(
                stale,
                0,
                &MutationCommand {
                    scope: "select_chat_model".into(),
                    key: "stale-command".into(),
                    fingerprint: [3; 32],
                },
            )
            .await,
        Err(grok_application::StoreError::Conflict)
    ));
    assert_eq!(
        reopened
            .get_chat_model_preference()
            .await
            .expect("preference preserved after conflict"),
        updated
    );
}

#[tokio::test]
async fn corrupt_persisted_chat_model_identifier_fails_with_sanitized_storage_error() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("chat-model-corrupt.db");
    let key = Arc::new(EphemeralKeyProvider::new([43; 32]));
    let store = open(&path, key.clone()).await;
    drop(store);

    let connection = rusqlite::Connection::open(&path).expect("raw encrypted connection");
    connection
        .execute_batch(&format!(
            "PRAGMA key = \"x'{}'\";",
            hex::encode([43_u8; 32])
        ))
        .expect("unlock encrypted fixture");
    connection
        .execute(
            "UPDATE chat_model_preferences SET selected_model_id=char(10) WHERE singleton=1",
            [],
        )
        .expect("inject structurally bounded corrupt identifier");
    drop(connection);

    let reopened = open(&path, key).await;
    assert!(matches!(
        reopened.get_chat_model_preference().await,
        Err(StoreError::Internal(message))
            if message == "invalid persisted chat model preference"
    ));
}

#[tokio::test]
async fn workspace_search_conforms_across_memory_and_sqlcipher_adapters() {
    let memory_store = Arc::new(InMemoryExecutionStore::new());
    let memory: Arc<dyn WorkspaceStore> = memory_store.clone();
    let memory_artifacts: Arc<dyn ArtifactStore> = memory_store;
    let memory_routes = exercise_workspace_search_conformance(memory, memory_artifacts).await;

    let directory = tempfile::tempdir().expect("tempdir");
    let key = Arc::new(EphemeralKeyProvider::new([51; 32]));
    let sql_store = open(&directory.path().join("search-conformance.db"), key).await;
    let sql: Arc<dyn WorkspaceStore> = sql_store.clone();
    let sql_artifacts: Arc<dyn ArtifactStore> = sql_store;
    let sql_routes = exercise_workspace_search_conformance(sql, sql_artifacts).await;

    assert_eq!(sql_routes, memory_routes);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn workspace_search_excludes_orphaned_and_forged_derived_rows() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("search-derived-cache.db");
    let key = Arc::new(EphemeralKeyProvider::new([52; 32]));
    let store = open(&path, key.clone()).await;
    let repository: Arc<dyn WorkspaceStore> = store.clone();
    let workspace = WorkspaceService::new(
        repository.clone(),
        Arc::new(FixedClock::new(10)),
        Arc::new(SequentialIdGenerator::new()),
    );
    let canonical_project = workspace
        .create_project(
            CreateProject {
                name: "Canonical project".into(),
                description: String::new(),
            },
            "forged-cache-project",
        )
        .await
        .expect("canonical project");
    let forged_scope = workspace
        .create_project(
            CreateProject {
                name: "Separate project".into(),
                description: String::new(),
            },
            "forged-cache-other-project",
        )
        .await
        .expect("separate project");
    let thread = workspace
        .create_thread(
            CreateThread {
                project_id: canonical_project.id.to_string(),
                title: "Canonical thread".into(),
            },
            "forged-cache-thread",
        )
        .await
        .expect("canonical thread");
    let message = workspace
        .create_message(
            CreateMessage {
                thread_id: thread.id.to_string(),
                role: MessageRole::User,
                content: "canonicalmessagebody".into(),
            },
            "forged-cache-message",
        )
        .await
        .expect("canonical message");
    let artifact = commit_test_artifact(
        store.as_ref(),
        "forged-cache-artifact",
        canonical_project.id.clone(),
        Some(thread.id.clone()),
        "Visible evidence.txt",
        12,
        10,
    )
    .await;
    drop(workspace);
    drop(repository);
    drop(store);

    let connection = rusqlite::Connection::open(&path).expect("raw encrypted connection");
    connection
        .execute_batch(&format!(
            "PRAGMA key = \"x'{}'\";",
            hex::encode([52_u8; 32])
        ))
        .expect("unlock encrypted fixture");
    connection
        .execute(
            "UPDATE search_documents
             SET project_id=?2,title='Forged title',body='forgedbodymarker',updated_at=999
             WHERE id=?1",
            rusqlite::params![message.id.as_str(), forged_scope.id.as_str()],
        )
        .expect("forge derived message fields");
    connection
        .execute(
            "INSERT INTO search_documents(id,project_id,kind,title,body,updated_at)
             VALUES ('orphan-search-row',?1,'message','Orphan','orphanbodymarker',999)",
            [forged_scope.id.as_str()],
        )
        .expect("orphaned derived row");
    connection
        .execute(
            "UPDATE search_documents SET body=?2 WHERE id=?1",
            rusqlite::params![artifact.id.as_str(), "privatecachetoken/evidence.txt"],
        )
        .expect("index legacy artifact path");
    connection
        .execute_batch("DROP TRIGGER search_documents_au;")
        .expect("disable external-content update synchronization");
    connection
        .execute(
            "UPDATE search_documents SET body='' WHERE id=?1",
            [artifact.id.as_str()],
        )
        .expect("desynchronize external artifact body");
    connection
        .execute_batch(
            "CREATE TRIGGER search_documents_au AFTER UPDATE ON search_documents BEGIN
                 INSERT INTO search_documents_fts(search_documents_fts,rowid,title,body)
                 VALUES ('delete',old.rowid,old.title,old.body);
                 INSERT INTO search_documents_fts(rowid,title,body)
                 VALUES (new.rowid,new.title,new.body);
             END;",
        )
        .expect("restore external-content update synchronization");
    drop(connection);

    let reopened = open(&path, key).await;
    for (scope, query) in [
        (None, "forgedbodymarker"),
        (Some(&canonical_project.id), "forgedbodymarker"),
        (Some(&forged_scope.id), "forgedbodymarker"),
        (None, "orphanbodymarker"),
        (Some(&forged_scope.id), "orphanbodymarker"),
        (Some(&canonical_project.id), "canonicalmessagebody"),
        (Some(&canonical_project.id), "privatecachetoken"),
    ] {
        assert!(
            reopened
                .search(scope, query, 0, 10)
                .await
                .expect("fail-closed derived cache search")
                .is_empty(),
            "derived cache corruption surfaced for query {query}"
        );
    }
    let canonical_project_hit = reopened
        .search(Some(&canonical_project.id), "canonical project", 0, 10)
        .await
        .expect("unmodified canonical row remains searchable");
    assert_eq!(canonical_project_hit.len(), 1);
    assert_eq!(canonical_project_hit[0].id, canonical_project.id.as_str());
    let artifact_hit = reopened
        .search(Some(&canonical_project.id), "visible evidence", 0, 10)
        .await
        .expect("artifact title remains searchable");
    assert_eq!(artifact_hit.len(), 1);
    assert_eq!(artifact_hit[0].id, artifact.id.as_str());
    assert!(artifact_hit[0].snippet.is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn workspace_restart_preserves_order_search_and_idempotency() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("workspace.db");
    let key = Arc::new(EphemeralKeyProvider::new([31; 32]));
    let clock = Arc::new(FixedClock::new(10));
    let store = open(&path, key.clone()).await;
    let repository: Arc<dyn WorkspaceStore> = store.clone();
    let workspace = WorkspaceService::new(
        repository,
        clock.clone(),
        Arc::new(SequentialIdGenerator::new()),
    );
    let project_input = CreateProject {
        name: "Mars research".into(),
        description: "Launch systems and surface science".into(),
    };
    let project = workspace
        .create_project(project_input.clone(), "create-project")
        .await
        .expect("project");
    let replay = workspace
        .create_project(project_input, "create-project")
        .await
        .expect("idempotent replay");
    assert_eq!(replay.id, project.id);
    assert!(matches!(
        workspace
            .create_project(
                CreateProject {
                    name: "Different request".into(),
                    description: String::new(),
                },
                "create-project",
            )
            .await,
        Err(ApplicationError::Conflict)
    ));
    let thread = workspace
        .create_thread(
            CreateThread {
                project_id: project.id.to_string(),
                title: "Launch plan".into(),
            },
            "create-thread",
        )
        .await
        .expect("thread");
    for (key, content) in [
        ("message-one", "First orbital launch note"),
        ("message-two", "Second surface science note"),
    ] {
        workspace
            .create_message(
                CreateMessage {
                    thread_id: thread.id.to_string(),
                    role: MessageRole::User,
                    content: content.into(),
                },
                key,
            )
            .await
            .expect("message");
    }
    let messages = workspace
        .list_messages(&thread.id, None, 10)
        .await
        .expect("messages");
    assert_eq!(messages.items[0].sequence, 1);
    assert_eq!(messages.items[1].sequence, 2);
    let hits = workspace
        .search(Some(&project.id), "surface science", 0, 10)
        .await
        .expect("search");
    assert!(hits.items.len() >= 2);
    assert!(hits.items.iter().any(|hit| hit.id.starts_with("message-")));
    assert!(
        hits.items
            .iter()
            .filter(|hit| hit.id.starts_with("message-"))
            .all(|hit| { hit.thread_id.as_ref() == Some(&thread.id) })
    );

    drop(workspace);
    drop(store);
    let reopened = open(&path, key).await;
    let repository: Arc<dyn WorkspaceStore> = reopened.clone();
    let workspace =
        WorkspaceService::new(repository, clock, Arc::new(SequentialIdGenerator::new()));
    assert_eq!(
        workspace
            .get_project(&project.id)
            .await
            .expect("restarted project")
            .name,
        "Mars research"
    );
    let updated = workspace
        .update_project(
            UpdateProject {
                id: project.id.to_string(),
                expected_revision: 0,
                name: "Mars program".into(),
                description: "Launch systems and surface science".into(),
            },
            "update-project",
        )
        .await
        .expect("update");
    let replay = workspace
        .update_project(
            UpdateProject {
                id: project.id.to_string(),
                expected_revision: 0,
                name: "Mars program".into(),
                description: "Launch systems and surface science".into(),
            },
            "update-project",
        )
        .await
        .expect("update replay");
    assert_eq!(replay.revision, updated.revision);
    assert!(matches!(
        workspace
            .update_project(
                UpdateProject {
                    id: project.id.to_string(),
                    expected_revision: 0,
                    name: "Stale update".into(),
                    description: String::new(),
                },
                "stale-update",
            )
            .await,
        Err(ApplicationError::Conflict)
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn automation_policy_history_and_database_lock_are_durable() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("workspace.db");
    let key = Arc::new(EphemeralKeyProvider::new([41; 32]));
    let clock = Arc::new(FixedClock::new(20));
    let store = open(&path, key.clone()).await;
    assert!(matches!(
        SqlCipherStore::open(&path, key.clone()).await,
        Err(grok_sqlcipher::SqlCipherStoreError::DatabaseInUse)
    ));
    let repository: Arc<dyn WorkspaceStore> = store.clone();
    let workspace =
        WorkspaceService::new(repository, clock, Arc::new(SequentialIdGenerator::new()));
    let project = workspace
        .create_project(
            CreateProject {
                name: "Operations".into(),
                description: String::new(),
            },
            "project",
        )
        .await
        .expect("project");
    let automation = workspace
        .create_automation(
            CreateAutomation {
                project_id: project.id.to_string(),
                title: "Daily brief".into(),
                prompt: "Summarize launch readiness".into(),
                schedule: "0 9 * * *".into(),
                timezone: "Europe/Paris".into(),
                missed_run_policy: MissedRunPolicy::RunOnce,
                overlap_policy: OverlapPolicy::QueueOne,
                enabled: false,
            },
            "automation",
        )
        .await
        .expect("automation");
    let first = workspace
        .record_automation_history(
            automation.id.clone(),
            20,
            AutomationHistoryStatus::Succeeded,
            "Completed".into(),
        )
        .await
        .expect("history");
    let replay = workspace
        .record_automation_history(
            automation.id.clone(),
            20,
            AutomationHistoryStatus::Succeeded,
            "Completed".into(),
        )
        .await
        .expect("history replay");
    assert_eq!(replay, first);
    assert!(matches!(
        workspace
            .record_automation_history(
                automation.id.clone(),
                20,
                AutomationHistoryStatus::Failed,
                "Conflicting duplicate".into(),
            )
            .await,
        Err(ApplicationError::Conflict)
    ));
    assert_eq!(
        workspace
            .automation_history(&automation.id, 0, 10)
            .await
            .expect("ordered history")
            .len(),
        1
    );
    drop(workspace);
    drop(store);
    let reopened = open(&path, key).await;
    assert!(
        reopened
            .get_project(&ProjectId::new(project.id.to_string()).expect("id"))
            .await
            .is_ok()
    );
    let orphan = Thread::new(
        ThreadId::new("thread-orphan").expect("id"),
        ProjectId::new("project-missing").expect("id"),
        "Orphan".into(),
        20,
    )
    .expect("thread");
    assert!(matches!(
        reopened
            .create_thread(
                orphan,
                &MutationCommand {
                    scope: "create_thread".into(),
                    key: "orphan".into(),
                    fingerprint: [9; 32],
                },
            )
            .await,
        Err(grok_application::StoreError::NotFound)
    ));
}
