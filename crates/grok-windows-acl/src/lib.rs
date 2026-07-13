#![deny(unsafe_code)]
#![warn(missing_docs)]

//! Safe, capability-focused wrappers for the Win32 ACL operations used by
//! Grok Desktop. The raw FFI is confined to one audited Windows-only module.

/// Exact private ACL shape for an application-owned filesystem object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivateObjectKind {
    /// A directory whose sole owner ACE is inherited by child files and directories.
    Directory,
    /// A file with one non-inheritable owner ACE.
    File,
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows;

#[cfg(windows)]
pub use windows::{
    VerifiedNamedPipeClient, apply_private_acl, apply_private_directory_acl,
    create_private_directory, create_private_named_pipe_server, file_has_single_link,
    open_private_file, open_private_lock_file, verify_named_pipe_client_executable,
    verify_private_acl, verify_private_directory,
};

#[cfg(any(windows, test))]
fn private_sddl(owner_sid: &str, kind: PrivateObjectKind) -> Option<String> {
    if owner_sid.len() < 5
        || owner_sid.len() > 184
        || !owner_sid.starts_with("S-1-")
        || !owner_sid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'S' | b'-'))
    {
        return None;
    }
    let inheritance = match kind {
        PrivateObjectKind::Directory => "OICI",
        PrivateObjectKind::File => "",
    };
    Some(format!(
        "O:{owner_sid}D:P(A;{inheritance};FA;;;{owner_sid})"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_descriptors_are_owner_only_and_protected() {
        assert_eq!(
            private_sddl("S-1-5-21-1-2-3-1001", PrivateObjectKind::Directory).as_deref(),
            Some("O:S-1-5-21-1-2-3-1001D:P(A;OICI;FA;;;S-1-5-21-1-2-3-1001)")
        );
        assert_eq!(
            private_sddl("S-1-5-21-1-2-3-1001", PrivateObjectKind::File).as_deref(),
            Some("O:S-1-5-21-1-2-3-1001D:P(A;;FA;;;S-1-5-21-1-2-3-1001)")
        );
        assert!(private_sddl("WD", PrivateObjectKind::File).is_none());
        assert!(private_sddl("S-1-5-21-1)(A;;FA;;;WD", PrivateObjectKind::File).is_none());
    }
}
