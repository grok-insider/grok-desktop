//! Process-level startup nonce and Linux process-inspection coverage.

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    process::{Output, Stdio},
    time::Duration,
};

use tokio::{io::AsyncWriteExt, process::Command};

#[cfg(target_os = "linux")]
use tokio::{net::TcpStream, time::Instant};

const STARTUP_NONCE: [u8; 32] = [0x5a; 32];
const STARTUP_TIMEOUT: Duration = Duration::from_secs(20);

fn reserve_loopback_address() -> SocketAddr {
    let listener = TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0))
        .expect("reserve loopback address");
    let address = listener.local_addr().expect("reserved address");
    drop(listener);
    address
}

fn daemon_command(address: SocketAddr) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_grok-daemon"));
    for variable in [
        "GROK_DAEMON_STARTUP_NONCE_HEX",
        "GROK_DAEMON_STARTUP_NONCE_STDIN",
        "GROK_DATABASE_PATH",
        "GROK_DATABASE_KEY_HEX",
        "GROK_ACP_EXECUTABLE",
        "GROK_ACP_VERSION",
        "GROK_ACP_SHA256",
        "GROK_ACP_WORKSPACE_ROOTS",
    ] {
        command.env_remove(variable);
    }
    command
        .env("GROK_DAEMON_EPHEMERAL", "1")
        .env("GROK_DAEMON_DEV_TCP_ADDR", address.to_string())
        .env("GROK_INSTALLATION_ID", "startup-security-test")
        .env("RUST_LOG", "error")
        .stdin(Stdio::piped());
    command
}

async fn spawn_with_input(mut command: Command, input: &[u8]) -> tokio::process::Child {
    let mut child = command.spawn().expect("launch daemon");
    let mut stdin = child.stdin.take().expect("daemon nonce stdin");
    if let Err(error) = stdin.write_all(input).await {
        // A configuration error such as the forbidden legacy nonce may be
        // rejected before the parent finishes this one-shot write. EPIPE is
        // therefore an expected race for negative cases; every other I/O
        // failure remains a harness error, and success cases still fail when
        // the child exits in their serving loop.
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    drop(stdin);
    child
}

async fn rejected_startup(input: &[u8], configure: impl FnOnce(&mut Command)) -> Output {
    let mut command = daemon_command(reserve_loopback_address());
    configure(&mut command);
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = spawn_with_input(command, input).await;
    tokio::time::timeout(STARTUP_TIMEOUT, child.wait_with_output())
        .await
        .expect("invalid startup exits promptly")
        .expect("wait for invalid startup")
}

fn diagnostics(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[tokio::test]
async fn stdin_nonce_requires_exactly_thirty_two_bytes_and_eof() {
    for input in [&STARTUP_NONCE[..31], &[0x5a; 33][..]] {
        let output = rejected_startup(input, |command| {
            command.env("GROK_DAEMON_STARTUP_NONCE_STDIN", "1");
        })
        .await;
        assert!(!output.status.success());
        assert!(diagnostics(&output).contains("reason=startup_failed"));
    }
}

#[tokio::test]
async fn legacy_nonce_environment_is_rejected_even_with_a_valid_stdin_handoff() {
    let output = rejected_startup(&STARTUP_NONCE, |command| {
        command
            .env("GROK_DAEMON_STARTUP_NONCE_HEX", "09".repeat(32))
            .env("GROK_DAEMON_STARTUP_NONCE_STDIN", "1");
    })
    .await;
    assert!(!output.status.success());
    assert!(diagnostics(&output).contains("reason=startup_failed"));
}

#[tokio::test]
async fn stdin_nonce_marker_rejects_open_ended_values() {
    let output = rejected_startup(&STARTUP_NONCE, |command| {
        command.env("GROK_DAEMON_STARTUP_NONCE_STDIN", "true");
    })
    .await;
    assert!(!output.status.success());
    assert!(diagnostics(&output).contains("reason=startup_failed"));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn serving_daemon_is_non_dumpable_to_its_same_user_parent() {
    let address = reserve_loopback_address();
    let mut command = daemon_command(address);
    command
        .env("GROK_DAEMON_STARTUP_NONCE_STDIN", "1")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let mut child = spawn_with_input(command, &STARTUP_NONCE).await;
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Ok(stream) = TcpStream::connect(address).await {
            drop(stream);
            break;
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

    let process_id = child.id().expect("daemon process ID");
    let error = std::fs::read(format!("/proc/{process_id}/environ"))
        .expect_err("non-dumpable daemon environment remained readable");
    assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);

    child.start_kill().expect("stop daemon");
    tokio::time::timeout(STARTUP_TIMEOUT, child.wait())
        .await
        .expect("daemon stops promptly")
        .expect("wait for daemon");
}
