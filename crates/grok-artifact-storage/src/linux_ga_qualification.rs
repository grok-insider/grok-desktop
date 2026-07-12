//! Structural qualification anchors for Linux private artifact storage.

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[test]
    fn shipped_linux_artifact_adapter_is_present() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/linux.rs");
        assert!(root.is_file(), "linux artifact content adapter must ship");
        let source = std::fs::read_to_string(&root).expect("read linux adapter");
        assert!(
            source.contains("portal") || source.contains("Portal") || source.contains("xdg"),
            "portal open path expected"
        );
        assert!(
            source.contains("unlink") || source.contains("purge") || source.contains("remove"),
            "removal path expected"
        );
    }
}
