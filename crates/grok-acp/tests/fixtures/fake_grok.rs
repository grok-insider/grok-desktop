use std::io::{self, BufRead, Write};

fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args != ["--no-auto-update", "agent", "stdio"] {
        std::process::exit(64);
    }
    let grok_home = std::env::var_os("GROK_HOME").expect("GROK_HOME");
    let expected_launch = std::path::Path::new(&grok_home).join("launch");
    if std::env::var_os("HOME").as_ref() != Some(&grok_home)
        || std::env::var_os("USERPROFILE").as_ref() != Some(&grok_home)
        || std::env::var_os("XAI_API_KEY").is_some()
        || std::env::var_os("OPENAI_BASE_URL").is_some()
        || std::env::var_os("NODE_OPTIONS").is_some()
        || !std::path::Path::new(&grok_home).join("requirements.toml").is_file()
        || std::env::current_dir().ok().as_deref() != Some(expected_launch.as_path())
    {
        std::process::exit(65);
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    let mut pending_prompt = None;
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.contains("\"method\":\"initialize\"") {
            respond(
                &mut stdout,
                id(&line),
                r#"{"protocolVersion":1,"agentCapabilities":{"loadSession":true,"promptCapabilities":{"image":false,"audio":false,"embeddedContext":true},"mcpCapabilities":{"http":true,"sse":true}},"authMethods":[{"id":"grok.com","name":"Grok.com OAuth"}],"agentInfo":{"name":"grok-build","version":"0.2.95"}}"#,
            );
        } else if line.contains("\"method\":\"authenticate\"") {
            respond(&mut stdout, id(&line), "{}");
        } else if line.contains("\"method\":\"session/new\"") {
            let host_tools_contract = line.contains("\"additionalDirectories\"")
                && line.contains("grok-desktop-host-tools")
                && line.contains("http://127.0.0.1:39281/mcp")
                && line.contains(
                    "Bearer 0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                );
            respond(
                &mut stdout,
                id(&line),
                if host_tools_contract {
                    r#"{"sessionId":"session-host-tools"}"#
                } else {
                    r#"{"sessionId":"session-1"}"#
                },
            );
        } else if line.contains("\"method\":\"session/load\"") {
            respond(&mut stdout, id(&line), "{}");
        } else if line.contains("\"method\":\"session/prompt\"") {
            let request_id = id(&line);
            if line.contains("early_exit") {
                eprintln!("Bearer should-never-leak");
                std::process::exit(23);
            } else if line.contains("malformed") {
                respond(&mut stdout, request_id, "{}");
            } else if line.contains("permission") {
                pending_prompt = Some(request_id);
                writeln!(
                    stdout,
                    "{}",
                    r#"{"jsonrpc":"2.0","id":900,"method":"session/request_permission","params":{"sessionId":"session-1","toolCall":{"toolCallId":"tool-1","title":"Write report"},"options":[{"optionId":"allow-once","name":"Allow once","kind":"allow_once"},{"optionId":"reject-once","name":"Reject","kind":"reject_once"}]}}"#
                )
                .expect("permission");
                stdout.flush().expect("flush");
            } else if line.contains("slow") {
                pending_prompt = Some(request_id);
            } else {
                update(&mut stdout, "hello from fake Grok");
                respond(&mut stdout, request_id, r#"{"stopReason":"end_turn"}"#);
            }
        } else if line.contains("\"method\":\"session/cancel\"") {
            if let Some(request_id) = pending_prompt.take() {
                respond(&mut stdout, request_id, r#"{"stopReason":"cancelled"}"#);
            }
        } else if line.contains("\"id\":900") && pending_prompt.is_some() {
            let selected = line.contains("\"outcome\":\"selected\"");
            update(
                &mut stdout,
                if selected {
                    "permission:selected"
                } else {
                    "permission:cancelled"
                },
            );
            respond(
                &mut stdout,
                pending_prompt.take().expect("prompt"),
                r#"{"stopReason":"end_turn"}"#,
            );
        }
    }
}

fn id(line: &str) -> String {
    let marker = "\"id\":";
    let start = line.find(marker).expect("request id") + marker.len();
    let value = &line[start..];
    if let Some(quoted) = value.strip_prefix('"') {
        let end = quoted.find('"').expect("quoted request id");
        format!("\"{}\"", &quoted[..end])
    } else {
        value
            .chars()
            .take_while(|character| character.is_ascii_digit())
            .collect()
    }
}

fn respond(stdout: &mut impl Write, id: String, result: &str) {
    writeln!(stdout, "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{result}}}")
        .expect("response");
    stdout.flush().expect("flush");
}

fn update(stdout: &mut impl Write, text: &str) {
    writeln!(
        stdout,
        "{{\"jsonrpc\":\"2.0\",\"method\":\"session/update\",\"params\":{{\"sessionId\":\"session-1\",\"update\":{{\"sessionUpdate\":\"agent_message_chunk\",\"content\":{{\"type\":\"text\",\"text\":\"{text}\"}}}}}}}}"
    )
    .expect("update");
    stdout.flush().expect("flush");
}
