//! Policy-free stdio MCP forwarder for daemon-owned Host Tools.

use std::{io, time::Duration};

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug)]
struct Configuration {
    endpoint: String,
    run_id: String,
    policy_revision: u64,
}

#[tokio::main]
async fn main() {
    if run().await.is_err() {
        std::process::exit(1);
    }
}

async fn run() -> Result<(), ()> {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--self-test")) {
        println!("grok-host-tools-mcp-v1");
        return Ok(());
    }
    let configuration = parse_configuration().ok_or(())?;
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    loop {
        let Some(line) = read_bounded_line(&mut stdin).await.map_err(|_| ())? else {
            return Ok(());
        };
        let Ok(request) = serde_json::from_slice::<Value>(&line) else {
            write_response(
                &mut stdout,
                error_response(&Value::Null, -32700, "invalid JSON"),
            )
            .await
            .map_err(|_| ())?;
            continue;
        };
        let id = request.get("id").cloned();
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            if let Some(id) = id {
                write_response(&mut stdout, error_response(&id, -32600, "invalid request"))
                    .await
                    .map_err(|_| ())?;
            }
            continue;
        };
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": "2025-06-18",
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "grok-desktop-host-tools", "version": env!("CARGO_PKG_VERSION") }
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tool_catalog()),
            "tools/call" => forward_tool_call(&configuration, request.get("params")).await,
            method if method.starts_with("notifications/") => continue,
            _ => Err((-32601, "method not found")),
        };
        if let Some(id) = id {
            let response = match result {
                Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                Err((code, message)) => error_response(&id, code, message),
            };
            write_response(&mut stdout, response)
                .await
                .map_err(|_| ())?;
        }
    }
}

fn parse_configuration() -> Option<Configuration> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments.len() != 6
        || arguments[0] != "--endpoint"
        || arguments[2] != "--run-id"
        || arguments[4] != "--policy-revision"
        || arguments[1].is_empty()
        || arguments[3].is_empty()
        || arguments[1].len() > 4096
        || arguments[3].len() > 128
    {
        return None;
    }
    Some(Configuration {
        endpoint: arguments[1].clone(),
        run_id: arguments[3].clone(),
        policy_revision: arguments[5].parse().ok()?,
    })
}

fn tool_catalog() -> Value {
    json!({ "tools": [
        {
            "name": "host_filesystem_list",
            "description": "List one enrolled host directory.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "path": { "type": "string" } }, "required": ["path"]
            }
        },
        {
            "name": "host_filesystem_read",
            "description": "Read one bounded file inside an enrolled host root.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": { "path": { "type": "string" } }, "required": ["path"]
            }
        },
        {
            "name": "host_filesystem_write",
            "description": "Write one exact enrolled host path after user approval.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "path": { "type": "string" }, "content": { "type": "string" }
                }, "required": ["path", "content"]
            }
        },
        {
            "name": "host_process_exec",
            "description": "Run one exact host process invocation after user approval. This has the desktop user's authority.",
            "inputSchema": {
                "type": "object", "additionalProperties": false,
                "properties": {
                    "argv": { "type": "array", "items": { "type": "string" }, "minItems": 1, "maxItems": 64 },
                    "cwd": { "type": "string" },
                    "timeoutMs": { "type": "integer", "minimum": 1, "maximum": 300_000 }
                }, "required": ["argv", "cwd"]
            }
        }
    ] })
}

async fn forward_tool_call(
    configuration: &Configuration,
    parameters: Option<&Value>,
) -> Result<Value, (i64, &'static str)> {
    let parameters = parameters.ok_or((-32602, "invalid tool arguments"))?;
    let request = json!({
        "version": 1,
        "runId": configuration.run_id,
        "policyRevision": configuration.policy_revision,
        "toolCall": parameters
    });
    bridge_call(&configuration.endpoint, &request)
        .await
        .map_err(|_| (-32000, "Host Tools daemon channel unavailable"))
}

#[cfg(unix)]
async fn bridge_call(endpoint: &str, request: &Value) -> io::Result<Value> {
    let stream = tokio::time::timeout(IO_TIMEOUT, tokio::net::UnixStream::connect(endpoint))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "connect timeout"))??;
    exchange(stream, request).await
}

#[cfg(windows)]
async fn bridge_call(endpoint: &str, request: &Value) -> io::Result<Value> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let stream = ClientOptions::new().open(endpoint)?;
    exchange(stream, request).await
}

#[cfg(not(any(unix, windows)))]
async fn bridge_call(_endpoint: &str, _request: &Value) -> io::Result<Value> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "unsupported host",
    ))
}

async fn exchange<S>(mut stream: S, request: &Value) -> io::Result<Value>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut encoded = serde_json::to_vec(request)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "encode"))?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "oversize request",
        ));
    }
    encoded.push(b'\n');
    tokio::time::timeout(IO_TIMEOUT, stream.write_all(&encoded))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "write timeout"))??;
    let response = tokio::time::timeout(IO_TIMEOUT, read_bounded_line(&mut stream))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "read timeout"))??
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "bridge closed"))?;
    serde_json::from_slice(&response)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "invalid bridge response"))
}

async fn read_bounded_line<R>(reader: &mut R) -> io::Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut line = Vec::with_capacity(4096);
    let mut byte = [0_u8; 1];
    loop {
        let read = reader.read(&mut byte).await?;
        if read == 0 {
            return if line.is_empty() {
                Ok(None)
            } else {
                Ok(Some(line))
            };
        }
        if byte[0] == b'\n' {
            return Ok(Some(line));
        }
        if line.len() == MAX_MESSAGE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "oversize message",
            ));
        }
        line.push(byte[0]);
    }
}

async fn write_response<W>(writer: &mut W, response: Value) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let mut encoded = serde_json::to_vec(&response)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "encode"))?;
    if encoded.len() > MAX_MESSAGE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversize response",
        ));
    }
    encoded.push(b'\n');
    writer.write_all(&encoded).await?;
    writer.flush().await
}

fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
