//! End-to-end framing, dispatch, and application composition test.

use std::sync::Arc;

use grok_application::{
    ApprovalService, CreateRun, CredentialMutationStore, CredentialService, ExecutionStore,
    IdGenerator, RunService, SecretValue, XaiApiKeyValidation, XaiApiKeyValidationError,
    XaiApiKeyValidator,
};
use grok_daemon::{Daemon, read_frame, serve_connection, write_frame};
use grok_memory::{FixedClock, InMemoryExecutionStore, InMemorySecretVault, SequentialIdGenerator};
use grok_protocol::{PROTOCOL_VERSION, v1};

#[derive(Debug)]
struct AcceptXaiKey;

#[async_trait::async_trait]
impl XaiApiKeyValidator for AcceptXaiKey {
    async fn validate(
        &self,
        _api_key: &SecretValue,
    ) -> Result<XaiApiKeyValidation, XaiApiKeyValidationError> {
        Ok(XaiApiKeyValidation::CapabilitiesResolved)
    }
}

#[tokio::test]
async fn framed_ipc_reaches_application_use_cases() {
    let backing = Arc::new(InMemoryExecutionStore::new());
    let store: Arc<dyn ExecutionStore> = backing.clone();
    let credential_store: Arc<dyn CredentialMutationStore> = backing;
    let clock = Arc::new(FixedClock::new(10));
    let ids: Arc<dyn IdGenerator> = Arc::new(SequentialIdGenerator::new());
    let runs = Arc::new(RunService::new(store.clone(), clock.clone(), ids.clone()));
    let run = runs
        .create(
            CreateRun {
                project_id: "project-1".into(),
                thread_id: "thread-1".into(),
            },
            "seed-run",
        )
        .await
        .expect("seed run");
    let approvals = Arc::new(ApprovalService::new(store, clock.clone(), ids));
    let credentials = Arc::new(CredentialService::new(
        Arc::new(InMemorySecretVault::new()),
        credential_store,
        Arc::new(AcceptXaiKey),
    ));
    let daemon = Arc::new(Daemon::new(
        runs,
        approvals,
        credentials,
        clock,
        [7; 32],
        "integration-instance".into(),
    ));
    let (mut client, server) = tokio::io::duplex(16 * 1024);
    let server_task = tokio::spawn(serve_connection(server, daemon));

    let poll = v1::Envelope {
        protocol_version: PROTOCOL_VERSION,
        request_id: "request-poll-events".into(),
        startup_nonce: vec![7; 32],
        deadline_unix_ms: 2_000,
        idempotency_key: String::new(),
        payload: Some(v1::envelope::Payload::Request(v1::Request {
            operation: Some(v1::request::Operation::PollRunEvents(
                v1::PollRunEventsRequest {
                    run_id: run.id.to_string(),
                    after_sequence: 0,
                    limit: 10,
                    wait_timeout_ms: 0,
                },
            )),
        })),
    };
    write_frame(&mut client, &poll)
        .await
        .expect("write event poll");
    let response = read_frame(&mut client).await.expect("read event batch");
    let Some(v1::envelope::Payload::Response(response)) = response.payload else {
        panic!("response payload expected");
    };
    let Some(v1::response::Result::RunEventBatch(batch)) = response.result else {
        panic!("run event batch expected");
    };
    assert_eq!(batch.events.len(), 1);
    assert_eq!(batch.events[0].sequence, 1);
    assert_eq!(batch.events[0].run_id, run.id.to_string());
    assert_eq!(batch.next_sequence, 1);
    assert!(!batch.has_more);

    drop(client);
    server_task
        .await
        .expect("server task")
        .expect("clean disconnect");
}
