//! Artifact journal idempotency, global serialization, and transition coverage.

use std::sync::Arc;

use grok_application::{
    ArtifactContentReadyResult, ArtifactImportFailureCode, ArtifactImportReservation,
    ArtifactImportState, ArtifactOpenFailureCode, ArtifactOpenReservation, ArtifactOpenState,
    ArtifactRemovalReservation, ArtifactRemovalState, ArtifactRetentionState, ArtifactStore,
    CreateProject, MAX_ARTIFACT_FILE_BYTES, MAX_PROJECT_ARTIFACT_BYTES, MutationCommand,
    StoreError, WorkspaceService, WorkspaceStore,
};
use grok_domain::{Artifact, ArtifactId, ArtifactVersion, ProjectId};
use grok_memory::{EphemeralKeyProvider, FixedClock, SequentialIdGenerator};
use grok_sqlcipher::SqlCipherStore;

async fn setup() -> (tempfile::TempDir, Arc<SqlCipherStore>, ProjectId, ProjectId) {
    let directory = tempfile::tempdir().expect("temporary directory");
    let store = Arc::new(
        SqlCipherStore::open(
            directory.path().join("artifacts.db"),
            Arc::new(EphemeralKeyProvider::new([149; 32])),
        )
        .await
        .expect("open encrypted store"),
    );
    let workspace_store: Arc<dyn WorkspaceStore> = store.clone();
    let workspace = WorkspaceService::new(
        workspace_store,
        Arc::new(FixedClock::new(1)),
        Arc::new(SequentialIdGenerator::new()),
    );
    let first = workspace
        .create_project(
            CreateProject {
                name: "First project".into(),
                description: String::new(),
            },
            "first-project",
        )
        .await
        .expect("first project");
    let second = workspace
        .create_project(
            CreateProject {
                name: "Second project".into(),
                description: String::new(),
            },
            "second-project",
        )
        .await
        .expect("second project");
    (directory, store, first.id, second.id)
}

fn unavailable_artifact(id: &str, project_id: ProjectId, name: &str, now: u64) -> Artifact {
    Artifact::new_unavailable(
        ArtifactId::new(id).expect("artifact identifier"),
        project_id,
        None,
        name.into(),
        now,
    )
    .expect("unavailable artifact")
}

fn command(scope: &str, key: &str, fingerprint: u8) -> MutationCommand {
    MutationCommand {
        scope: scope.into(),
        key: key.into(),
        fingerprint: [fingerprint; 32],
    }
}

async fn commit_artifact(
    store: &SqlCipherStore,
    artifact: Artifact,
    command: &MutationCommand,
    digest: u8,
    byte_size: u64,
    now: u64,
) -> ArtifactVersion {
    let prepared = match store
        .reserve_import(artifact.clone(), command)
        .await
        .expect("reserve import")
    {
        ArtifactImportReservation::NewlyPrepared(plan) => plan,
        ArtifactImportReservation::ExactReplay(_) => panic!("unexpected import replay"),
    };
    let content = ArtifactVersion::new(
        artifact.id.clone(),
        1,
        [digest; 32],
        "text/plain".into(),
        byte_size,
        now + 1,
    )
    .expect("artifact version");
    let ready = store
        .mark_content_ready(&artifact.id, prepared.revision, content.clone(), now + 1)
        .await
        .expect("mark content ready");
    let ArtifactContentReadyResult::ContentReady(ready) = ready else {
        panic!("unexpected artifact quota failure");
    };
    assert_eq!(ready.state, ArtifactImportState::ContentReady);
    let mut available = artifact;
    available
        .record_content(content.summary(), now + 2)
        .expect("make artifact available");
    let committed = store
        .commit_import(available, 0, ready.revision, content.clone(), now + 2)
        .await
        .expect("commit import");
    assert_eq!(committed.state, ArtifactImportState::Committed);
    content
}

#[tokio::test]
async fn import_commands_resolve_exactly_and_have_one_global_active_slot() {
    let (_directory, store, first_project, second_project) = setup().await;
    let artifacts = [
        unavailable_artifact("artifact-one", first_project, "one.txt", 10),
        unavailable_artifact("artifact-two", second_project, "two.txt", 11),
    ];
    let commands = [
        command("import_artifact", "import-one", 1),
        command("import_artifact", "import-two", 2),
    ];

    let first = store.reserve_import(artifacts[0].clone(), &commands[0]);
    let second = store.reserve_import(artifacts[1].clone(), &commands[1]);
    let results = tokio::join!(first, second);
    let mut winner = None;
    let mut conflicts = 0;
    for (index, result) in [results.0, results.1].into_iter().enumerate() {
        match result {
            Ok(ArtifactImportReservation::NewlyPrepared(plan)) => winner = Some((index, plan)),
            Err(StoreError::Conflict) => conflicts += 1,
            other => panic!("unexpected reservation result: {other:?}"),
        }
    }
    let (winner_index, winner_plan) = winner.expect("one winning import");
    assert_eq!(conflicts, 1);

    assert_eq!(
        store
            .resolve_import(&commands[winner_index])
            .await
            .expect("resolve exact import"),
        Some(winner_plan.clone())
    );
    let conflicting_command = MutationCommand {
        fingerprint: [99; 32],
        ..commands[winner_index].clone()
    };
    assert_eq!(
        store.resolve_import(&conflicting_command).await,
        Err(StoreError::Conflict)
    );
    assert_eq!(
        store
            .resolve_import(&command("import_artifact", "missing", 3))
            .await
            .expect("resolve missing import"),
        None
    );

    let failed = store
        .fail_import(
            &winner_plan.artifact.id,
            winner_plan.revision,
            ArtifactImportFailureCode::SourceUnavailable,
            20,
        )
        .await
        .expect("release winning import slot");
    assert_eq!(failed.state, ArtifactImportState::Failed);

    let loser_index = 1 - winner_index;
    let newly_prepared = match store
        .reserve_import(artifacts[loser_index].clone(), &commands[loser_index])
        .await
        .expect("reserve after global slot release")
    {
        ArtifactImportReservation::NewlyPrepared(plan) => plan,
        ArtifactImportReservation::ExactReplay(_) => panic!("losing insert was not rolled back"),
    };
    assert!(matches!(
        store
            .reserve_import(artifacts[loser_index].clone(), &commands[loser_index])
            .await
            .expect("exact reservation replay"),
        ArtifactImportReservation::ExactReplay(ref replay) if replay == &newly_prepared
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn quota_failures_are_terminal_and_do_not_publish_versions() {
    let (_directory, store, first_project, second_project) = setup().await;
    let artifact_count = MAX_PROJECT_ARTIFACT_BYTES / MAX_ARTIFACT_FILE_BYTES;
    assert_eq!(artifact_count, 16, "test assumes exact quota-sized files");
    for index in 0..artifact_count {
        let suffix = index + 1;
        commit_artifact(
            &store,
            unavailable_artifact(
                &format!("quota-artifact-{suffix}"),
                first_project.clone(),
                &format!("quota-{suffix}.bin"),
                100 + index * 10,
            ),
            &command(
                "import_artifact",
                &format!("quota-import-{suffix}"),
                u8::try_from(suffix).expect("small fingerprint"),
            ),
            u8::try_from(suffix + 32).expect("small digest"),
            MAX_ARTIFACT_FILE_BYTES,
            100 + index * 10,
        )
        .await;
    }
    let usage = store
        .quota_usage(&first_project)
        .await
        .expect("project quota usage");
    assert_eq!(usage.project_bytes, MAX_PROJECT_ARTIFACT_BYTES);
    assert_eq!(usage.project_artifact_count, artifact_count);

    let overflow = unavailable_artifact(
        "quota-overflow",
        first_project.clone(),
        "overflow.bin",
        1_000,
    );
    let overflow_command = command("import_artifact", "quota-overflow", 80);
    let prepared = match store
        .reserve_import(overflow.clone(), &overflow_command)
        .await
        .expect("reserve quota overflow")
    {
        ArtifactImportReservation::NewlyPrepared(plan) => plan,
        ArtifactImportReservation::ExactReplay(_) => panic!("unexpected quota replay"),
    };
    let overflow_content = ArtifactVersion::new(
        overflow.id.clone(),
        1,
        [81; 32],
        "application/octet-stream".into(),
        1,
        1_001,
    )
    .expect("overflow metadata");
    let quota = store
        .mark_content_ready(&overflow.id, prepared.revision, overflow_content, 1_001)
        .await
        .expect("report project quota failure");
    let ArtifactContentReadyResult::QuotaExceeded { plan, failure } = quota else {
        panic!("expected project quota failure");
    };
    assert_eq!(failure, ArtifactImportFailureCode::ProjectByteQuotaExceeded);
    assert_eq!(plan.state, ArtifactImportState::Prepared);
    let failed = store
        .fail_import(&overflow.id, plan.revision, failure, 1_001)
        .await
        .expect("persist project quota failure after cleanup");
    assert_eq!(failed.state, ArtifactImportState::Failed);
    assert_eq!(
        failed.failure,
        Some(ArtifactImportFailureCode::ProjectByteQuotaExceeded)
    );
    assert_eq!(
        store.get_artifact_version(&overflow.id, 1).await,
        Err(StoreError::NotFound)
    );
    assert_eq!(
        store
            .resolve_import(&overflow_command)
            .await
            .expect("resolve quota failure"),
        Some(failed)
    );

    let oversized =
        unavailable_artifact("oversized-artifact", second_project, "oversized.bin", 2_000);
    let oversized_command = command("import_artifact", "oversized-import", 90);
    let prepared = match store
        .reserve_import(oversized.clone(), &oversized_command)
        .await
        .expect("reserve oversized import")
    {
        ArtifactImportReservation::NewlyPrepared(plan) => plan,
        ArtifactImportReservation::ExactReplay(_) => panic!("unexpected oversized replay"),
    };
    let oversized_content = ArtifactVersion::new(
        oversized.id.clone(),
        1,
        [91; 32],
        "application/octet-stream".into(),
        MAX_ARTIFACT_FILE_BYTES + 1,
        2_001,
    )
    .expect("oversized metadata");
    let quota = store
        .mark_content_ready(&oversized.id, prepared.revision, oversized_content, 2_001)
        .await
        .expect("report file-size failure");
    let ArtifactContentReadyResult::QuotaExceeded { plan, failure } = quota else {
        panic!("expected file-size failure");
    };
    assert_eq!(failure, ArtifactImportFailureCode::FileTooLarge);
    let failed = store
        .fail_import(&oversized.id, plan.revision, failure, 2_001)
        .await
        .expect("persist file-size failure after cleanup");
    assert_eq!(failed.state, ArtifactImportState::Failed);
    assert_eq!(
        failed.failure,
        Some(ArtifactImportFailureCode::FileTooLarge)
    );
    assert!(
        store
            .list_incomplete_imports(10)
            .await
            .expect("list incomplete imports")
            .is_empty()
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn open_commands_resolve_exactly_and_serialize_different_versions_globally() {
    let (_directory, store, first_project, second_project) = setup().await;
    let contents = [
        commit_artifact(
            &store,
            unavailable_artifact("open-one", first_project, "one.txt", 10),
            &command("import_artifact", "seed-one", 11),
            21,
            128,
            10,
        )
        .await,
        commit_artifact(
            &store,
            unavailable_artifact("open-two", second_project, "two.txt", 20),
            &command("import_artifact", "seed-two", 12),
            22,
            128,
            20,
        )
        .await,
    ];
    let commands = [
        command("open_artifact", "open-one", 31),
        command("open_artifact", "open-two", 32),
    ];

    let first = store.prepare_open(contents[0].clone(), &commands[0], 100);
    let second = store.prepare_open(contents[1].clone(), &commands[1], 100);
    let results = tokio::join!(first, second);
    let mut winner = None;
    let mut conflicts = 0;
    for (index, result) in [results.0, results.1].into_iter().enumerate() {
        match result {
            Ok(ArtifactOpenReservation::NewlyPrepared(plan)) => winner = Some((index, plan)),
            Err(StoreError::Conflict) => conflicts += 1,
            other => panic!("unexpected open reservation result: {other:?}"),
        }
    }
    let (winner_index, winner_plan) = winner.expect("one winning open");
    assert_eq!(conflicts, 1);
    assert_eq!(
        store
            .resolve_open(&commands[winner_index])
            .await
            .expect("resolve exact open"),
        Some(winner_plan.clone())
    );
    let conflicting_command = MutationCommand {
        fingerprint: [98; 32],
        ..commands[winner_index].clone()
    };
    assert_eq!(
        store.resolve_open(&conflicting_command).await,
        Err(StoreError::Conflict)
    );

    let dispatching = store
        .mark_open_dispatching(
            &winner_plan.content.artifact_id,
            winner_plan.content.version,
            winner_plan.revision,
            101,
        )
        .await
        .expect("persist open dispatch intent");
    let interrupted = store
        .interrupt_open(
            &dispatching.content.artifact_id,
            dispatching.content.version,
            dispatching.revision,
            102,
        )
        .await
        .expect("interrupt dispatched open");
    assert_eq!(interrupted.state, ArtifactOpenState::InterruptedNeedsReview);
    assert_eq!(
        store
            .resolve_open(&commands[winner_index])
            .await
            .expect("resolve terminal open"),
        Some(interrupted)
    );

    let loser_index = 1 - winner_index;
    let newly_prepared = match store
        .prepare_open(contents[loser_index].clone(), &commands[loser_index], 103)
        .await
        .expect("prepare after global slot release")
    {
        ArtifactOpenReservation::NewlyPrepared(plan) => plan,
        ArtifactOpenReservation::ExactReplay(_) => panic!("losing open insert was not persisted"),
    };
    let failed = store
        .fail_open(
            &newly_prepared.content.artifact_id,
            newly_prepared.content.version,
            newly_prepared.revision,
            ArtifactOpenFailureCode::InterruptedBeforeDispatch,
            104,
        )
        .await
        .expect("fail prepared open");
    assert_eq!(failed.state, ArtifactOpenState::Failed);
    assert!(matches!(
        store
            .prepare_open(
                contents[loser_index].clone(),
                &commands[loser_index],
                105,
            )
            .await
            .expect("exact terminal replay"),
        ArtifactOpenReservation::ExactReplay(ref replay) if replay == &failed
    ));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn removal_tombstones_atomically_retains_quota_and_recovers_exactly() {
    let (_directory, store, first_project, second_project) = setup().await;
    let byte_sizes = [128_u64, 256_u64];
    let contents = [
        commit_artifact(
            &store,
            unavailable_artifact("remove-one", first_project.clone(), "one.txt", 10),
            &command("import_artifact", "remove-seed-one", 41),
            51,
            byte_sizes[0],
            10,
        )
        .await,
        commit_artifact(
            &store,
            unavailable_artifact("remove-two", second_project.clone(), "two.txt", 20),
            &command("import_artifact", "remove-seed-two", 42),
            52,
            byte_sizes[1],
            20,
        )
        .await,
    ];
    let projects = [first_project, second_project];
    let commands = [
        command("remove_artifact", "remove-one", 61),
        command("remove_artifact", "remove-two", 62),
    ];

    let active_open_command = command("open_artifact", "block-removal", 60);
    let active_open = match store
        .prepare_open(contents[1].clone(), &active_open_command, 90)
        .await
        .expect("prepare global active open")
    {
        ArtifactOpenReservation::NewlyPrepared(plan) => plan,
        ArtifactOpenReservation::ExactReplay(_) => panic!("unexpected active-open replay"),
    };
    assert_eq!(
        store
            .reserve_removal(
                &contents[0].artifact_id,
                1,
                contents[0].version,
                &commands[0],
                91,
            )
            .await,
        Err(StoreError::Conflict),
        "a global active open must block metadata tombstoning"
    );
    assert_eq!(
        store
            .resolve_removal(&commands[0])
            .await
            .expect("failed removal did not reserve a command"),
        None
    );
    store
        .fail_open(
            &active_open.content.artifact_id,
            active_open.content.version,
            active_open.revision,
            ArtifactOpenFailureCode::InterruptedBeforeDispatch,
            92,
        )
        .await
        .expect("release active open slot");

    let first = store.reserve_removal(
        &contents[0].artifact_id,
        1,
        contents[0].version,
        &commands[0],
        100,
    );
    let second = store.reserve_removal(
        &contents[1].artifact_id,
        1,
        contents[1].version,
        &commands[1],
        100,
    );
    let results = tokio::join!(first, second);
    let mut winner = None;
    let mut conflicts = 0;
    for (index, result) in [results.0, results.1].into_iter().enumerate() {
        match result {
            Ok(ArtifactRemovalReservation::NewlyPending(plan)) => winner = Some((index, plan)),
            Err(StoreError::Conflict) => conflicts += 1,
            other => panic!("unexpected removal reservation result: {other:?}"),
        }
    }
    let (winner_index, pending) = winner.expect("one globally serialized removal");
    let loser_index = 1 - winner_index;
    assert_eq!(conflicts, 1);
    assert_eq!(pending.state, ArtifactRemovalState::Pending);
    assert_eq!(pending.artifact.state, grok_domain::ArtifactState::Deleted);
    assert!(pending.artifact.content.is_none());
    assert_eq!(pending.artifact.revision, 2);
    assert_eq!(
        store
            .resolve_removal(&commands[winner_index])
            .await
            .expect("resolve pending removal"),
        Some(pending.clone())
    );
    assert_eq!(
        store
            .resolve_removal(&MutationCommand {
                fingerprint: [99; 32],
                ..commands[winner_index].clone()
            })
            .await,
        Err(StoreError::Conflict)
    );
    assert_eq!(
        store
            .resolve_removal(&commands[loser_index])
            .await
            .expect("losing removal transaction rolled back"),
        None
    );
    assert_eq!(
        store
            .list_incomplete_removals(10)
            .await
            .expect("list pending removals"),
        vec![pending.clone()]
    );

    let usage_while_pending = store
        .quota_usage(&projects[winner_index])
        .await
        .expect("pending removal quota");
    assert_eq!(usage_while_pending.project_artifact_count, 0);
    assert_eq!(usage_while_pending.project_bytes, byte_sizes[winner_index]);
    assert_eq!(
        usage_while_pending.global_bytes,
        byte_sizes.iter().sum::<u64>()
    );
    assert_eq!(
        store
            .commit_removal(&pending.artifact.id, pending.revision, 101)
            .await,
        Err(StoreError::Conflict),
        "removal cannot commit before every exact version is Purged"
    );
    assert_eq!(
        store
            .prepare_open(
                contents[winner_index].clone(),
                &command("open_artifact", "open-tombstone", 70),
                101,
            )
            .await,
        Err(StoreError::Conflict),
        "tombstoned content must not be opened"
    );

    let pending_versions = store
        .list_pending_removal_versions(&pending.artifact.id, 10)
        .await
        .expect("list exact purge work");
    assert_eq!(pending_versions.len(), 1);
    let retention = &pending_versions[0];
    assert_eq!(retention.content, contents[winner_index]);
    assert_eq!(retention.state, ArtifactRetentionState::PurgePending);
    assert_eq!(retention.revision, 1);
    assert_eq!(
        store
            .mark_content_purged(
                &pending.artifact.id,
                retention.content.version,
                retention.revision + 1,
                102,
            )
            .await,
        Err(StoreError::Conflict)
    );
    let purged = store
        .mark_content_purged(
            &pending.artifact.id,
            retention.content.version,
            retention.revision,
            102,
        )
        .await
        .expect("record confirmed exact purge");
    assert_eq!(purged.state, ArtifactRetentionState::Purged);
    assert_eq!(purged.revision, 2);
    assert_eq!(purged.purged_at, Some(102));
    assert_eq!(
        store
            .get_artifact_version(&pending.artifact.id, purged.content.version)
            .await
            .expect("immutable metadata survives physical purge"),
        contents[winner_index]
    );
    let usage_after_purge = store
        .quota_usage(&projects[winner_index])
        .await
        .expect("purged quota");
    assert_eq!(usage_after_purge.project_bytes, 0);
    assert_eq!(
        usage_after_purge.global_bytes, byte_sizes[loser_index],
        "quota releases only after confirmed physical absence"
    );

    let committed = store
        .commit_removal(&pending.artifact.id, pending.revision, 103)
        .await
        .expect("commit fully purged removal");
    assert_eq!(committed.state, ArtifactRemovalState::Committed);
    assert_eq!(committed.revision, 1);
    assert!(
        store
            .list_incomplete_removals(10)
            .await
            .expect("no pending removal after commit")
            .is_empty()
    );
    assert_eq!(
        store
            .list_pending_removal_versions(&committed.artifact.id, 10)
            .await,
        Err(StoreError::Conflict)
    );
    assert!(matches!(
        store
            .reserve_removal(
                &committed.artifact.id,
                u64::MAX,
                u32::MAX,
                &commands[winner_index],
                104,
            )
            .await
            .expect("terminal exact replay ignores stale volatile arguments"),
        ArtifactRemovalReservation::ExactReplay(ref replay) if replay == &committed
    ));

    assert!(matches!(
        store
            .reserve_removal(
                &contents[loser_index].artifact_id,
                1,
                contents[loser_index].version,
                &commands[loser_index],
                105,
            )
            .await
            .expect("global removal slot released after commit"),
        ArtifactRemovalReservation::NewlyPending(_)
    ));
}
