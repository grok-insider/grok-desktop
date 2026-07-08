use std::{
    ffi::OsString,
    mem,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Path, PathBuf},
    ptr,
};

use crate::service_policy::{has_allowed_reported_service_type, has_exact_configured_service_type};
use grok_application::IsolationProbeError;
use tokio::net::windows::named_pipe::NamedPipeClient;
use windows_sys::Win32::{
    Foundation::{ERROR_INSUFFICIENT_BUFFER, GetLastError, HANDLE},
    Globalization::{CSTR_EQUAL, CompareStringOrdinal},
    Storage::Packaging::Appx::{GetCurrentPackagePath, GetPackageFamilyName, GetPackageFullName},
    System::{
        Pipes::GetNamedPipeServerProcessId,
        Services::{
            CloseServiceHandle, OpenSCManagerW, OpenServiceW, QUERY_SERVICE_CONFIGW,
            QueryServiceConfigW, QueryServiceStatusEx, SC_MANAGER_CONNECT, SC_STATUS_PROCESS_INFO,
            SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_STATUS_PROCESS,
            SERVICE_WIN32_OWN_PROCESS,
        },
        SystemServices::SERVICE_PKG_SERVICE,
        Threading::{
            GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
};

const SERVICE_NAME: &str = "GrokDesktopVmBroker";
const PACKAGE_APP_DIRECTORY: &str = "app";
const RESOURCES_DIRECTORY: &str = "resources";
const SERVICE_DIRECTORY: &str = "service";
const SERVICE_EXECUTABLE: &str = "grok-vm-service.exe";
const DAEMON_DIRECTORY: &str = "bin";
const DAEMON_EXECUTABLE: &str = "grok-daemon.exe";
const APPMODEL_ERROR_NO_PACKAGE: u32 = 15_700;
const MAX_PACKAGE_IDENTITY_CHARS: u32 = 1_024;
const MAX_PACKAGE_PATH_CHARS: u32 = 32_768;
const MAX_PROCESS_PATH_CHARS: usize = 32_768;
const MAX_SERVICE_CONFIG_BYTES: u32 = 64 * 1_024;
const LOCAL_SYSTEM_ACCOUNT: &str = "LocalSystem";
const EXPECTED_SERVICE_TYPE: u32 = SERVICE_WIN32_OWN_PROCESS | SERVICE_PKG_SERVICE;

/// Keeps the qualified service process alive so its PID cannot be reused during
/// the request. The connected pipe and this handle refer to the same SCM process.
pub(crate) struct VerifiedServerProcess {
    _process: OwnedHandle,
}

struct ServiceHandle(windows_sys::Win32::System::Services::SC_HANDLE);

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this wrapper exclusively owns a successful SCM or service handle.
            unsafe { CloseServiceHandle(self.0) };
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PackageIdentity {
    full_name: String,
    family_name: String,
}

pub(crate) fn verify_pipe_server(
    pipe: &NamedPipeClient,
) -> Result<VerifiedServerProcess, IsolationProbeError> {
    let pipe_handle = pipe.as_raw_handle() as HANDLE;
    if pipe_handle.is_null() {
        return Err(IsolationProbeError::Unqualified);
    }
    let mut server_pid = 0_u32;
    // SAFETY: pipe_handle is borrowed from a live NamedPipeClient and server_pid is writable.
    if unsafe { GetNamedPipeServerProcessId(pipe_handle, &raw mut server_pid) } == 0
        || server_pid == 0
    {
        return Err(IsolationProbeError::Unqualified);
    }
    let service = open_service()?;
    require_running_service(&service, server_pid)?;
    require_local_system_configuration(&service)?;

    // SAFETY: server_pid is kernel-reported and the requested access is query-only. Retaining
    // the resulting process-object handle prevents PID reuse until the request completes.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, server_pid) };
    if process.is_null() {
        return Err(IsolationProbeError::Unqualified);
    }
    // SAFETY: OpenProcess returned a new owned kernel handle which OwnedHandle closes on drop.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    qualify_service_process(process.as_raw_handle() as HANDLE)?;
    require_running_service(&service, server_pid)?;
    Ok(VerifiedServerProcess { _process: process })
}

fn open_service() -> Result<ServiceHandle, IsolationProbeError> {
    // SAFETY: null machine/database select the local active service-control database.
    let manager = unsafe { OpenSCManagerW(ptr::null(), ptr::null(), SC_MANAGER_CONNECT) };
    if manager.is_null() {
        return Err(IsolationProbeError::Unqualified);
    }
    let manager = ServiceHandle(manager);
    let service_name = wide(SERVICE_NAME);
    // SAFETY: manager is live and service_name is a terminated UTF-16 string.
    let service = unsafe {
        OpenServiceW(
            manager.0,
            service_name.as_ptr(),
            SERVICE_QUERY_STATUS | SERVICE_QUERY_CONFIG,
        )
    };
    if service.is_null() {
        return Err(IsolationProbeError::Unqualified);
    }
    Ok(ServiceHandle(service))
}

fn require_running_service(
    service: &ServiceHandle,
    expected_pid: u32,
) -> Result<(), IsolationProbeError> {
    let mut status = SERVICE_STATUS_PROCESS::default();
    let mut required = 0_u32;
    let status_size = u32::try_from(mem::size_of::<SERVICE_STATUS_PROCESS>())
        .map_err(|_| IsolationProbeError::Unqualified)?;
    // SAFETY: status is a correctly sized writable SERVICE_STATUS_PROCESS buffer.
    if unsafe {
        QueryServiceStatusEx(
            service.0,
            SC_STATUS_PROCESS_INFO,
            (&raw mut status).cast::<u8>(),
            status_size,
            &raw mut required,
        )
    } == 0
        // golang.org/x/sys v0.31.0 reports the base own-process type from
        // SetServiceStatus. The separately queried SCM configuration remains
        // the package-authority check.
        || !has_allowed_reported_service_type(
            status.dwServiceType,
            SERVICE_WIN32_OWN_PROCESS,
            EXPECTED_SERVICE_TYPE,
        )
        || status.dwCurrentState != SERVICE_RUNNING
        || status.dwProcessId != expected_pid
        || status.dwServiceFlags != 0
    {
        return Err(IsolationProbeError::Unqualified);
    }
    Ok(())
}

fn require_local_system_configuration(service: &ServiceHandle) -> Result<(), IsolationProbeError> {
    let mut required = 0_u32;
    // SAFETY: this size query deliberately supplies no configuration buffer.
    let first = unsafe { QueryServiceConfigW(service.0, ptr::null_mut(), 0, &raw mut required) };
    // SAFETY: GetLastError immediately observes the failed size query above.
    let first_error = unsafe { GetLastError() };
    let minimum = u32::try_from(mem::size_of::<QUERY_SERVICE_CONFIGW>())
        .map_err(|_| IsolationProbeError::Unqualified)?;
    if first != 0
        || first_error != ERROR_INSUFFICIENT_BUFFER
        || !(minimum..=MAX_SERVICE_CONFIG_BYTES).contains(&required)
    {
        return Err(IsolationProbeError::Unqualified);
    }
    let word = mem::size_of::<usize>();
    let bytes = usize::try_from(required).map_err(|_| IsolationProbeError::Unqualified)?;
    let words = aligned_word_count(bytes, word)?;
    let mut buffer = vec![0_usize; words];
    let mut returned = 0_u32;
    // SAFETY: the aligned usize buffer contains at least required writable bytes.
    if unsafe {
        QueryServiceConfigW(
            service.0,
            buffer.as_mut_ptr().cast::<QUERY_SERVICE_CONFIGW>(),
            required,
            &raw mut returned,
        )
    } == 0
    {
        return Err(IsolationProbeError::Unqualified);
    }
    // SAFETY: the successful query initialized the aligned QUERY_SERVICE_CONFIGW prefix.
    let configuration = unsafe { buffer.as_ptr().cast::<QUERY_SERVICE_CONFIGW>().read() };
    if !has_exact_configured_service_type(configuration.dwServiceType, EXPECTED_SERVICE_TYPE) {
        return Err(IsolationProbeError::Unqualified);
    }
    let account = bounded_utf16(
        configuration.lpServiceStartName,
        buffer.as_ptr().cast::<u8>(),
        bytes,
    )
    .ok_or(IsolationProbeError::Unqualified)?;
    if !utf16_eq_ascii_case(&account, LOCAL_SYSTEM_ACCOUNT) {
        return Err(IsolationProbeError::Unqualified);
    }
    Ok(())
}

fn qualify_service_process(process: HANDLE) -> Result<(), IsolationProbeError> {
    let service_path = process_executable(process)?;

    // SAFETY: GetCurrentProcess returns a valid pseudo-handle for query APIs.
    let current = unsafe { GetCurrentProcess() };
    let daemon_path = process_executable(current)?;
    let package_root = current_package_root()?;
    let (expected_daemon, expected_service) =
        expected_packaged_executables(&package_root).ok_or(IsolationProbeError::Unqualified)?;
    if !paths_equal(&expected_daemon, &daemon_path)
        || !paths_equal(&expected_service, &service_path)
    {
        return Err(IsolationProbeError::Unqualified);
    }
    let daemon_package = package_identity(current)?.ok_or(IsolationProbeError::Unqualified)?;
    let service_package = package_identity(process)?.ok_or(IsolationProbeError::Unqualified)?;
    if daemon_package != service_package {
        return Err(IsolationProbeError::Unqualified);
    }
    Ok(())
}

fn process_executable(process: HANDLE) -> Result<PathBuf, IsolationProbeError> {
    let mut buffer = vec![0_u16; MAX_PROCESS_PATH_CHARS];
    let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    // SAFETY: process has query rights and buffer/length describe writable UTF-16 storage.
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &raw mut length) } == 0
        || length == 0
        || usize::try_from(length).map_or(true, |value| value >= buffer.len())
    {
        return Err(IsolationProbeError::Unqualified);
    }
    buffer.truncate(usize::try_from(length).map_err(|_| IsolationProbeError::Unqualified)?);
    Ok(PathBuf::from(OsString::from_wide(&buffer)))
}

fn current_package_root() -> Result<PathBuf, IsolationProbeError> {
    let mut length = 0_u32;
    // SAFETY: the first query deliberately supplies no path buffer.
    let status = unsafe { GetCurrentPackagePath(&raw mut length, ptr::null_mut()) };
    if status != ERROR_INSUFFICIENT_BUFFER || !(2..=MAX_PACKAGE_PATH_CHARS).contains(&length) {
        return Err(IsolationProbeError::Unqualified);
    }
    let mut buffer = vec![0_u16; usize::try_from(length).unwrap_or(0)];
    if buffer.is_empty() {
        return Err(IsolationProbeError::Unqualified);
    }
    // SAFETY: buffer contains length writable UTF-16 elements.
    let status = unsafe { GetCurrentPackagePath(&raw mut length, buffer.as_mut_ptr()) };
    if status != 0
        || length < 2
        || usize::try_from(length).map_or(true, |value| value > buffer.len())
    {
        return Err(IsolationProbeError::Unqualified);
    }
    let final_index = usize::try_from(length - 1).map_err(|_| IsolationProbeError::Unqualified)?;
    if buffer.get(final_index) != Some(&0) {
        return Err(IsolationProbeError::Unqualified);
    }
    buffer.truncate(final_index);
    let root = PathBuf::from(OsString::from_wide(&buffer));
    if !root.is_absolute() {
        return Err(IsolationProbeError::Unqualified);
    }
    Ok(root)
}

fn package_identity(process: HANDLE) -> Result<Option<PackageIdentity>, IsolationProbeError> {
    let full_name = package_value(process, GetPackageFullName)?;
    let family_name = package_value(process, GetPackageFamilyName)?;
    match (full_name, family_name) {
        (Some(full_name), Some(family_name))
            if !full_name.is_empty() && !family_name.is_empty() =>
        {
            Ok(Some(PackageIdentity {
                full_name,
                family_name,
            }))
        }
        (None, None) => Ok(None),
        _ => Err(IsolationProbeError::Unqualified),
    }
}

fn package_value(
    process: HANDLE,
    query: unsafe extern "system" fn(HANDLE, *mut u32, *mut u16) -> u32,
) -> Result<Option<String>, IsolationProbeError> {
    let mut length = 0_u32;
    // SAFETY: this size query deliberately supplies a null output buffer.
    let status = unsafe { query(process, &raw mut length, ptr::null_mut()) };
    if status == APPMODEL_ERROR_NO_PACKAGE {
        return Ok(None);
    }
    if status != ERROR_INSUFFICIENT_BUFFER || !(2..=MAX_PACKAGE_IDENTITY_CHARS).contains(&length) {
        return Err(IsolationProbeError::Unqualified);
    }
    let mut buffer = vec![0_u16; usize::try_from(length).unwrap_or(0)];
    if buffer.is_empty() {
        return Err(IsolationProbeError::Unqualified);
    }
    // SAFETY: buffer contains length writable UTF-16 elements.
    let status = unsafe { query(process, &raw mut length, buffer.as_mut_ptr()) };
    if status != 0
        || length < 2
        || usize::try_from(length).map_or(true, |value| value > buffer.len())
    {
        return Err(IsolationProbeError::Unqualified);
    }
    let final_index = usize::try_from(length - 1).map_err(|_| IsolationProbeError::Unqualified)?;
    if buffer.get(final_index) != Some(&0) {
        return Err(IsolationProbeError::Unqualified);
    }
    buffer.truncate(final_index);
    String::from_utf16(&buffer)
        .map(Some)
        .map_err(|_| IsolationProbeError::Unqualified)
}

fn expected_packaged_executables(package_root: &Path) -> Option<(PathBuf, PathBuf)> {
    if !package_root.is_absolute() {
        return None;
    }
    let resources = package_root
        .join(PACKAGE_APP_DIRECTORY)
        .join(RESOURCES_DIRECTORY);
    Some((
        resources.join(DAEMON_DIRECTORY).join(DAEMON_EXECUTABLE),
        resources.join(SERVICE_DIRECTORY).join(SERVICE_EXECUTABLE),
    ))
}

fn paths_equal(expected: &Path, actual: &Path) -> bool {
    let expected: Vec<u16> = expected.as_os_str().encode_wide().collect();
    let actual: Vec<u16> = actual.as_os_str().encode_wide().collect();
    let (Ok(expected_length), Ok(actual_length)) =
        (i32::try_from(expected.len()), i32::try_from(actual.len()))
    else {
        return false;
    };
    if expected.is_empty() || actual.is_empty() {
        return false;
    }
    // SAFETY: both pointers reference initialized UTF-16 buffers for their explicit lengths.
    unsafe {
        CompareStringOrdinal(
            expected.as_ptr(),
            expected_length,
            actual.as_ptr(),
            actual_length,
            1,
        ) == CSTR_EQUAL
    }
}

fn aligned_word_count(bytes: usize, word: usize) -> Result<usize, IsolationProbeError> {
    bytes
        .checked_add(word.saturating_sub(1))
        .and_then(|value| value.checked_div(word))
        .filter(|value| *value > 0)
        .ok_or(IsolationProbeError::Unqualified)
}

fn bounded_utf16(pointer: *const u16, base: *const u8, bytes: usize) -> Option<Vec<u16>> {
    if pointer.is_null() || base.is_null() || bytes == 0 {
        return None;
    }
    let base = base as usize;
    let end = base.checked_add(bytes)?;
    let start = pointer as usize;
    if start < base || start >= end || !start.is_multiple_of(mem::align_of::<u16>()) {
        return None;
    }
    let characters = end.checked_sub(start)?.checked_div(mem::size_of::<u16>())?;
    if characters == 0 {
        return None;
    }
    // SAFETY: start was checked to lie in the live aligned buffer and characters cannot pass end.
    let values = unsafe { std::slice::from_raw_parts(pointer, characters) };
    let terminator = values.iter().position(|value| *value == 0)?;
    (terminator > 0).then(|| values[..terminator].to_vec())
}

fn utf16_eq_ascii_case(value: &[u16], expected: &str) -> bool {
    value.len() == expected.len()
        && value.iter().zip(expected.bytes()).all(|(left, right)| {
            u8::try_from(*left).is_ok_and(|left| left.eq_ignore_ascii_case(&right))
        })
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packaged_layout_is_anchored_to_the_package_root() {
        let root = Path::new(r"C:\Program Files\WindowsApps\Grok");
        assert_eq!(
            expected_packaged_executables(root),
            Some((
                PathBuf::from(
                    r"C:\Program Files\WindowsApps\Grok\app\resources\bin\grok-daemon.exe"
                ),
                PathBuf::from(
                    r"C:\Program Files\WindowsApps\Grok\app\resources\service\grok-vm-service.exe"
                )
            ))
        );
        assert!(expected_packaged_executables(Path::new("relative")).is_none());
    }

    #[test]
    fn service_account_policy_accepts_only_local_system_ascii() {
        assert!(utf16_eq_ascii_case(
            &"localsystem".encode_utf16().collect::<Vec<_>>(),
            LOCAL_SYSTEM_ACCOUNT
        ));
        assert!(!utf16_eq_ascii_case(
            &"NT AUTHORITY\\SYSTEM".encode_utf16().collect::<Vec<_>>(),
            LOCAL_SYSTEM_ACCOUNT
        ));
    }
}
