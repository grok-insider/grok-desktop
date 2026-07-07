//! Compiles the local daemon contract with a vendored `protoc`.

fn main() {
    let protoc = protoc_bin_vendored::protoc_bin_path().expect("vendored protoc is available");
    let mut config = prost_build::Config::new();
    config
        .protoc_executable(protoc)
        .compile_protos(&["../../proto/daemon/v1/daemon.proto"], &["../../proto"])
        .expect("daemon protocol compiles");
    println!("cargo:rerun-if-changed=../../proto/daemon/v1/daemon.proto");
}
