//! Capability-root confinement and output-bound tests.

use std::path::Path;

use grok_application::{HostFilesystemErrorKind, HostFilesystemReader};
use grok_host_tools::CapabilityHostFilesystem;

#[tokio::test]
async fn lists_and_reads_only_beneath_an_enrolled_capability() {
    let root = tempfile::tempdir().expect("root");
    std::fs::write(root.path().join("note.txt"), "hello").expect("note");
    std::fs::create_dir(root.path().join("folder")).expect("folder");
    let filesystem = CapabilityHostFilesystem::open(&[root.path().to_string_lossy().into_owned()])
        .expect("filesystem");
    let entries = filesystem.list(root.path()).await.expect("list");
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["folder", "note.txt"]
    );
    assert_eq!(
        filesystem
            .read_text(&root.path().join("note.txt"))
            .await
            .expect("read"),
        "hello"
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
}
