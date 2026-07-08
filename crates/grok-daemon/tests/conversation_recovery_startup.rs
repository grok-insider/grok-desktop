//! Process-level startup recovery boundary coverage for direct conversations.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    path::Path,
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use grok_application::{
    ConversationTurnReservationSource, ConversationTurnStore, CreateProject, CreateThread,
    MAX_CONVERSATION_RECOVERY_BATCH, MutationCommand, NewRunEvent, ProviderStartCommit,
    WorkspaceService,
};
use grok_domain::{
    ConversationTurn, ConversationTurnEventKind, ConversationTurnLineage, ConversationTurnState,
    EffectId, EffectKind, EffectState, Idempotency, Message, MessageId, MessageRole, Run,
    RunEventKind, RunId, RunState, SideEffect,
};
use grok_memory::{EphemeralKeyProvider, FixedClock, SequentialIdGenerator};
use grok_sqlcipher::SqlCipherStore;
use tokio::{io::AsyncWriteExt, net::TcpStream, process::Command, time::Instant};

const DATABASE_KEY: [u8; 32] = [67; 32];
const STARTUP_NONCE: [u8; 32] = [9; 32];
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InitialState {
    Reserved,
    ProviderStarted,
}

#[derive(Clone, Debug)]
struct SeededTurn {
    command_key: String,
    request_fingerprint: [u8; 32],
    provider_request_fingerprint: Option<[u8; 32]>,
    initial_state: InitialState,
    effect_id: Option<EffectId>,
}

async fn open_store(path: &Path) -> Arc<SqlCipherStore> {
    Arc::new(
        SqlCipherStore::open(path, Arc::new(EphemeralKeyProvider::new(DATABASE_KEY)))
            .await
            .expect("open conversation recovery fixture"),
    )
}

#[allow(clippy::too_many_lines)]
async fn seed_incomplete_turns(path: &Path, count: usize) -> Vec<SeededTurn> {
    let store = open_store(path).await;
    let workspace = WorkspaceService::new(
        store.clone(),
        Arc::new(FixedClock::new(100)),
        Arc::new(SequentialIdGenerator::new()),
    );
    let project = workspace
        .create_project(
            CreateProject {
                name: "Conversation recovery".into(),
                description: String::new(),
            },
            "conversation-recovery-project",
        )
        .await
        .expect("create recovery project");
    let mut seeded = Vec::with_capacity(count);
    for index in 0..count {
        let marker = u8::try_from(index + 1).expect("bounded recovery fixture marker");
        let thread = workspace
            .create_thread(
                CreateThread {
                    project_id: project.id.to_string(),
                    title: format!("Recovery turn {index:04}"),
                },
                &format!("conversation-recovery-thread-{index:04}"),
            )
            .await
            .expect("create recovery thread");
        let created_at = 1_000 + u64::try_from(index).expect("fixture timestamp");
        let command_key = format!("conversation-recovery-command-{index:04}");
        let request_fingerprint = [marker; 32];
        let user = Message::new(
            MessageId::new(format!("conversation-recovery-message-{index:04}"))
                .expect("message ID"),
            thread.id.clone(),
            MessageRole::User,
            format!("Recover turn {index}"),
            created_at,
        )
        .expect("user message");
        let run = Run::queued(
            RunId::new(format!("conversation-recovery-run-{index:04}")).expect("run ID"),
            project.id.clone(),
            thread.id.clone(),
            created_at,
        );
        let turn = ConversationTurn::reserve(
            grok_domain::ConversationTurnId::new(format!("conversation-recovery-turn-{index:04}"))
                .expect("turn ID"),
            command_key.clone(),
            request_fingerprint,
            project.id.clone(),
            thread.id,
            user.id.clone(),
            run.id.clone(),
            "grok-4.3".into(),
            created_at,
        )
        .expect("reserve turn aggregate");
        let reservation = store
            .reserve_turn(
                turn,
                ConversationTurnLineage::original("xai-binding-recovery".into()).expect("lineage"),
                ConversationTurnReservationSource::CurrentThread,
                user,
                run,
                NewRunEvent {
                    occurred_at: created_at,
                    kind: RunEventKind::Created,
                },
                ConversationTurnEventKind::Created,
            )
            .await
            .expect("persist turn reservation");

        if index % 2 == 0 {
            seeded.push(SeededTurn {
                command_key,
                request_fingerprint,
                provider_request_fingerprint: None,
                initial_state: InitialState::Reserved,
                effect_id: None,
            });
            continue;
        }

        let started_at = created_at + 1;
        let mut turn = reservation.snapshot.turn;
        let mut run = reservation.snapshot.run;
        let mut effect = SideEffect::prepare(
            EffectId::new(format!("conversation-recovery-effect-{index:04}")).expect("effect ID"),
            run.id.clone(),
            EffectKind::ExternalMutation,
            format!("official xAI Responses API model {}", turn.model_id),
            Idempotency::NonIdempotent,
            started_at,
        );
        effect.start(started_at).expect("start provider effect");
        run.transition(RunState::Planning, started_at)
            .expect("plan provider run");
        run.transition(RunState::Running, started_at)
            .expect("start provider run");
        let provider_request_fingerprint = [marker ^ 0xa5; 32];
        turn.start_provider(effect.id.clone(), provider_request_fingerprint, started_at)
            .expect("record provider boundary");
        let effect_id = effect.id.clone();
        store
            .commit_provider_start(ProviderStartCommit {
                turn,
                expected_turn_revision: 0,
                run,
                expected_run_revision: 0,
                effect,
                events: vec![
                    NewRunEvent {
                        occurred_at: started_at,
                        kind: RunEventKind::StateChanged {
                            from: RunState::Queued,
                            to: RunState::Planning,
                        },
                    },
                    NewRunEvent {
                        occurred_at: started_at,
                        kind: RunEventKind::StateChanged {
                            from: RunState::Planning,
                            to: RunState::Running,
                        },
                    },
                    NewRunEvent {
                        occurred_at: started_at,
                        kind: RunEventKind::EffectPrepared {
                            effect_id: effect_id.clone(),
                        },
                    },
                ],
                turn_event: ConversationTurnEventKind::StateChanged {
                    from: ConversationTurnState::Reserved,
                    to: ConversationTurnState::ProviderStarted,
                },
            })
            .await
            .expect("persist provider-started turn");
        seeded.push(SeededTurn {
            command_key,
            request_fingerprint,
            provider_request_fingerprint: Some(provider_request_fingerprint),
            initial_state: InitialState::ProviderStarted,
            effect_id: Some(effect_id),
        });
    }
    drop(workspace);
    drop(store);
    seeded
}

fn reserve_loopback_address() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("reserve loopback address");
    let address = listener.local_addr().expect("reserved address");
    drop(listener);
    address
}

fn daemon_command(path: &Path, address: SocketAddr) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_grok-daemon"));
    for variable in [
        "GROK_DAEMON_EPHEMERAL",
        "GROK_DAEMON_STARTUP_NONCE_HEX",
        "GROK_ACP_EXECUTABLE",
        "GROK_ACP_VERSION",
        "GROK_ACP_SHA256",
        "GROK_ACP_WORKSPACE_ROOTS",
    ] {
        command.env_remove(variable);
    }
    command
        .env("GROK_DATABASE_PATH", path)
        .env("GROK_DATABASE_KEY_HEX", hex::encode(DATABASE_KEY))
        .env("GROK_DAEMON_DEV_TCP_ADDR", address.to_string())
        .env("GROK_DAEMON_STARTUP_NONCE_STDIN", "1")
        .env("GROK_INSTALLATION_ID", "conversation-startup-recovery-test")
        .env("RUST_LOG", "info")
        .stdin(Stdio::piped());
    command
}

async fn spawn_daemon(mut command: Command) -> tokio::process::Child {
    let mut child = command.spawn().expect("launch daemon");
    let mut stdin = child.stdin.take().expect("daemon nonce stdin");
    stdin
        .write_all(&STARTUP_NONCE)
        .await
        .expect("write exact daemon nonce");
    drop(stdin);
    child
}

async fn wait_until_serving(child: &mut tokio::process::Child, address: SocketAddr) {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Ok(stream) = TcpStream::connect(address).await {
            drop(stream);
            return;
        }
        if let Some(status) = child.try_wait().expect("poll daemon") {
            panic!("daemon exited before serving IPC: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not begin serving IPC before the startup deadline"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn stop_daemon(mut child: tokio::process::Child) {
    child.start_kill().expect("stop daemon");
    tokio::time::timeout(STARTUP_TIMEOUT, child.wait())
        .await
        .expect("daemon stops promptly")
        .expect("wait for daemon");
}

async fn assert_recovery_state(path: &Path, seeded: &[SeededTurn], terminal_count: usize) {
    let store = open_store(path).await;
    let mut observed_terminal = 0;
    for expected in seeded {
        let snapshot = store
            .load_turn_by_command(&MutationCommand {
                scope: "execute_conversation_turn".into(),
                key: expected.command_key.clone(),
                fingerprint: expected.request_fingerprint,
            })
            .await
            .expect("load recovered turn")
            .expect("seeded turn exists");
        assert_eq!(
            snapshot.turn.provider_request_fingerprint,
            expected.provider_request_fingerprint
        );
        assert_eq!(snapshot.turn.effect_id, expected.effect_id);
        assert!(snapshot.assistant_message.is_none());
        match (expected.initial_state, snapshot.turn.state) {
            (InitialState::Reserved, ConversationTurnState::Reserved) => {
                assert_eq!(snapshot.turn.revision, 0);
                assert_eq!(snapshot.run.state, RunState::Queued);
                assert_eq!(snapshot.run.revision, 0);
                assert!(snapshot.effect.is_none());
            }
            (InitialState::Reserved, ConversationTurnState::Cancelled) => {
                observed_terminal += 1;
                assert_eq!(snapshot.turn.revision, 1);
                assert_eq!(snapshot.run.state, RunState::Cancelled);
                assert_eq!(snapshot.run.revision, 1);
                assert!(snapshot.effect.is_none());
            }
            (InitialState::ProviderStarted, ConversationTurnState::ProviderStarted) => {
                assert_eq!(snapshot.turn.revision, 1);
                assert_eq!(snapshot.run.state, RunState::Running);
                assert_eq!(snapshot.run.revision, 2);
                let effect = snapshot.effect.expect("provider effect");
                assert_eq!(Some(&effect.id), expected.effect_id.as_ref());
                assert_eq!(effect.state, EffectState::Executing);
                assert_eq!(effect.revision, 1);
            }
            (InitialState::ProviderStarted, ConversationTurnState::InterruptedNeedsReview) => {
                observed_terminal += 1;
                assert_eq!(snapshot.turn.revision, 2);
                assert_eq!(snapshot.run.state, RunState::InterruptedNeedsReview);
                assert_eq!(snapshot.run.revision, 3);
                let effect = snapshot.effect.expect("recovered provider effect");
                assert_eq!(Some(&effect.id), expected.effect_id.as_ref());
                assert_eq!(effect.state, EffectState::NeedsReview);
                assert_eq!(effect.revision, 2);
            }
            (initial, actual) => {
                panic!("unexpected recovery transition from {initial:?} to {actual:?}")
            }
        }
    }
    assert_eq!(observed_terminal, terminal_count);
}

#[tokio::test]
async fn exact_conversation_recovery_bound_serves_without_provider_redispatch() {
    let directory = tempfile::tempdir().expect("temporary daemon database directory");
    let path = directory.path().join("conversation-recovery-max.db");
    let seeded = seed_incomplete_turns(&path, MAX_CONVERSATION_RECOVERY_BATCH).await;
    let address = reserve_loopback_address();

    let mut command = daemon_command(&path, address);
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut daemon = spawn_daemon(command).await;
    wait_until_serving(&mut daemon, address).await;
    stop_daemon(daemon).await;

    assert_recovery_state(&path, &seeded, MAX_CONVERSATION_RECOVERY_BATCH).await;
}

#[tokio::test]
async fn oversized_conversation_recovery_blocks_ipc_then_later_startup_continues() {
    let directory = tempfile::tempdir().expect("temporary daemon database directory");
    let path = directory.path().join("conversation-recovery-overflow.db");
    let seeded = seed_incomplete_turns(&path, MAX_CONVERSATION_RECOVERY_BATCH + 1).await;
    let address = reserve_loopback_address();

    let mut first_command = daemon_command(&path, address);
    first_command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let first_child = spawn_daemon(first_command).await;
    let first = tokio::time::timeout(STARTUP_TIMEOUT, first_child.wait_with_output())
        .await
        .expect("oversized startup must fail promptly")
        .expect("launch first daemon");
    assert!(!first.status.success());
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        diagnostics.contains("conversation recovery backlog exceeds the bounded startup pass"),
        "unexpected daemon diagnostics: {diagnostics}"
    );
    assert!(!diagnostics.contains("daemon development transport ready"));
    assert!(TcpStream::connect(address).await.is_err());

    assert_recovery_state(&path, &seeded, MAX_CONVERSATION_RECOVERY_BATCH).await;

    let mut second_command = daemon_command(&path, address);
    second_command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut second = spawn_daemon(second_command).await;
    wait_until_serving(&mut second, address).await;
    stop_daemon(second).await;

    assert_recovery_state(&path, &seeded, MAX_CONVERSATION_RECOVERY_BATCH + 1).await;
}
