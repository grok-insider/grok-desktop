//! Process-level startup recovery boundary coverage for the privileged journal.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    process::Stdio,
    sync::Arc,
    time::Duration,
};

use grok_application::{
    BeginPrivilegedDispatch, MAX_PRIVILEGED_RECOVERY_BATCH, PreparePrivilegedOperation,
    PrivilegedOperationService, PrivilegedOperationStore,
};
use grok_domain::{
    AuthorityGrantId, PayloadDigest, PrivilegedAuthority, PrivilegedIdempotency,
    PrivilegedIdempotencyKey, PrivilegedOperationId, PrivilegedOperationIntent,
    PrivilegedOperationKind, PrivilegedOperationLinks, PrivilegedOperationState,
    PrivilegedOperationTarget, PrivilegedResourceId, RequestDigest,
};
use grok_memory::{EphemeralKeyProvider, FixedClock, SequentialIdGenerator};
use grok_sqlcipher::SqlCipherStore;
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, net::TcpStream, process::Command, time::Instant};

const DATABASE_KEY: [u8; 32] = [83; 32];
const PAYLOAD: &[u8] = b"{}";
const STARTUP_NONCE: [u8; 32] = [7; 32];
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

async fn open_store(path: &std::path::Path) -> Arc<SqlCipherStore> {
    Arc::new(
        SqlCipherStore::open(path, Arc::new(EphemeralKeyProvider::new(DATABASE_KEY)))
            .await
            .expect("open privileged recovery fixture"),
    )
}

fn non_idempotent_intent(index: usize) -> PrivilegedOperationIntent {
    let request_digest: [u8; 32] =
        Sha256::digest(format!("startup-overflow-request-{index:04}")).into();
    PrivilegedOperationIntent::new(
        PrivilegedOperationKind::IntegrationStart,
        PrivilegedOperationTarget::IntegrationStart {
            vm_id: PrivilegedResourceId::new("work-vm").expect("VM ID"),
            integration_id: PrivilegedResourceId::new("wisp").expect("integration ID"),
        },
        PayloadDigest::new(Sha256::digest(PAYLOAD).into()),
        PrivilegedAuthority::new(
            AuthorityGrantId::new("startup-authority-grant-0001").expect("authority grant"),
            10_000,
        ),
        PrivilegedIdempotency::new(
            PrivilegedIdempotencyKey::new(format!("startup-overflow-key-{index:04}"))
                .expect("idempotency key"),
            RequestDigest::new(request_digest),
        ),
        PrivilegedOperationLinks::default(),
    )
}

async fn seed_interrupted_non_idempotent_operations(
    path: &std::path::Path,
) -> Vec<PrivilegedOperationId> {
    let store = open_store(path).await;
    let service = PrivilegedOperationService::new(
        store.clone(),
        Arc::new(FixedClock::new(100)),
        Arc::new(SequentialIdGenerator::new()),
    );
    let mut operation_ids = Vec::with_capacity(MAX_PRIVILEGED_RECOVERY_BATCH + 1);
    for index in 0..=MAX_PRIVILEGED_RECOVERY_BATCH {
        let prepared = service
            .prepare(PreparePrivilegedOperation {
                intent: non_idempotent_intent(index),
                payload: PAYLOAD.to_vec(),
            })
            .await
            .expect("prepare non-idempotent operation");
        let wire_digest: [u8; 32] =
            Sha256::digest(format!("startup-overflow-wire-{index:04}")).into();
        let dispatching = service
            .begin_dispatch(BeginPrivilegedDispatch {
                operation_id: prepared.operation.id.clone(),
                expected_revision: prepared.operation.revision,
                transport_operation_id: format!("startup-overflow-transport-{index:04}"),
                wire_digest,
                broker_boot_id: [3; 16],
                guest_boot_id: [4; 16],
                timeout_ms: 1_000,
            })
            .await
            .expect("commit interrupted dispatch attempt");
        assert_eq!(dispatching.attempt_count, 1);
        operation_ids.push(dispatching.id);
    }
    drop(service);
    drop(store);
    operation_ids
}

fn reserve_loopback_address() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("reserve loopback address");
    let address = listener.local_addr().expect("reserved address");
    drop(listener);
    address
}

fn daemon_command(path: &std::path::Path, address: SocketAddr) -> Command {
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
        .env("GROK_INSTALLATION_ID", "startup-recovery-test")
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

async fn assert_first_bounded_pass(path: &std::path::Path, ids: &[PrivilegedOperationId]) {
    let store = open_store(path).await;
    let mut dispatching = 0;
    let mut needs_review = 0;
    for id in ids {
        let operation = store
            .get_privileged_operation(id)
            .await
            .expect("operation after failed startup");
        assert_eq!(operation.attempt_count, 1);
        match operation.state {
            PrivilegedOperationState::Dispatching => dispatching += 1,
            PrivilegedOperationState::InterruptedNeedsReview => needs_review += 1,
            state => panic!("unexpected state after bounded startup pass: {state:?}"),
        }
    }
    assert_eq!(needs_review, MAX_PRIVILEGED_RECOVERY_BATCH);
    assert_eq!(dispatching, 1);
}

async fn wait_until_serving(child: &mut tokio::process::Child, address: SocketAddr) {
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Ok(stream) = TcpStream::connect(address).await {
            drop(stream);
            return;
        }
        if let Some(status) = child.try_wait().expect("poll second daemon") {
            panic!("later daemon exited before serving IPC: {status}");
        }
        assert!(
            Instant::now() < deadline,
            "later daemon did not begin serving IPC before the startup deadline"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn assert_all_attempts_are_review_only(
    path: &std::path::Path,
    ids: &[PrivilegedOperationId],
) {
    let store = open_store(path).await;
    for id in ids {
        let operation = store
            .get_privileged_operation(id)
            .await
            .expect("operation after successful later startup");
        assert_eq!(
            operation.state,
            PrivilegedOperationState::InterruptedNeedsReview
        );
        // Recovery records uncertainty; it never creates a second transport
        // attempt that could replay the non-idempotent integration start.
        assert_eq!(operation.attempt_count, 1);
    }
    let recovery = PrivilegedOperationService::new(
        store,
        Arc::new(FixedClock::new(20_000)),
        Arc::new(SequentialIdGenerator::new()),
    )
    .recover_interrupted(MAX_PRIVILEGED_RECOVERY_BATCH)
    .await
    .expect("recovery remains idempotent");
    assert_eq!(recovery.recovered(), 0);
    assert!(!recovery.truncated);
}

#[tokio::test]
async fn oversized_privileged_recovery_blocks_ipc_then_later_startup_finishes_without_replay() {
    let directory = tempfile::tempdir().expect("temporary daemon database directory");
    let path = directory.path().join("startup-recovery.db");
    let operation_ids = seed_interrupted_non_idempotent_operations(&path).await;
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
        diagnostics
            .contains("privileged-operation recovery backlog exceeds the bounded startup pass")
    );
    assert!(!diagnostics.contains("daemon development transport ready"));
    assert!(TcpStream::connect(address).await.is_err());

    assert_first_bounded_pass(&path, &operation_ids).await;

    let mut second_command = daemon_command(&path, address);
    second_command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut second = spawn_daemon(second_command).await;
    wait_until_serving(&mut second, address).await;
    second.start_kill().expect("stop later daemon");
    tokio::time::timeout(STARTUP_TIMEOUT, second.wait())
        .await
        .expect("later daemon stops promptly")
        .expect("wait for later daemon");

    assert_all_attempts_are_review_only(&path, &operation_ids).await;
}
