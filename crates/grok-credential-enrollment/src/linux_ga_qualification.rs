//! Structural qualification anchors for the Linux GA pinentry enrollment path.

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn shipped_unix_enrollment_uses_pinentry_boundary() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/unix.rs");
        assert!(root.is_file(), "unix pinentry enrollment source must ship");
        let source = std::fs::read_to_string(&root).expect("read unix enrollment");
        assert!(
            source.contains("pinentry"),
            "pinentry binary selection required"
        );
        assert!(
            source.contains("getpin") || source.contains("GETPIN") || source.contains("Assuan"),
            "Assuan getpin conversation required"
        );
        assert!(
            source.contains("cleared")
                || source.contains("allowlist")
                || source.contains("environment"),
            "spawn environment must be rebuilt/cleared for pinentry"
        );
    }
}
