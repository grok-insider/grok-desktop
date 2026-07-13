//! Capability-root confinement and output-bound tests.

#[cfg(unix)]
use std::path::Path;
use std::time::Duration;

use grok_application::{
    HostFilesystemErrorKind, HostFilesystemReader, HostFilesystemWriter, HostProcessErrorKind,
    HostProcessExecutor, HostProcessRequest,
};
use grok_host_tools::CapabilityHostFilesystem;
#[cfg(unix)]
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn lists_and_reads_only_beneath_an_enrolled_capability() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("note.txt"), "hello").expect("note");
    std::fs::write(root.path().join("binary.bin"), [0xff, 0x00, 0x01]).expect("binary");
    std::fs::create_dir(root.path().join("folder")).expect("folder");
    let filesystem = CapabilityHostFilesystem::open(&[root.path().to_string_lossy().into_owned()])
        .expect("filesystem");
    let entries = filesystem.list(root.path()).await.expect("list");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["binary.bin", "folder", "note.txt"]
    );
    assert_eq!(
        filesystem
            .read_text(&root.path().join("note.txt"))
            .await
            .expect("read"),
        "hello"
    );
    assert_eq!(
        filesystem
            .read_bytes(&root.path().join("binary.bin"), 8 * 1024 * 1024)
            .await
            .expect("binary read"),
        [0xff, 0x00, 0x01]
    );

    let outside = tempfile::tempdir().expect("outside");
    assert_eq!(
        filesystem
            .read_text(&outside.path().join("secret"))
            .await
            .expect_err("outside denied")
            .kind,
        HostFilesystemErrorKind::Denied
    );
}

#[cfg(unix)]
#[tokio::test]
async fn symlink_and_parent_escapes_fail_closed() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    std::fs::write(outside.path().join("secret.txt"), "secret").expect("secret");
    symlink(outside.path(), root.path().join("escape")).expect("symlink");
    let filesystem = CapabilityHostFilesystem::open(&[root.path().to_string_lossy().into_owned()])
        .expect("filesystem");
    assert!(
        filesystem
            .read_text(&root.path().join("escape/secret.txt"))
            .await
            .is_err()
    );
    assert!(
        filesystem
            .read_text(&root.path().join("../secret.txt"))
            .await
            .is_err()
    );
    assert!(
        filesystem
            .read_text(Path::new("relative.txt"))
            .await
            .is_err()
    );
    let outside_target = outside.path().join("unchanged.txt");
    std::fs::write(&outside_target, "outside").expect("outside target");
    symlink(&outside_target, root.path().join("replace-link")).expect("write symlink");
    filesystem
        .write_text(&root.path().join("replace-link"), "inside".into())
        .await
        .expect("replace link itself");
    assert_eq!(
        std::fs::read_to_string(outside_target).expect("outside unchanged"),
        "outside"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("replace-link")).expect("replacement"),
        "inside"
    );
}

#[tokio::test]
async fn writes_replace_exact_files_and_reject_outside_targets() {
    let root = tempfile::tempdir().expect("root");
    let outside = tempfile::tempdir().expect("outside");
    let filesystem = CapabilityHostFilesystem::open(&[root.path().to_string_lossy().into_owned()])
        .expect("filesystem");
    let target = root.path().join("result.txt");
    filesystem
        .write_text(&target, "first".into())
        .await
        .expect("first write");
    filesystem
        .write_text(&target, "second".into())
        .await
        .expect("replacement");
    assert_eq!(std::fs::read_to_string(target).expect("content"), "second");
    assert_eq!(
        filesystem
            .write_text(&outside.path().join("denied.txt"), "no".into())
            .await
            .expect_err("outside denied")
            .kind,
        HostFilesystemErrorKind::Denied
    );
}

#[tokio::test]
async fn daemon_private_subtrees_remain_denied_inside_a_broad_root() {
    let root = tempfile::tempdir().expect("root");
    let private = root.path().join("daemon-private");
    std::fs::create_dir(&private).expect("private directory");
    std::fs::write(private.join("secret.txt"), "secret").expect("private file");
    let filesystem = CapabilityHostFilesystem::open_with_denied_roots(
        &[root.path().to_string_lossy().into_owned()],
        &[private.to_string_lossy().into_owned()],
    )
    .expect("filesystem");

    assert_eq!(
        filesystem
            .read_text(&private.join("secret.txt"))
            .await
            .expect_err("private read denied")
            .kind,
        HostFilesystemErrorKind::Denied
    );
    assert_eq!(
        filesystem
            .write_text(&private.join("new.txt"), "no".into())
            .await
            .expect_err("private write denied")
            .kind,
        HostFilesystemErrorKind::Denied
    );
    assert_eq!(
        filesystem
            .validate(HostProcessRequest {
                argv: vec!["printf".into(), "no".into()],
                cwd: private.to_string_lossy().into_owned(),
                timeout: Duration::from_secs(1),
            })
            .await
            .expect_err("private cwd denied")
            .kind,
        HostProcessErrorKind::Denied
    );

    let public = root.path().join("public.txt");
    std::fs::write(&public, "ok").expect("public file");
    assert_eq!(
        filesystem.read_text(&public).await.expect("public read"),
        "ok"
    );

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&private, root.path().join("private-link"))
            .expect("private symlink");
        assert_eq!(
            filesystem
                .read_text(&root.path().join("private-link/secret.txt"))
                .await
                .expect_err("private symlink denied")
                .kind,
            HostFilesystemErrorKind::Denied
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn process_execution_is_bounded_rooted_and_cancellable() {
    let root = tempfile::tempdir().expect("root");
    let filesystem = CapabilityHostFilesystem::open(&[root.path().to_string_lossy().into_owned()])
        .expect("filesystem");
    let output = filesystem
        .execute(
            HostProcessRequest {
                argv: vec!["printf".into(), "hello".into()],
                cwd: root.path().to_string_lossy().into_owned(),
                timeout: Duration::from_secs(5),
            },
            CancellationToken::new(),
        )
        .await
        .expect("process");
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(output.stdout, "hello");
    assert!(output.stderr.is_empty());
    assert!(!output.truncated);

    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert_eq!(
        filesystem
            .execute(
                HostProcessRequest {
                    argv: vec!["sleep".into(), "30".into()],
                    cwd: root.path().to_string_lossy().into_owned(),
                    timeout: Duration::from_mins(1),
                },
                cancellation,
            )
            .await
            .expect_err("cancelled")
            .kind,
        HostProcessErrorKind::Interrupted
    );
}
