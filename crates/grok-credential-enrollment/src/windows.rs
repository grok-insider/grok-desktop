use std::{
    ffi::c_void,
    mem,
    os::windows::ffi::OsStringExt,
    path::{Path, PathBuf},
    ptr::{self, null},
    sync::atomic::{Ordering, compiler_fence},
};

use grok_application::{CredentialEnrollmentError, SecretValue};
use windows_sys::Win32::{
    Foundation::{CloseHandle, ERROR_CANCELLED, ERROR_INSUFFICIENT_BUFFER, HANDLE, HWND},
    Security::Credentials::{
        CREDUI_FLAGS_ALWAYS_SHOW_UI, CREDUI_FLAGS_DO_NOT_PERSIST,
        CREDUI_FLAGS_EXCLUDE_CERTIFICATES, CREDUI_FLAGS_GENERIC_CREDENTIALS,
        CREDUI_FLAGS_KEEP_USERNAME, CREDUI_FLAGS_PASSWORD_ONLY_OK, CREDUI_INFOW,
        CredUIPromptForCredentialsW,
    },
    Storage::Packaging::Appx::{GetPackageFamilyName, GetPackageFullName},
    System::{
        Memory::{VirtualLock, VirtualUnlock},
        Threading::{
            GetCurrentProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
    UI::WindowsAndMessaging::GetWindowThreadProcessId,
};

use crate::{ascii_secret_length, expected_owner_executable};

const APPMODEL_ERROR_NO_PACKAGE: u32 = 15_700;
const MAX_PACKAGE_IDENTITY_CHARS: u32 = 1_024;
const MAX_PROCESS_PATH_CHARS: usize = 32_768;
const PASSWORD_CHARS: usize = 257;

#[derive(Debug, PartialEq, Eq)]
struct PackageIdentity {
    full_name: String,
    family_name: String,
}

struct ProcessHandle(HANDLE);

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: this wrapper exclusively owns the successful OpenProcess handle.
            unsafe { CloseHandle(self.0) };
        }
    }
}

struct LockedBuffer<T: Copy + Default> {
    values: Vec<T>,
    locked_bytes: usize,
}

impl<T: Copy + Default> LockedBuffer<T> {
    fn new(length: usize) -> Result<Self, CredentialEnrollmentError> {
        let values = vec![T::default(); length];
        let locked_bytes = length
            .checked_mul(mem::size_of::<T>())
            .ok_or(CredentialEnrollmentError::Unavailable)?;
        if locked_bytes == 0 {
            return Err(CredentialEnrollmentError::Unavailable);
        }
        // SAFETY: the non-empty Vec owns a contiguous initialized allocation of locked_bytes.
        if unsafe { VirtualLock(values.as_ptr().cast::<c_void>(), locked_bytes) } == 0 {
            return Err(CredentialEnrollmentError::Unavailable);
        }
        Ok(Self {
            values,
            locked_bytes,
        })
    }
}

impl LockedBuffer<u8> {
    fn into_secret(mut self, length: usize) -> Result<SecretValue, CredentialEnrollmentError> {
        if !(1..=self.values.len()).contains(&length) {
            return Err(CredentialEnrollmentError::Integrity);
        }
        // SAFETY: this allocation was locked by new and remains at the same address.
        let _ = unsafe { VirtualUnlock(self.values.as_ptr().cast::<c_void>(), self.locked_bytes) };
        self.locked_bytes = 0;
        self.values.truncate(length);
        let values = mem::take(&mut self.values);
        SecretValue::new(values).map_err(|_| CredentialEnrollmentError::Integrity)
    }
}

impl<T: Copy + Default> Drop for LockedBuffer<T> {
    fn drop(&mut self) {
        for value in &mut self.values {
            // SAFETY: value is a valid, uniquely borrowed element. Volatile writes plus the
            // compiler fence keep the erase observable before the allocation is released.
            unsafe { ptr::write_volatile(value, T::default()) };
        }
        compiler_fence(Ordering::SeqCst);
        if self.locked_bytes != 0 {
            // SAFETY: this allocation was locked by new and remains at the same address.
            let _ =
                unsafe { VirtualUnlock(self.values.as_ptr().cast::<c_void>(), self.locked_bytes) };
        }
    }
}

pub(super) fn prompt_xai_api_key(
    parent_window_token: u64,
) -> Result<SecretValue, CredentialEnrollmentError> {
    let parent_value =
        usize::try_from(parent_window_token).map_err(|_| CredentialEnrollmentError::Integrity)?;
    let parent = parent_value as HWND;
    if parent.is_null() {
        return Err(CredentialEnrollmentError::Integrity);
    }

    // Keep the qualified owner process open across the modal prompt so its PID cannot be reused.
    let (owner_process, owner_pid) = qualify_owner(parent)?;
    let mut current_owner = 0_u32;
    // SAFETY: parent is an opaque HWND; the API validates it before writing current_owner.
    if unsafe { GetWindowThreadProcessId(parent, &raw mut current_owner) } == 0
        || current_owner != owner_pid
    {
        return Err(CredentialEnrollmentError::Integrity);
    }

    let caption = wide("Grok Desktop");
    let message = wide(
        "Enter your xAI API key. Grok Desktop will validate it only with the official xAI API and store it in Windows Credential Manager.",
    );
    let target = wide("Grok Desktop xAI API key");
    let mut username = vec![0_u16; 256];
    for (target, source) in username.iter_mut().zip("xAI API key".encode_utf16()) {
        *target = source;
    }
    let mut password = LockedBuffer::<u16>::new(PASSWORD_CHARS)?;
    let info = CREDUI_INFOW {
        cbSize: u32::try_from(mem::size_of::<CREDUI_INFOW>())
            .map_err(|_| CredentialEnrollmentError::Unavailable)?,
        hwndParent: parent,
        pszMessageText: message.as_ptr(),
        pszCaptionText: caption.as_ptr(),
        hbmBanner: ptr::null_mut(),
    };
    let mut save = 0;
    let flags = CREDUI_FLAGS_ALWAYS_SHOW_UI
        | CREDUI_FLAGS_DO_NOT_PERSIST
        | CREDUI_FLAGS_EXCLUDE_CERTIFICATES
        | CREDUI_FLAGS_PASSWORD_ONLY_OK
        | CREDUI_FLAGS_GENERIC_CREDENTIALS
        | CREDUI_FLAGS_KEEP_USERNAME;
    // SAFETY: every pointer references a live, correctly sized buffer for the duration of the
    // synchronous call. The password allocation remains locked and is zeroed by Drop.
    let result = unsafe {
        CredUIPromptForCredentialsW(
            &raw const info,
            target.as_ptr(),
            null(),
            0,
            username.as_mut_ptr(),
            u32::try_from(username.len()).unwrap_or(u32::MAX),
            password.values.as_mut_ptr(),
            u32::try_from(password.values.len()).unwrap_or(u32::MAX),
            &raw mut save,
            flags,
        )
    };
    drop(owner_process);
    secure_zero(&mut username);
    if result == ERROR_CANCELLED {
        return Err(CredentialEnrollmentError::Cancelled);
    }
    if result != 0 {
        return Err(CredentialEnrollmentError::Unavailable);
    }

    let length =
        ascii_secret_length(&password.values).ok_or(CredentialEnrollmentError::Integrity)?;
    let mut output = LockedBuffer::<u8>::new(length)?;
    for (target, source) in output.values.iter_mut().zip(&password.values[..length]) {
        *target = u8::try_from(*source).map_err(|_| CredentialEnrollmentError::Integrity)?;
    }
    output.into_secret(length)
}

fn qualify_owner(parent: HWND) -> Result<(ProcessHandle, u32), CredentialEnrollmentError> {
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle for identity queries.
    let current_process = unsafe { GetCurrentProcess() };
    let daemon_path = process_executable(current_process)?;
    let expected_owner =
        expected_owner_executable(&daemon_path).ok_or(CredentialEnrollmentError::Integrity)?;
    let daemon_package =
        package_identity(current_process)?.ok_or(CredentialEnrollmentError::Integrity)?;

    let mut owner_pid = 0_u32;
    // SAFETY: parent is treated as opaque input and owner_pid is a valid output pointer.
    if unsafe { GetWindowThreadProcessId(parent, &raw mut owner_pid) } == 0 || owner_pid == 0 {
        return Err(CredentialEnrollmentError::Integrity);
    }
    // SAFETY: owner_pid came from User32 and the requested right is query-only.
    let owner = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, owner_pid) };
    if owner.is_null() {
        return Err(CredentialEnrollmentError::Integrity);
    }
    let owner = ProcessHandle(owner);
    let owner_path = process_executable(owner.0)?;
    let owner_package = package_identity(owner.0)?.ok_or(CredentialEnrollmentError::Integrity)?;
    if !paths_equal(&expected_owner, &owner_path) || daemon_package != owner_package {
        return Err(CredentialEnrollmentError::Integrity);
    }
    Ok((owner, owner_pid))
}

fn process_executable(process: HANDLE) -> Result<PathBuf, CredentialEnrollmentError> {
    let mut buffer = vec![0_u16; MAX_PROCESS_PATH_CHARS];
    let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    // SAFETY: process has query rights and buffer/length describe a writable UTF-16 allocation.
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &raw mut length) } == 0
        || length == 0
        || usize::try_from(length).map_or(true, |value| value >= buffer.len())
    {
        return Err(CredentialEnrollmentError::Integrity);
    }
    buffer.truncate(usize::try_from(length).map_err(|_| CredentialEnrollmentError::Integrity)?);
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&buffer)))
}

fn package_identity(process: HANDLE) -> Result<Option<PackageIdentity>, CredentialEnrollmentError> {
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
        _ => Err(CredentialEnrollmentError::Integrity),
    }
}

fn package_value(
    process: HANDLE,
    query: unsafe extern "system" fn(HANDLE, *mut u32, *mut u16) -> u32,
) -> Result<Option<String>, CredentialEnrollmentError> {
    let mut length = 0_u32;
    // SAFETY: the first package query requires a null buffer and returns the required length.
    let status = unsafe { query(process, &raw mut length, ptr::null_mut()) };
    if status == APPMODEL_ERROR_NO_PACKAGE {
        return Ok(None);
    }
    if status != ERROR_INSUFFICIENT_BUFFER || !(2..=MAX_PACKAGE_IDENTITY_CHARS).contains(&length) {
        return Err(CredentialEnrollmentError::Integrity);
    }
    let mut buffer = vec![0_u16; usize::try_from(length).unwrap_or(0)];
    if buffer.is_empty() {
        return Err(CredentialEnrollmentError::Integrity);
    }
    // SAFETY: buffer contains length writable UTF-16 elements and process has query rights.
    let status = unsafe { query(process, &raw mut length, buffer.as_mut_ptr()) };
    if status != 0
        || length < 2
        || usize::try_from(length).map_or(true, |value| value > buffer.len())
    {
        return Err(CredentialEnrollmentError::Integrity);
    }
    let value = usize::try_from(length - 1).map_err(|_| CredentialEnrollmentError::Integrity)?;
    buffer.truncate(value);
    String::from_utf16(&buffer)
        .map(Some)
        .map_err(|_| CredentialEnrollmentError::Integrity)
}

fn paths_equal(expected: &Path, actual: &Path) -> bool {
    expected
        .as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&actual.as_os_str().to_string_lossy())
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain([0]).collect()
}

fn secure_zero<T: Copy + Default>(values: &mut [T]) {
    for value in values {
        // SAFETY: value is a valid, uniquely borrowed element.
        unsafe { ptr::write_volatile(value, T::default()) };
    }
    compiler_fence(Ordering::SeqCst);
}
