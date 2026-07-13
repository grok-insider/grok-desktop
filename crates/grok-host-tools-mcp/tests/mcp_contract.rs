//! Process contract for the policy-free Host Tools MCP helper.

#![cfg(unix)]

use std::io::{BufRead, Write};

use serde_json::Value;

#[test]
fn self_test_and_closed_tool_catalog_are_stable() {
    let executable = env!("CARGO_BIN_EXE_grok-host-tools-mcp");
    let self_test = std::process::Command::new(executable)
        .arg("--self-test")
        .output()
        .expect("self test");
    assert!(self_test.status.success());
    assert_eq!(self_test.stdout, b"grok-host-tools-mcp-v1\n");

    let endpoint = std::env::temp_dir().join(format!(
        "grok-host-tools-contract-{}-{}.sock",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time")
            .as_nanos()
    ));
    let listener = std::os::unix::net::UnixListener::bind(&endpoint).expect("bind endpoint");
    let bridge = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept initialization");
        let mut request = String::new();
        std::io::BufReader::new(stream.try_clone().expect("clone stream"))
            .read_line(&mut request)
            .expect("read initialization");
        assert!(request.contains("\"initialize\":true"));
        stream
            .write_all(b"{\"content\":[],\"isError\":false}\n")
            .expect("write initialization");
    });

    let mut child = std::process::Command::new(executable)
        .args([
            "--endpoint",
            endpoint.to_str().expect("endpoint UTF-8"),
            "--run-id",
            "run-1",
            "--policy-revision",
            "7",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn helper");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let mut lines = std::io::BufReader::new(stdout).lines();

    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":1,"method":"initialize","params":{{}}}}"#
    )
    .expect("initialize");
    let initialize: Value = serde_json::from_str(
        &lines
            .next()
            .expect("initialize line")
            .expect("initialize read"),
    )
    .expect("initialize JSON");
    assert_eq!(
        initialize["result"]["serverInfo"]["name"],
        "grok-desktop-host-tools"
    );
    bridge.join().expect("bridge thread");
    let _ = std::fs::remove_file(&endpoint);

    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{{}}}}"#
    )
    .expect("tools list");
    let tools: Value =
        serde_json::from_str(&lines.next().expect("tools line").expect("tools read"))
            .expect("tools JSON");
    let names = tools["result"]["tools"]
        .as_array()
        .expect("tool array")
        .iter()
        .map(|tool| tool["name"].as_str().expect("tool name"))
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        [
            "host_filesystem_list",
            "host_filesystem_read",
            "host_filesystem_write",
            "host_process_exec"
        ]
    );
    drop(stdin);
    assert!(child.wait().expect("wait").success());
}
