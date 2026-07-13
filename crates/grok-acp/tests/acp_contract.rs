//! Process-level contract tests against a deterministic fake ACP agent.

use std::{num::NonZeroUsize, path::PathBuf, process::Command, time::Duration};

use futures_util::StreamExt;
use grok_acp::{
    ExternalGrokComponent, GrokAcpConfig, GrokAcpRuntime, GrokHomeSpec, HostPermissionChannel,
    VerifiedGrokComponent, permission_channel,
};
use grok_application::{
    AgentEvent, AgentPermissionDecision, AgentRuntime, AgentRuntimeErrorKind, AgentSessionRequest,
    HostToolsMcpServer,
};
use sha2::{Digest, Sha256};

struct Fixture {
    _directory: tempfile::TempDir,
    root: PathBuf,
    component: VerifiedGrokComponent,
    home: GrokHomeSpec,
}

impl Fixture {
    fn compile() -> Self {
        let directory = tempfile::tempdir().expect("tempdir");
        let root = directory.path().join("workspace");
        std::fs::create_dir(&root).expect("workspace");
        let executable = directory
            .path()
            .join(if cfg!(windows) { "grok.exe" } else { "grok" });
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("fake_grok.rs");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let status = Command::new(rustc)
            .arg("--edition=2024")
            .arg(source)
            .arg("-o")
            .arg(&executable)
            .status()
            .expect("compile fixture");
        assert!(status.success());
        let bytes = std::fs::read(&executable).expect("fixture bytes");
        let component = VerifiedGrokComponent::verify(&ExternalGrokComponent {
            executable,
            version: "0.2.95".into(),
            sha256: hex::encode(Sha256::digest(bytes)),
            publisher: "xAI".into(),
        })
        .expect("verify fixture");
        let home = GrokHomeSpec::new(directory.path().join("app-data"), "test-installation")
            .expect("home spec");
        Self {
            _directory: directory,
            root,
            component,
            home,
        }
    }

    async fn runtime(
        &self,
        permission_timeout: Duration,
    ) -> (GrokAcpRuntime, HostPermissionChannel) {
        let (host, broker) =
            permission_channel(NonZeroUsize::new(2).expect("nonzero"), permission_timeout);
        let mut config = GrokAcpConfig::isolated_guest(
            self.component.clone(),
            vec![self.root.clone()],
            self.home.clone(),
        );
        config.initialize_timeout = Duration::from_secs(5);
        config.request_timeout = Duration::from_secs(5);
        (
            GrokAcpRuntime::start(config, broker)
                .await
                .expect("start runtime"),
            host,
        )
    }
}

#[tokio::test]
async fn reports_configuration_isolation_failure_after_managed_home_tampering() {
    let fixture = Fixture::compile();
    let (runtime, _host) = fixture.runtime(Duration::from_secs(1)).await;
    runtime.shutdown().await.expect("shutdown initial runtime");
    std::fs::write(
        fixture.home.home_path().join("config.toml"),
        b"[models]\ndefault = \"unmanaged\"\n",
    )
    .expect("tamper with managed configuration");

    let (_host, broker) = permission_channel(
        NonZeroUsize::new(2).expect("nonzero"),
        Duration::from_secs(1),
    );
    let config = GrokAcpConfig::isolated_guest(
        fixture.component.clone(),
        vec![fixture.root.clone()],
        fixture.home.clone(),
    );
    let error = GrokAcpRuntime::start(config, broker)
        .await
        .expect_err("tampered configuration must fail closed");
    assert_eq!(error.kind, AgentRuntimeErrorKind::ConfigurationIsolation);
}

#[tokio::test]
async fn host_control_runtime_authenticates_but_rejects_sessions() {
    let fixture = Fixture::compile();
    let (host, broker) = permission_channel(
        NonZeroUsize::new(2).expect("nonzero"),
        Duration::from_secs(1),
    );
    drop(host);
    let mut config = GrokAcpConfig::host_control(fixture.component.clone(), fixture.home.clone());
    config.initialize_timeout = Duration::from_secs(5);
    config.request_timeout = Duration::from_secs(5);
    let runtime = GrokAcpRuntime::start(config, broker).await.expect("start");
    runtime
        .authenticate("grok.com")
        .await
        .expect("authenticate");
    let error = runtime
        .open_session(AgentSessionRequest {
            working_directory: fixture.root.clone(),
            additional_directories: Vec::new(),
            host_tools_mcp: None,
            existing_session_id: None,
        })
        .await
        .expect_err("host session denied");
    assert_eq!(error.kind, AgentRuntimeErrorKind::Unavailable);
    runtime.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn host_work_runtime_sends_only_daemon_created_stdio_mcp_and_directories() {
    let fixture = Fixture::compile();
    let additional = fixture.root.join("secondary");
    std::fs::create_dir(&additional).expect("additional workspace");
    let (host, broker) = permission_channel(
        NonZeroUsize::new(2).expect("nonzero"),
        Duration::from_secs(1),
    );
    drop(host);
    let mut config = GrokAcpConfig::host_work_tools(
        fixture.component.clone(),
        vec![fixture.root.clone()],
        fixture.home.clone(),
    );
    config.initialize_timeout = Duration::from_secs(5);
    config.request_timeout = Duration::from_secs(5);
    let runtime = GrokAcpRuntime::start(config, broker).await.expect("start");
    let session = runtime
        .open_session(AgentSessionRequest {
            working_directory: fixture.root.clone(),
            additional_directories: vec![additional],
            host_tools_mcp: Some(HostToolsMcpServer {
                executable: fixture.component.executable().to_path_buf(),
                arguments: vec!["host-tools-contract".into()],
            }),
            existing_session_id: None,
        })
        .await
        .expect("host tools session");
    assert_eq!(session.id, "session-host-tools");
    runtime.shutdown().await.expect("shutdown");
}

async fn session(runtime: &GrokAcpRuntime, root: &std::path::Path) -> String {
    runtime
        .open_session(AgentSessionRequest {
            working_directory: root.into(),
            additional_directories: Vec::new(),
            host_tools_mcp: None,
            existing_session_id: None,
        })
        .await
        .expect("session")
        .id
}

async fn prompt_events(
    runtime: &GrokAcpRuntime,
    session_id: &str,
    text: &str,
) -> Vec<Result<AgentEvent, grok_application::AgentRuntimeError>> {
    runtime
        .prompt(grok_application::AgentPrompt {
            session_id: session_id.into(),
            text: text.into(),
        })
        .await
        .expect("prompt")
        .collect()
        .await
}

#[tokio::test]
async fn initializes_v1_streams_prompt_and_loads_session() {
    let fixture = Fixture::compile();
    let (runtime, _host) = fixture.runtime(Duration::from_secs(1)).await;
    let probe = runtime.probe().await.expect("probe");
    assert_eq!(probe.protocol_version, 1);
    assert_eq!(probe.agent_name.as_deref(), Some("grok-build"));
    assert_eq!(probe.auth_methods[0].id, "grok.com");
    assert!(probe.capabilities.load_session);
    assert!(probe.capabilities.embedded_context);
    assert!(!probe.capabilities.image_input);
    runtime
        .authenticate("grok.com")
        .await
        .expect("authenticate");
    assert_eq!(
        runtime
            .authenticate("unadvertised")
            .await
            .expect_err("method rejected")
            .kind,
        AgentRuntimeErrorKind::InvalidRequest
    );

    let session_id = session(&runtime, &fixture.root).await;
    let events = prompt_events(&runtime, &session_id, "hello").await;
    assert!(events.contains(&Ok(AgentEvent::MessageDelta("hello from fake Grok".into()))));
    assert!(events.contains(&Ok(AgentEvent::Completed {
        stop_reason: "end_turn".into()
    })));
    let loaded = runtime
        .open_session(AgentSessionRequest {
            working_directory: fixture.root.clone(),
            additional_directories: Vec::new(),
            host_tools_mcp: None,
            existing_session_id: Some("session-existing".into()),
        })
        .await
        .expect("load");
    assert_eq!(loaded.id, "session-existing");
    runtime.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn permission_grant_and_deny_are_routed_to_host() {
    for (decision, expected) in [
        (
            AgentPermissionDecision::Selected("allow-once".into()),
            "permission:selected",
        ),
        (AgentPermissionDecision::Cancelled, "permission:cancelled"),
    ] {
        let fixture = Fixture::compile();
        let (runtime, mut host) = fixture.runtime(Duration::from_secs(1)).await;
        let session_id = session(&runtime, &fixture.root).await;
        let stream = runtime
            .prompt(grok_application::AgentPrompt {
                session_id,
                text: "permission".into(),
            })
            .await
            .expect("prompt");
        let pending = host.recv().await.expect("permission request");
        assert_eq!(pending.request().title, "Write report");
        pending.respond(decision).expect("respond");
        let events = stream.collect::<Vec<_>>().await;
        assert!(events.contains(&Ok(AgentEvent::MessageDelta(expected.into()))));
        runtime.shutdown().await.expect("shutdown");
    }
}

#[tokio::test]
async fn permission_timeout_fails_closed() {
    let fixture = Fixture::compile();
    let (runtime, _host) = fixture.runtime(Duration::from_millis(20)).await;
    let session_id = session(&runtime, &fixture.root).await;
    let events = prompt_events(&runtime, &session_id, "permission").await;
    assert!(events.contains(&Ok(AgentEvent::MessageDelta("permission:cancelled".into()))));
    runtime.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn cancellation_completes_stream_with_cancelled_reason() {
    let fixture = Fixture::compile();
    let (runtime, _host) = fixture.runtime(Duration::from_secs(1)).await;
    let session_id = session(&runtime, &fixture.root).await;
    let stream = runtime
        .prompt(grok_application::AgentPrompt {
            session_id: session_id.clone(),
            text: "slow".into(),
        })
        .await
        .expect("prompt");
    runtime.cancel(&session_id).await.expect("cancel");
    let events = stream.collect::<Vec<_>>().await;
    assert!(events.contains(&Ok(AgentEvent::Completed {
        stop_reason: "cancelled".into()
    })));
    runtime.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn malformed_output_and_early_exit_fail_active_streams() {
    for (prompt, kind) in [
        ("malformed", AgentRuntimeErrorKind::Protocol),
        ("early_exit", AgentRuntimeErrorKind::Process),
    ] {
        let fixture = Fixture::compile();
        let (runtime, _host) = fixture.runtime(Duration::from_secs(1)).await;
        let session_id = session(&runtime, &fixture.root).await;
        let events = tokio::time::timeout(
            Duration::from_secs(3),
            prompt_events(&runtime, &session_id, prompt),
        )
        .await
        .expect("stream terminates");
        assert!(events.iter().any(|event| {
            matches!(event, Err(error) if error.kind == kind && !error.message.contains("Bearer"))
        }));
    }
}

#[tokio::test]
async fn workspace_outside_allowlist_is_rejected_before_acp() {
    let fixture = Fixture::compile();
    let outside = tempfile::tempdir().expect("outside");
    let (runtime, _host) = fixture.runtime(Duration::from_secs(1)).await;
    let error = runtime
        .open_session(AgentSessionRequest {
            working_directory: outside.path().into(),
            additional_directories: Vec::new(),
            host_tools_mcp: None,
            existing_session_id: None,
        })
        .await
        .expect_err("outside rejected");
    assert_eq!(error.kind, AgentRuntimeErrorKind::InvalidRequest);
    runtime.shutdown().await.expect("shutdown");
}
