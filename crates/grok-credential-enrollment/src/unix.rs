//! Renderer-free credential entry through pinentry for unix daemons.
//!
//! The daemon speaks the Assuan protocol to a local `pinentry` program over
//! pipes, so the entered key never crosses renderer IPC and never appears in
//! argv, files, or the environment. An explicit `GROK_PINENTRY` development
//! override must be an absolute executable path. Default lookup may consult
//! `PATH`, but only a canonical root-owned executable beneath safe canonical
//! ancestors is trusted. The child receives a closed display/session/locale
//! environment instead of inheriting the daemon environment.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use grok_application::{CredentialEnrollmentError, SecretValue};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use zeroize::Zeroize;

/// People type keys by hand; give them time without holding the prompt open forever.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(110);
/// Bounds the whole Assuan conversation; anything past a vault-sized secret is hostile.
const MAX_CONVERSATION_BYTES: u64 = 1024 * 1024 + 4096;
/// GPG error values carry the code in their low 16 bits.
const GPG_ERR_CODE_MASK: u32 = 0xFFFF;
/// `GPG_ERR_CANCELED` and `GPG_ERR_FULLY_CANCELED`.
const GPG_ERR_CANCELED: u32 = 99;
const GPG_ERR_FULLY_CANCELED: u32 = 198;
/// Keeps hostile environment values from turning path validation into unbounded work.
const MAX_PINENTRY_PATH_BYTES: usize = 4096;
const MAX_PINENTRY_BASENAME_BYTES: usize = 64;
/// One inherited session value cannot consume arbitrary argv/environment space.
const MAX_PINENTRY_ENVIRONMENT_VALUE_BYTES: usize = 4096;
/// The complete child environment stays small even when every allowed value is present.
const MAX_PINENTRY_ENVIRONMENT_BYTES: usize = 16 * 1024;
const MAX_DISPLAY_BYTES: usize = 64;
const MAX_WAYLAND_DISPLAY_BYTES: usize = 256;
const MAX_LOCALE_BYTES: usize = 128;
/// Only variables needed to attach a native prompt to the user's graphical
/// session and render localized text cross the child-process boundary.
const PINENTRY_ENVIRONMENT_ALLOWLIST: &[&str] = &[
    "DBUS_SESSION_BUS_ADDRESS",
    "DISPLAY",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_MESSAGES",
    "WAYLAND_DISPLAY",
    "XAUTHORITY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProgramTrust {
    ExplicitDevelopmentOverride,
    DefaultSystemLookup,
}

#[derive(Debug)]
struct PinentryProgram {
    path: PathBuf,
    trust: ProgramTrust,
}

struct PinentryChild {
    child: Child,
    process_group: Option<rustix::process::Pid>,
}

impl PinentryChild {
    fn new(child: Child) -> Result<Self, CredentialEnrollmentError> {
        let raw_pid = child
            .id()
            .and_then(|value| i32::try_from(value).ok())
            .and_then(rustix::process::Pid::from_raw)
            .ok_or(CredentialEnrollmentError::Unavailable)?;
        Ok(Self {
            child,
            process_group: Some(raw_pid),
        })
    }

    fn kill_process_group(&self) {
        if let Some(process_group) = self.process_group {
            let _ =
                rustix::process::kill_process_group(process_group, rustix::process::Signal::KILL);
        }
    }

    async fn kill_and_reap(&mut self) {
        self.kill_process_group();
        let _ = self.child.wait().await;
        self.process_group = None;
    }
}

impl Drop for PinentryChild {
    fn drop(&mut self) {
        self.kill_process_group();
    }
}

/// Collects the xAI API key through a local pinentry dialog.
///
/// # Errors
///
/// Maps a dismissed dialog to [`CredentialEnrollmentError::Cancelled`] and
/// every transport, protocol, or configuration failure to
/// [`CredentialEnrollmentError::Unavailable`].
pub(crate) async fn prompt_xai_api_key() -> Result<SecretValue, CredentialEnrollmentError> {
    #[cfg(not(debug_assertions))]
    if std::env::var_os("GROK_PINENTRY").is_some() {
        return Err(CredentialEnrollmentError::Unavailable);
    }
    let program = resolve_pinentry_program(explicit_pinentry_override(), std::env::var_os("PATH"))?;
    let environment = filtered_pinentry_environment(std::env::vars_os());
    prompt_with_program(program, environment).await
}

#[cfg(debug_assertions)]
fn explicit_pinentry_override() -> Option<OsString> {
    std::env::var_os("GROK_PINENTRY")
}

#[cfg(not(debug_assertions))]
const fn explicit_pinentry_override() -> Option<OsString> {
    None
}

#[cfg(test)]
async fn prompt_with(program: OsString) -> Result<SecretValue, CredentialEnrollmentError> {
    let path = validate_pinentry_program(
        Path::new(&program),
        ProgramTrust::ExplicitDevelopmentOverride,
    )?;
    let environment = filtered_pinentry_environment(std::env::vars_os());
    prompt_with_program(
        PinentryProgram {
            path,
            trust: ProgramTrust::ExplicitDevelopmentOverride,
        },
        environment,
    )
    .await
}

async fn prompt_with_program<I>(
    program: PinentryProgram,
    environment: I,
) -> Result<SecretValue, CredentialEnrollmentError>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    // Revalidate the canonical target immediately before spawning. Default
    // targets cannot be replaced by this unprivileged process because their
    // file and canonical ancestors are root-owned and non-writable.
    let path = validate_pinentry_program(&program.path, program.trust)?;
    let environment = filtered_pinentry_environment(environment);
    let child = spawn_pinentry(&path, &environment).await?;
    let mut child = PinentryChild::new(child)?;
    let stdin = child
        .child
        .stdin
        .take()
        .ok_or(CredentialEnrollmentError::Unavailable)?;
    let stdout = child
        .child
        .stdout
        .take()
        .ok_or(CredentialEnrollmentError::Unavailable)?;
    let mut reader = BufReader::new(stdout).take(MAX_CONVERSATION_BYTES);
    let conversation = converse(stdin, &mut reader);
    let result = match tokio::time::timeout(PROMPT_TIMEOUT, conversation).await {
        Ok(result) => result,
        Err(_elapsed) => Err(CredentialEnrollmentError::Cancelled),
    };
    // The process-group guard is the cancellation backstop; normal cleanup
    // also kills descendants and deterministically reaps the direct child.
    child.kill_and_reap().await;
    result
}

fn resolve_pinentry_program(
    explicit: Option<OsString>,
    search_path: Option<OsString>,
) -> Result<PinentryProgram, CredentialEnrollmentError> {
    if let Some(explicit) = explicit {
        let path = validate_pinentry_program(
            Path::new(&explicit),
            ProgramTrust::ExplicitDevelopmentOverride,
        )?;
        return Ok(PinentryProgram {
            path,
            trust: ProgramTrust::ExplicitDevelopmentOverride,
        });
    }

    let search_path = search_path.ok_or(CredentialEnrollmentError::Unavailable)?;
    for directory in std::env::split_paths(&search_path) {
        // Empty and relative PATH entries delegate executable choice to the
        // daemon working directory and are never trusted.
        if !directory.is_absolute() {
            continue;
        }
        let candidate = directory.join("pinentry");
        if let Ok(path) = validate_pinentry_program(&candidate, ProgramTrust::DefaultSystemLookup) {
            return Ok(PinentryProgram {
                path,
                trust: ProgramTrust::DefaultSystemLookup,
            });
        }
    }
    Err(CredentialEnrollmentError::Unavailable)
}

fn validate_pinentry_program(
    path: &Path,
    trust: ProgramTrust,
) -> Result<PathBuf, CredentialEnrollmentError> {
    use std::os::unix::ffi::OsStrExt as _;
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if !path.is_absolute() || path.as_os_str().as_bytes().len() > MAX_PINENTRY_PATH_BYTES {
        return Err(CredentialEnrollmentError::Unavailable);
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| CredentialEnrollmentError::Unavailable)?;
    if !canonical.is_absolute() || canonical.as_os_str().as_bytes().len() > MAX_PINENTRY_PATH_BYTES
    {
        return Err(CredentialEnrollmentError::Unavailable);
    }
    let metadata = canonical
        .metadata()
        .map_err(|_| CredentialEnrollmentError::Unavailable)?;
    let mode = metadata.permissions().mode();
    if !metadata.is_file() || mode & 0o111 == 0 || mode & 0o6022 != 0 {
        return Err(CredentialEnrollmentError::Unavailable);
    }
    if trust == ProgramTrust::DefaultSystemLookup {
        if metadata.uid() != 0 || !is_canonical_pinentry_name(&canonical) {
            return Err(CredentialEnrollmentError::Unavailable);
        }
        for ancestor in canonical.ancestors().skip(1) {
            let metadata = ancestor
                .metadata()
                .map_err(|_| CredentialEnrollmentError::Unavailable)?;
            if !safe_default_ancestor(ancestor, &metadata) {
                return Err(CredentialEnrollmentError::Unavailable);
            }
        }
    }
    Ok(canonical)
}

fn is_canonical_pinentry_name(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;

    let Some(name) = path.file_name() else {
        return false;
    };
    let name = name.as_bytes();
    if name == b"pinentry" {
        return true;
    }
    let Some(suffix) = name.strip_prefix(b"pinentry-") else {
        return false;
    };
    !suffix.is_empty()
        && name.len() <= MAX_PINENTRY_BASENAME_BYTES
        && suffix
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn safe_default_ancestor(path: &Path, metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

    if !metadata.is_dir() || metadata.uid() != 0 {
        return false;
    }
    let mode = metadata.permissions().mode();
    if mode & 0o022 == 0 {
        return true;
    }
    // Multi-user Nix uses a root-owned, sticky, group-writable /nix/store.
    // Sticky ownership prevents nix builders from replacing a root-owned store
    // object; every package directory below it is still required to be
    // root-owned and non-writable. World-writable sticky directories such as
    // /tmp remain rejected.
    path == Path::new("/nix/store") && mode & 0o1000 != 0 && mode & 0o002 == 0
}

fn filtered_pinentry_environment<I>(environment: I) -> Vec<(OsString, OsString)>
where
    I: IntoIterator<Item = (OsString, OsString)>,
{
    let mut candidates = vec![None; PINENTRY_ENVIRONMENT_ALLOWLIST.len()];
    for (key, value) in environment {
        if let Some(index) = PINENTRY_ENVIRONMENT_ALLOWLIST
            .iter()
            .position(|allowed| key == OsStr::new(allowed))
        {
            candidates[index].get_or_insert(value);
        }
    }

    let mut total_bytes: usize = 0;
    PINENTRY_ENVIRONMENT_ALLOWLIST
        .iter()
        .zip(candidates)
        .filter_map(|(key, value)| {
            let value = value?;
            if !valid_pinentry_environment_value(key, &value) {
                return None;
            }
            let entry_bytes = key.len().saturating_add(1).saturating_add(os_bytes(&value));
            let next_total = total_bytes.saturating_add(entry_bytes);
            if next_total > MAX_PINENTRY_ENVIRONMENT_BYTES {
                return None;
            }
            total_bytes = next_total;
            Some((OsString::from(key), value))
        })
        .collect()
}

fn valid_pinentry_environment_value(key: &str, value: &OsStr) -> bool {
    let byte_length = os_bytes(value);
    if byte_length == 0 || byte_length > MAX_PINENTRY_ENVIRONMENT_VALUE_BYTES {
        return false;
    }
    let Some(value) = value.to_str() else {
        return false;
    };
    if value.chars().any(char::is_control) {
        return false;
    }
    match key {
        "DBUS_SESSION_BUS_ADDRESS" => value
            .strip_prefix("unix:path=")
            .is_some_and(|path| !path.contains([',', ';']) && is_absolute_session_path(path)),
        "DISPLAY" => is_local_x11_display(value),
        "LANG" | "LANGUAGE" | "LC_ALL" | "LC_CTYPE" | "LC_MESSAGES" => {
            value.len() <= MAX_LOCALE_BYTES
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._@:-".contains(&byte))
        }
        "WAYLAND_DISPLAY" => {
            value.len() <= MAX_WAYLAND_DISPLAY_BYTES
                && (is_absolute_session_path(value)
                    || (value.starts_with("wayland-")
                        && value
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))))
        }
        "XAUTHORITY" | "XDG_RUNTIME_DIR" => is_absolute_session_path(value),
        "XDG_SESSION_TYPE" => matches!(value, "wayland" | "x11"),
        _ => false,
    }
}

fn os_bytes(value: &OsStr) -> usize {
    use std::os::unix::ffi::OsStrExt as _;

    value.as_bytes().len()
}

fn is_absolute_session_path(value: &str) -> bool {
    value.len() > 1
        && value.starts_with('/')
        && !value.ends_with('/')
        && value
            .split('/')
            .skip(1)
            .all(|segment| !segment.is_empty() && !matches!(segment, "." | ".."))
}

fn is_local_x11_display(value: &str) -> bool {
    if value.len() > MAX_DISPLAY_BYTES {
        return false;
    }
    let Some(display) = value
        .strip_prefix(':')
        .or_else(|| value.strip_prefix("unix:"))
    else {
        return false;
    };
    let (display, screen) = display
        .split_once('.')
        .map_or((display, None), |(display, screen)| (display, Some(screen)));
    decimal_component(display, 5) && screen.is_none_or(|screen| decimal_component(screen, 3))
}

fn decimal_component(value: &str, maximum_digits: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_digits
        && value.bytes().all(|byte| byte.is_ascii_digit())
}

async fn spawn_pinentry(
    program: &Path,
    environment: &[(OsString, OsString)],
) -> Result<Child, CredentialEnrollmentError> {
    let mut attempts = 0;
    loop {
        let mut command = Command::new(program);
        command
            .env_clear()
            .envs(environment.iter().cloned())
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        let spawned = command.spawn();
        match spawned {
            Ok(child) => return Ok(child),
            // A concurrently forked process can briefly hold the executable
            // open (ETXTBSY); one short retry window resolves it.
            Err(error)
                if error.kind() == std::io::ErrorKind::ExecutableFileBusy && attempts < 3 =>
            {
                attempts += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(_) => return Err(CredentialEnrollmentError::Unavailable),
        }
    }
}

async fn converse<R>(
    stdin: ChildStdin,
    reader: &mut R,
) -> Result<SecretValue, CredentialEnrollmentError>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut writer = stdin;
    expect_ok(reader).await?;
    for command in [
        "SETTITLE Grok Desktop",
        "SETDESC Enter your xAI API key. It is stored only inside the local daemon vault and never shown to the interface process.",
        "SETPROMPT xAI API key:",
        "SETOK Save key",
    ] {
        send_line(&mut writer, command).await?;
        expect_ok(reader).await?;
    }
    send_line(&mut writer, "GETPIN").await?;
    let secret = read_pin(reader).await;
    let _ = send_line(&mut writer, "BYE").await;
    secret
}

async fn send_line(writer: &mut ChildStdin, line: &str) -> Result<(), CredentialEnrollmentError> {
    writer
        .write_all(format!("{line}\n").as_bytes())
        .await
        .map_err(|_| CredentialEnrollmentError::Unavailable)?;
    writer
        .flush()
        .await
        .map_err(|_| CredentialEnrollmentError::Unavailable)
}

async fn read_line<R>(reader: &mut R) -> Result<String, CredentialEnrollmentError>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut line = String::new();
    let read = reader
        .read_line(&mut line)
        .await
        .map_err(|_| CredentialEnrollmentError::Unavailable)?;
    if read == 0 {
        return Err(CredentialEnrollmentError::Unavailable);
    }
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    Ok(line)
}

async fn expect_ok<R>(reader: &mut R) -> Result<(), CredentialEnrollmentError>
where
    R: AsyncBufReadExt + Unpin,
{
    loop {
        let line = read_line(reader).await?;
        if line.starts_with("OK") {
            return Ok(());
        }
        if line.starts_with("S ") || line.starts_with('#') {
            continue;
        }
        return Err(classify_error(&line));
    }
}

async fn read_pin<R>(reader: &mut R) -> Result<SecretValue, CredentialEnrollmentError>
where
    R: AsyncBufReadExt + Unpin,
{
    let mut data: Option<Vec<u8>> = None;
    loop {
        let mut line = read_line(reader).await?;
        if let Some(payload) = line.strip_prefix("D ") {
            let decoded = unescape_assuan(payload)?;
            if let Some(mut previous) = data.replace(decoded) {
                previous.zeroize();
            }
            line.zeroize();
            continue;
        }
        if line.starts_with("S ") || line.starts_with('#') {
            continue;
        }
        if line.starts_with("OK") {
            // Submitting an empty dialog is a dismissal, not a credential.
            let value = data.take().ok_or(CredentialEnrollmentError::Cancelled)?;
            return SecretValue::new(value).map_err(|_| CredentialEnrollmentError::Cancelled);
        }
        let error = classify_error(&line);
        if let Some(mut stale) = data.take() {
            stale.zeroize();
        }
        line.zeroize();
        return Err(error);
    }
}

fn classify_error(line: &str) -> CredentialEnrollmentError {
    let Some(rest) = line.strip_prefix("ERR ") else {
        return CredentialEnrollmentError::Unavailable;
    };
    let code = rest
        .split_whitespace()
        .next()
        .and_then(|raw| raw.parse::<u32>().ok())
        .map(|full| full & GPG_ERR_CODE_MASK);
    match code {
        Some(GPG_ERR_CANCELED | GPG_ERR_FULLY_CANCELED) => CredentialEnrollmentError::Cancelled,
        _ if rest.to_ascii_lowercase().contains("cancel") => CredentialEnrollmentError::Cancelled,
        _ => CredentialEnrollmentError::Unavailable,
    }
}

/// Decodes Assuan percent escapes (`%25`, `%0A`, ...) into raw bytes.
fn unescape_assuan(payload: &str) -> Result<Vec<u8>, CredentialEnrollmentError> {
    let bytes = payload.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let escape = bytes
                .get(index + 1..index + 3)
                .and_then(|pair| std::str::from_utf8(pair).ok())
                .and_then(|pair| u8::from_str_radix(pair, 16).ok());
            let Some(value) = escape else {
                decoded.zeroize();
                return Err(CredentialEnrollmentError::Unavailable);
            };
            decoded.push(value);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    Ok(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_timeout_precedes_the_daemon_enrollment_budget() {
        assert_eq!(PROMPT_TIMEOUT, Duration::from_secs(110));
        assert!(PROMPT_TIMEOUT < Duration::from_mins(2));
    }

    fn fake_pinentry(script_body: &str) -> tempfile::TempPath {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let mut file = tempfile::NamedTempFile::new().expect("temp script");
        write!(file, "#!/bin/sh\n{script_body}").expect("write script");
        let path = file.into_temp_path();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))
            .expect("script permissions");
        path
    }

    async fn run_with(script_body: &str) -> Result<SecretValue, CredentialEnrollmentError> {
        let script = fake_pinentry(script_body);
        prompt_with(script.as_os_str().to_os_string()).await
    }

    async fn run_with_environment(
        script_body: &str,
        environment: Vec<(OsString, OsString)>,
    ) -> Result<SecretValue, CredentialEnrollmentError> {
        let script = fake_pinentry(script_body);
        let path = validate_pinentry_program(&script, ProgramTrust::ExplicitDevelopmentOverride)
            .expect("validated fake pinentry");
        prompt_with_program(
            PinentryProgram {
                path,
                trust: ProgramTrust::ExplicitDevelopmentOverride,
            },
            environment,
        )
        .await
    }

    #[tokio::test]
    async fn collects_percent_escaped_secret() {
        let result = run_with(
            r#"echo OK
while read line; do
  case "$line" in
    GETPIN) echo "D xai-%25key%0Aend"; echo OK ;;
    BYE) echo OK; exit 0 ;;
    *) echo OK ;;
  esac
done"#,
        )
        .await
        .expect("secret");
        assert_eq!(result.expose_secret(), b"xai-%key\nend");
    }

    #[tokio::test]
    async fn maps_cancel_to_cancelled() {
        let error = run_with(
            r#"echo OK
while read line; do
  case "$line" in
    GETPIN) echo "ERR 83886179 Operation cancelled <Pinentry>" ;;
    BYE) echo OK; exit 0 ;;
    *) echo OK ;;
  esac
done"#,
        )
        .await
        .expect_err("cancelled");
        assert_eq!(error, CredentialEnrollmentError::Cancelled);
    }

    #[tokio::test]
    async fn child_environment_is_a_closed_display_session_locale_allowlist() {
        let result = run_with_environment(
            r#"if [ "$DBUS_SESSION_BUS_ADDRESS" != "unix:path=/run/test/bus" ] ||
   [ "$DISPLAY" != ":77" ] ||
   [ "$LANG" != "C.UTF-8" ] ||
   [ "$LANGUAGE" != "en" ] ||
   [ "$LC_ALL" != "C.UTF-8" ] ||
   [ "$LC_CTYPE" != "C.UTF-8" ] ||
   [ "$LC_MESSAGES" != "C.UTF-8" ] ||
   [ "$WAYLAND_DISPLAY" != "wayland-test" ] ||
   [ "$XAUTHORITY" != "/run/test/xauthority" ] ||
   [ "$XDG_RUNTIME_DIR" != "/run/test" ] ||
   [ "$XDG_SESSION_TYPE" != "wayland" ]; then
  echo "ERR 1 required environment missing"
  exit 0
fi
if [ "${GROK_DAEMON_STARTUP_NONCE_STDIN+x}" = x ] ||
   [ "${GROK_DAEMON_STARTUP_NONCE_HEX+x}" = x ] ||
   [ "${GROK_DAEMON_SOCKET+x}" = x ] ||
   [ "${GROK_DAEMON_PIPE+x}" = x ] ||
   [ "${GROK_DATABASE_PATH+x}" = x ] ||
   [ "${GROK_DATABASE_KEY_HEX+x}" = x ] ||
   [ "${XAI_API_KEY+x}" = x ] ||
   [ "${GROK_PINENTRY+x}" = x ] ||
   [ "${GROK_TEST_CANARY+x}" = x ] ||
   [ "${HOME+x}" = x ]; then
  echo "ERR 1 forbidden environment present"
  exit 0
fi
echo OK
while read line; do
  case "$line" in
    GETPIN) echo "D local-test-value"; echo OK ;;
    BYE) echo OK; exit 0 ;;
    *) echo OK ;;
  esac
done"#,
            [
                ("DBUS_SESSION_BUS_ADDRESS", "unix:path=/run/test/bus"),
                ("DISPLAY", ":77"),
                ("LANG", "C.UTF-8"),
                ("LANGUAGE", "en"),
                ("LC_ALL", "C.UTF-8"),
                ("LC_CTYPE", "C.UTF-8"),
                ("LC_MESSAGES", "C.UTF-8"),
                ("WAYLAND_DISPLAY", "wayland-test"),
                ("XAUTHORITY", "/run/test/xauthority"),
                ("XDG_RUNTIME_DIR", "/run/test"),
                ("XDG_SESSION_TYPE", "wayland"),
                ("GROK_DAEMON_STARTUP_NONCE_STDIN", "must-not-cross"),
                ("GROK_DAEMON_STARTUP_NONCE_HEX", "must-not-cross"),
                ("GROK_DAEMON_SOCKET", "/run/test/daemon.sock"),
                ("GROK_DAEMON_PIPE", "must-not-cross"),
                ("GROK_DATABASE_PATH", "/private/database"),
                ("GROK_DATABASE_KEY_HEX", "must-not-cross"),
                ("XAI_API_KEY", "must-not-cross"),
                ("GROK_PINENTRY", "/must/not/cross"),
                ("GROK_TEST_CANARY", "must-not-cross"),
                ("HOME", "/must/not/cross"),
            ]
            .into_iter()
            .map(|(key, value)| (OsString::from(key), OsString::from(value)))
            .collect(),
        )
        .await
        .expect("allowlisted environment");
        assert_eq!(result.expose_secret(), b"local-test-value");
    }

    #[test]
    fn filter_omits_malformed_transports_paths_displays_and_locales() {
        let oversized = "x".repeat(MAX_PINENTRY_ENVIRONMENT_VALUE_BYTES + 1);
        let environment = [
            (
                "DBUS_SESSION_BUS_ADDRESS",
                "tcp:host=attacker.example".into(),
            ),
            ("DISPLAY", "attacker.example:0".into()),
            ("LANG", oversized),
            ("LANGUAGE", "en:fr".into()),
            ("LC_ALL", "C.UTF-8\nLD_PRELOAD=bad".into()),
            ("WAYLAND_DISPLAY", "../../attacker.sock".into()),
            ("XAUTHORITY", "relative/xauthority".into()),
            ("XDG_RUNTIME_DIR", "/run/user/1000/../0".into()),
            ("XDG_SESSION_TYPE", "remote".into()),
            ("GROK_DAEMON_STARTUP_NONCE_STDIN", "must-not-cross".into()),
        ]
        .into_iter()
        .map(|(key, value)| (OsString::from(key), OsString::from(value)));

        assert_eq!(
            filtered_pinentry_environment(environment),
            vec![(OsString::from("LANGUAGE"), OsString::from("en:fr"))]
        );
    }

    #[test]
    fn filter_accepts_only_local_display_and_unix_bus_syntax() {
        for display in [":0", ":12.3", "unix:7"] {
            assert!(valid_pinentry_environment_value(
                "DISPLAY",
                OsStr::new(display)
            ));
        }
        for display in ["localhost:10.0", "host:0", ":", ":1.bad"] {
            assert!(!valid_pinentry_environment_value(
                "DISPLAY",
                OsStr::new(display)
            ));
        }
        assert!(valid_pinentry_environment_value(
            "DBUS_SESSION_BUS_ADDRESS",
            OsStr::new("unix:path=/run/user/1000/bus")
        ));
        for address in [
            "autolaunch:",
            "tcp:host=127.0.0.1",
            "unix:exec=/bin/false",
            "unix:path=relative",
            "unix:path=/run/bus;tcp:host=127.0.0.1",
        ] {
            assert!(!valid_pinentry_environment_value(
                "DBUS_SESSION_BUS_ADDRESS",
                OsStr::new(address)
            ));
        }
    }

    #[tokio::test]
    async fn empty_submission_is_cancelled() {
        let error = run_with(
            r#"echo OK
while read line; do
  case "$line" in
    GETPIN) echo OK ;;
    BYE) echo OK; exit 0 ;;
    *) echo OK ;;
  esac
done"#,
        )
        .await
        .expect_err("empty");
        assert_eq!(error, CredentialEnrollmentError::Cancelled);
    }

    #[tokio::test]
    async fn missing_binary_is_unavailable() {
        let error = prompt_with(OsString::from("/nonexistent/grok-pinentry"))
            .await
            .expect_err("unavailable");
        assert_eq!(error, CredentialEnrollmentError::Unavailable);
    }

    #[tokio::test]
    async fn explicit_program_must_be_absolute_regular_and_executable() {
        use std::os::unix::fs::PermissionsExt;

        for relative in ["pinentry", "relative/pinentry", ""] {
            let error = prompt_with(OsString::from(relative))
                .await
                .expect_err("relative executable denied");
            assert_eq!(error, CredentialEnrollmentError::Unavailable);
        }

        let directory = tempfile::tempdir().expect("temp directory");
        let error = prompt_with(directory.path().as_os_str().to_os_string())
            .await
            .expect_err("directory denied");
        assert_eq!(error, CredentialEnrollmentError::Unavailable);

        let file = directory.path().join("not-executable");
        std::fs::write(&file, "#!/bin/sh\necho OK\n").expect("write non-executable");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600))
            .expect("non-executable permissions");
        let error = prompt_with(file.into_os_string())
            .await
            .expect_err("non-executable denied");
        assert_eq!(error, CredentialEnrollmentError::Unavailable);
    }

    #[test]
    fn default_lookup_rejects_user_owned_code_and_root_owned_aliases() {
        use std::io::Write as _;
        use std::os::unix::fs::{PermissionsExt as _, symlink};

        let directory = tempfile::tempdir().expect("temp directory");
        let candidate = directory.path().join("pinentry");
        let mut file = std::fs::File::create(&candidate).expect("fake candidate");
        writeln!(file, "#!/bin/sh\necho OK").expect("fake body");
        drop(file);
        std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o700))
            .expect("fake permissions");
        let error = resolve_pinentry_program(
            None,
            Some(std::env::join_paths([directory.path()]).expect("search path")),
        )
        .expect_err("user-owned PATH code denied");
        assert_eq!(error, CredentialEnrollmentError::Unavailable);

        std::fs::remove_file(&candidate).expect("remove user-owned candidate");
        symlink("/bin/sh", &candidate).expect("root-owned executable alias");
        let error = resolve_pinentry_program(
            None,
            Some(std::env::join_paths([directory.path()]).expect("alias search path")),
        )
        .expect_err("root-owned non-pinentry alias denied");
        assert_eq!(error, CredentialEnrollmentError::Unavailable);
    }

    #[test]
    fn canonical_pinentry_names_are_closed_and_bounded() {
        for accepted in ["pinentry", "pinentry-qt", "pinentry-gtk-2"] {
            assert!(is_canonical_pinentry_name(Path::new(accepted)));
        }
        for rejected in [
            "sh",
            "pinentry-",
            "pinentry/qt",
            "pinentry-qt;sh",
            "pinentry_mac",
        ] {
            assert!(!is_canonical_pinentry_name(Path::new(rejected)));
        }
        let too_long = format!("pinentry-{}", "x".repeat(MAX_PINENTRY_BASENAME_BYTES));
        assert!(!is_canonical_pinentry_name(Path::new(&too_long)));
    }

    #[test]
    fn nix_store_sticky_group_write_is_the_only_writable_ancestor_exception() {
        if let Ok(metadata) = std::fs::metadata("/nix/store") {
            assert!(safe_default_ancestor(Path::new("/nix/store"), &metadata));
        }
        let temporary = std::fs::metadata("/tmp").expect("temporary directory metadata");
        assert!(!safe_default_ancestor(Path::new("/tmp"), &temporary));
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dropping_prompt_kills_its_direct_child_and_forked_process_group() {
        let directory = tempfile::tempdir().expect("process marker directory");
        let marker = directory.path().join("pinentry-processes");
        let sleep = std::env::split_paths(&std::env::var_os("PATH").expect("test PATH"))
            .map(|directory| directory.join("sleep"))
            .find(|candidate| candidate.is_file())
            .and_then(|candidate| candidate.canonicalize().ok())
            .expect("sleep executable");
        let script = fake_pinentry(&format!(
            r#"(
  while :; do '{}' 60; done
) &
grandchild=$!
echo "$$ $grandchild" > '{}'
echo OK
while read line; do
  case "$line" in
    GETPIN) '{}' 60 ;;
    BYE) echo OK; exit 0 ;;
    *) echo OK ;;
  esac
done"#,
            sleep.display(),
            marker.display(),
            sleep.display(),
        ));
        let path = validate_pinentry_program(&script, ProgramTrust::ExplicitDevelopmentOverride)
            .expect("validated forking pinentry");
        let task = tokio::spawn(prompt_with_program(
            PinentryProgram {
                path,
                trust: ProgramTrust::ExplicitDevelopmentOverride,
            },
            Vec::new(),
        ));

        let processes = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(value) = std::fs::read_to_string(&marker) {
                    break value;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("fake pinentry published process identities");
        let mut processes = processes.split_whitespace().map(|value| {
            value
                .parse::<u32>()
                .expect("fake pinentry process identity")
        });
        let direct_child = processes.next().expect("direct child identity");
        let grandchild = processes.next().expect("grandchild identity");
        assert!(processes.next().is_none());

        task.abort();
        let _ = task.await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while linux_process_is_live(direct_child) || linux_process_is_live(grandchild) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("pinentry process group stopped");
    }

    #[cfg(target_os = "linux")]
    fn linux_process_is_live(pid: u32) -> bool {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
            return false;
        };
        !stat
            .rsplit_once(") ")
            .and_then(|(_, suffix)| suffix.chars().next())
            .is_some_and(|state| matches!(state, 'Z' | 'X'))
    }

    #[tokio::test]
    async fn protocol_garbage_is_unavailable() {
        let error = run_with(r#"echo "TOTALLY NOT ASSUAN""#)
            .await
            .expect_err("garbage");
        assert_eq!(error, CredentialEnrollmentError::Unavailable);
    }
}
