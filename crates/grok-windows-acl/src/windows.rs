use std::{
    ffi::c_void,
    fs::File,
    io,
    mem::{size_of, size_of_val},
    os::windows::{
        ffi::{OsStrExt as _, OsStringExt as _},
        fs::MetadataExt as _,
        io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle, RawHandle},
    },
    path::{Path, PathBuf},
    ptr::{addr_of, null, null_mut},
    slice,
};

use windows_sys::Win32::{
    Foundation::{
        CloseHandle, ERROR_INSUFFICIENT_BUFFER, ERROR_SUCCESS, GENERIC_READ, GENERIC_WRITE,
        GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
    },
    Security::{
        ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
        Authorization::{
            ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
            GetSecurityInfo, SDDL_REVISION_1, SE_FILE_OBJECT, SetSecurityInfo,
        },
        CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation,
        GetLengthSid, GetSecurityDescriptorControl, GetSecurityDescriptorDacl,
        GetSecurityDescriptorOwner, GetTokenInformation, OBJECT_INHERIT_ACE,
        OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        PSID, SE_DACL_DEFAULTED, SE_DACL_PRESENT, SE_DACL_PROTECTED, SE_OWNER_DEFAULTED,
        SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    },
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_ALL_ACCESS,
        FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, FileRenameInfo, GetFileInformationByHandle, OPEN_EXISTING, READ_CONTROL,
        SetFileInformationByHandle, WRITE_DAC, WRITE_OWNER,
    },
    System::{
        Pipes::GetNamedPipeClientProcessId,
        Threading::{
            GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    },
};

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};

use crate::{PrivateObjectKind, private_sddl};

const ACCESS_ALLOWED_ACE_TYPE: u8 = 0;
const MAX_PROCESS_PATH_CHARS: usize = 32_768;

/// Retains the kernel process object used to qualify a named-pipe client.
///
/// Keeping this value alive while the connection is served prevents PID reuse
/// from changing the identity associated with the qualified pipe client.
pub struct VerifiedNamedPipeClient {
    _process: OwnedHandle,
}

/// Creates the first, local-only instance of an owner-only named pipe.
///
/// # Errors
///
/// Returns an operating-system error when the current token cannot produce the
/// private security descriptor or the pipe cannot be created atomically with it.
pub fn create_private_named_pipe_server(
    name: &str,
    first_instance: bool,
) -> io::Result<NamedPipeServer> {
    if !name.starts_with(r"\\.\pipe\grok-desktop-host-tools-") || name.len() > 512 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid private pipe name",
        ));
    }
    let descriptor = LocalDescriptor::private(PrivateObjectKind::File)?;
    let mut attributes = descriptor.security_attributes();
    let mut options = ServerOptions::new();
    options
        .first_pipe_instance(first_instance)
        .reject_remote_clients(true);
    // SAFETY: attributes points into descriptor, which remains live throughout
    // the synchronous CreateNamedPipeW call performed by Tokio.
    unsafe { options.create_with_security_attributes_raw(name, (&raw mut attributes).cast()) }
}

/// Verifies that the connected pipe client is the expected executable.
///
/// # Errors
///
/// Returns an operating-system error if the kernel-reported client PID cannot
/// be held open or its executable path does not match the expected absolute path.
pub fn verify_named_pipe_client_executable(
    pipe: &NamedPipeServer,
    expected: &Path,
) -> io::Result<VerifiedNamedPipeClient> {
    if !expected.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected path is not absolute",
        ));
    }
    let handle = pipe.as_raw_handle() as HANDLE;
    let mut pid = 0_u32;
    // SAFETY: handle is borrowed from a connected live server and pid is writable.
    if handle.is_null()
        || unsafe { GetNamedPipeClientProcessId(handle, &raw mut pid) } == 0
        || pid == 0
    {
        return Err(last_error());
    }
    // SAFETY: pid is kernel-reported for this pipe and access is query-only.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Err(last_error());
    }
    // SAFETY: OpenProcess returned a new owned handle.
    let process = unsafe { OwnedHandle::from_raw_handle(process) };
    let actual = process_executable(process.as_raw_handle() as HANDLE)?;
    let expected = expected.canonicalize()?;
    if !expected
        .as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&actual.as_os_str().to_string_lossy())
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "pipe client identity mismatch",
        ));
    }
    Ok(VerifiedNamedPipeClient { _process: process })
}

fn process_executable(process: HANDLE) -> io::Result<PathBuf> {
    let mut buffer = vec![0_u16; MAX_PROCESS_PATH_CHARS];
    let mut length = u32::try_from(buffer.len()).unwrap_or(u32::MAX);
    // SAFETY: process has query rights and the UTF-16 buffer is writable.
    if unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &raw mut length) } == 0
        || length == 0
        || usize::try_from(length).map_or(true, |value| value >= buffer.len())
    {
        return Err(last_error());
    }
    buffer
        .truncate(usize::try_from(length).map_err(|_| io::Error::other("process path overflow"))?);
    Ok(PathBuf::from(std::ffi::OsString::from_wide(&buffer)))
}

/// Creates a directory with its owner-only protected DACL applied atomically.
///
/// # Errors
///
/// Returns an operating-system error when the current token, descriptor, path,
/// or directory creation operation cannot satisfy the private ACL contract.
pub fn create_private_directory(path: &Path) -> io::Result<()> {
    let path = wide_path(path)?;
    let descriptor = LocalDescriptor::private(PrivateObjectKind::Directory)?;
    let attributes = descriptor.security_attributes();
    if unsafe { CreateDirectoryW(path.as_ptr(), &raw const attributes) } == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Opens a file without following a final reparse point.
///
/// New files receive the owner-only protected DACL as part of `CreateFileW`.
///
/// # Errors
///
/// Returns an operating-system error when the current token, descriptor, path,
/// or requested file operation is invalid or unavailable.
pub fn open_private_file(
    path: &Path,
    read: bool,
    write: bool,
    create_new: bool,
) -> io::Result<File> {
    let share_mode = if create_new {
        // Atomic hard-link publication opens the source and removes the
        // still-open temporary name. Keep all three sharing modes compatible
        // while the original handle pins the file identity.
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE
    } else if read && !write {
        // Readers may coexist, but no writer or deleter can race verification.
        FILE_SHARE_READ
    } else {
        0
    };
    open_file(path, read, write, create_new, share_mode)
}

/// Opens a runtime lock file with sharing enabled for competing lock owners.
///
/// The byte-range lock, rather than a share-mode denial, is the ownership
/// primitive. New files still receive the private ACL atomically.
///
/// # Errors
///
/// Returns an operating-system error when the lock path cannot be opened
/// without following a final reparse point.
pub fn open_private_lock_file(path: &Path, create_new: bool) -> io::Result<File> {
    open_file(
        path,
        true,
        true,
        create_new,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
    )
}

fn open_file(
    path: &Path,
    read: bool,
    write: bool,
    create_new: bool,
    share_mode: u32,
) -> io::Result<File> {
    let path = wide_path(path)?;
    let descriptor = create_new
        .then(|| LocalDescriptor::private(PrivateObjectKind::File))
        .transpose()?;
    let attributes = descriptor
        .as_ref()
        .map(LocalDescriptor::security_attributes);
    let mut access = READ_CONTROL;
    if read {
        access |= GENERIC_READ;
    }
    if write {
        access |= GENERIC_WRITE;
    }
    if create_new {
        access |= WRITE_DAC | WRITE_OWNER | windows_sys::Win32::Storage::FileSystem::DELETE;
    }
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            share_mode,
            attributes.as_ref().map_or(null(), |value| value),
            if create_new {
                CREATE_NEW
            } else {
                OPEN_EXISTING
            },
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    file_from_handle(handle)
}

/// Atomically publishes an open private file under a new absolute path.
///
/// The operation renames the object identified by `file` without replacing an
/// existing destination. Keeping publication handle-based prevents a path
/// substitution between content verification and publication.
///
/// # Errors
///
/// Returns an operating-system error when the destination is invalid, already
/// exists, or cannot be created on the same volume as the open file.
pub fn publish_private_file(file: &File, destination: &Path) -> io::Result<()> {
    if !destination.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "publication path is not absolute",
        ));
    }
    let destination = wide_path(destination)?;
    let name_bytes = destination
        .len()
        .checked_sub(1)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "publication path is empty"))?
        .checked_mul(size_of::<u16>())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "publication path is oversized")
        })?;
    let terminated_name_bytes =
        destination
            .len()
            .checked_mul(size_of::<u16>())
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "publication path is oversized")
            })?;
    let header_bytes = std::mem::offset_of!(FILE_RENAME_INFO, FileName);
    let buffer_bytes = header_bytes
        .checked_add(terminated_name_bytes)
        .ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "publication path is oversized")
        })?;
    let buffer_size = u32::try_from(buffer_bytes).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "publication path is oversized")
    })?;
    let mut buffer = vec![0_usize; buffer_bytes.div_ceil(size_of::<usize>())];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    unsafe {
        (*information).Anonymous.ReplaceIfExists = false;
        (*information).RootDirectory = null_mut();
        (*information).FileNameLength = name_bytes;
        std::ptr::copy_nonoverlapping(
            destination.as_ptr(),
            (*information).FileName.as_mut_ptr(),
            destination.len(),
        );
    }
    if unsafe {
        SetFileInformationByHandle(
            raw_handle(file),
            FileRenameInfo,
            information.cast(),
            buffer_size,
        )
    } == 0
    {
        return Err(last_error());
    }
    Ok(())
}

/// Reapplies the exact owner-only protected DACL through an existing handle.
///
/// # Errors
///
/// Returns an operating-system error when the descriptor cannot be constructed
/// or the handle does not permit owner and DACL changes.
pub fn apply_private_acl(file: &File, kind: PrivateObjectKind) -> io::Result<()> {
    let descriptor = LocalDescriptor::private(kind)?;
    let (owner, dacl) = descriptor.owner_and_dacl()?;
    let status = unsafe {
        SetSecurityInfo(
            raw_handle(file),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION
                | DACL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            owner,
            null_mut(),
            dacl,
            null(),
        )
    };
    win32_status(status)
}

/// Reapplies the exact owner-only protected directory DACL through a directory handle.
///
/// # Errors
///
/// Returns an operating-system error when the directory cannot be opened safely
/// or its owner and DACL cannot be replaced.
pub fn apply_private_directory_acl(path: &Path) -> io::Result<()> {
    let directory = open_directory(path, true)?;
    apply_private_acl(&directory, PrivateObjectKind::Directory)
}

/// Verifies that a handle is owned by the current token and has exactly one protected owner ACE.
///
/// # Errors
///
/// Returns an operating-system error when token or security-descriptor metadata
/// cannot be queried. A well-formed but unsafe ACL returns `Ok(false)`.
pub fn verify_private_acl(file: &File, kind: PrivateObjectKind) -> io::Result<bool> {
    let expected = LocalDescriptor::private(kind)?;
    let (expected_owner, _) = expected.owner_and_dacl()?;
    let actual = LocalDescriptor::from_handle(file)?;
    let (owner, dacl) = actual.owner_and_dacl()?;
    if unsafe { EqualSid(owner, expected_owner) } == 0 {
        return Ok(false);
    }

    let mut control = 0;
    let mut revision = 0;
    if unsafe { GetSecurityDescriptorControl(actual.0, &raw mut control, &raw mut revision) } == 0 {
        return Err(last_error());
    }
    if control & (SE_DACL_PRESENT | SE_DACL_PROTECTED) != SE_DACL_PRESENT | SE_DACL_PROTECTED
        || control & (SE_DACL_DEFAULTED | SE_OWNER_DEFAULTED) != 0
    {
        return Ok(false);
    }
    verify_exact_acl(dacl, expected_owner, kind)
}

/// Opens and verifies a directory without following a final reparse point.
///
/// # Errors
///
/// Returns an operating-system error when the directory or its security metadata
/// cannot be opened. A well-formed but unsafe object returns `Ok(false)`.
pub fn verify_private_directory(path: &Path) -> io::Result<bool> {
    let directory = open_directory(path, false)?;
    let metadata = directory.metadata()?;
    if !metadata.is_dir() || metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Ok(false);
    }
    verify_private_acl(&directory, PrivateObjectKind::Directory)
}

/// Returns whether a file handle identifies an object with exactly one hard link.
///
/// # Errors
///
/// Returns an operating-system error when handle metadata cannot be queried.
pub fn file_has_single_link(file: &File) -> io::Result<bool> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(raw_handle(file), &raw mut information) } == 0 {
        return Err(last_error());
    }
    Ok(information.nNumberOfLinks == 1)
}

fn open_directory(path: &Path, write_security: bool) -> io::Result<File> {
    let path = wide_path(path)?;
    let mut access = READ_CONTROL;
    if write_security {
        access |= WRITE_DAC | WRITE_OWNER;
    }
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    file_from_handle(handle)
}

fn verify_exact_acl(dacl: *mut ACL, owner: PSID, kind: PrivateObjectKind) -> io::Result<bool> {
    let mut information = ACL_SIZE_INFORMATION::default();
    if unsafe {
        GetAclInformation(
            dacl,
            (&raw mut information).cast(),
            u32::try_from(size_of_val(&information)).expect("ACL information size fits u32"),
            AclSizeInformation,
        )
    } == 0
    {
        return Err(last_error());
    }
    if information.AceCount != 1 {
        return Ok(false);
    }
    let mut raw_ace = null_mut();
    if unsafe { GetAce(dacl, 0, &raw mut raw_ace) } == 0 {
        return Err(last_error());
    }
    let ace = unsafe { &*raw_ace.cast::<ACCESS_ALLOWED_ACE>() };
    let expected_flags = match kind {
        PrivateObjectKind::Directory => u8::try_from(OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE)
            .expect("directory inheritance flags fit u8"),
        PrivateObjectKind::File => 0,
    };
    if ace.Header.AceType != ACCESS_ALLOWED_ACE_TYPE
        || ace.Header.AceFlags != expected_flags
        || usize::from(ace.Header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>()
        || ace.Mask != FILE_ALL_ACCESS
    {
        return Ok(false);
    }
    let ace_sid = addr_of!(ace.SidStart).cast_mut().cast::<c_void>();
    let sid_length = unsafe { GetLengthSid(owner) };
    let expected_ace_size = u32::try_from(size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>())
        .expect("ACE prefix size fits u32")
        .saturating_add(sid_length);
    if sid_length == 0
        || u32::from(ace.Header.AceSize) != expected_ace_size
        || information.AclBytesInUse
            != u32::try_from(size_of::<ACL>())
                .expect("ACL header size fits u32")
                .saturating_add(expected_ace_size)
    {
        return Ok(false);
    }
    Ok(unsafe { EqualSid(ace_sid, owner) } != 0)
}

struct LocalDescriptor(PSECURITY_DESCRIPTOR);

impl LocalDescriptor {
    fn private(kind: PrivateObjectKind) -> io::Result<Self> {
        let owner = current_user_sid_string()?;
        let sddl = private_sddl(&owner, kind).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "current user SID is invalid")
        })?;
        Self::from_sddl(&sddl)
    }

    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let mut wide = sddl.encode_utf16().collect::<Vec<_>>();
        wide.push(0);
        let mut descriptor = null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(last_error());
        }
        if descriptor.is_null() {
            return Err(io::Error::other(
                "Win32 returned an empty security descriptor",
            ));
        }
        Ok(Self(descriptor))
    }

    fn from_handle(file: &File) -> io::Result<Self> {
        let mut descriptor = null_mut();
        let status = unsafe {
            GetSecurityInfo(
                raw_handle(file),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null_mut(),
                null_mut(),
                &raw mut descriptor,
            )
        };
        win32_status(status)?;
        if descriptor.is_null() {
            return Err(io::Error::other(
                "Win32 returned an empty security descriptor",
            ));
        }
        Ok(Self(descriptor))
    }

    fn owner_and_dacl(&self) -> io::Result<(PSID, *mut ACL)> {
        let mut owner = null_mut();
        let mut owner_defaulted = 0;
        if unsafe { GetSecurityDescriptorOwner(self.0, &raw mut owner, &raw mut owner_defaulted) }
            == 0
        {
            return Err(last_error());
        }
        let mut present = 0;
        let mut dacl = null_mut();
        let mut dacl_defaulted = 0;
        if unsafe {
            GetSecurityDescriptorDacl(
                self.0,
                &raw mut present,
                &raw mut dacl,
                &raw mut dacl_defaulted,
            )
        } == 0
        {
            return Err(last_error());
        }
        if owner.is_null()
            || dacl.is_null()
            || present == 0
            || owner_defaulted != 0
            || dacl_defaulted != 0
        {
            return Err(io::Error::other("security descriptor is incomplete"));
        }
        Ok((owner, dacl))
    }

    fn security_attributes(&self) -> SECURITY_ATTRIBUTES {
        SECURITY_ATTRIBUTES {
            nLength: u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
                .expect("security attributes size fits u32"),
            lpSecurityDescriptor: self.0,
            bInheritHandle: 0,
        }
    }
}

impl Drop for LocalDescriptor {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0);
            }
        }
    }
}

struct OwnedWinHandle(HANDLE);

impl Drop for OwnedWinHandle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
}

struct LocalWide(*mut u16);

impl Drop for LocalWide {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                LocalFree(self.0.cast());
            }
        }
    }
}

fn current_user_sid_string() -> io::Result<String> {
    let mut token = null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) } == 0 {
        return Err(last_error());
    }
    let token = OwnedWinHandle(token);
    let mut length = 0;
    let first = unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &raw mut length) };
    if first != 0 || length == 0 || unsafe { GetLastError() } != ERROR_INSUFFICIENT_BUFFER {
        return Err(last_error());
    }
    let words = (length as usize).div_ceil(size_of::<usize>());
    let mut buffer = vec![0_usize; words];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            buffer.as_mut_ptr().cast(),
            length,
            &raw mut length,
        )
    } == 0
    {
        return Err(last_error());
    }
    let token_user = unsafe { &*buffer.as_ptr().cast::<TOKEN_USER>() };
    let mut string_sid = null_mut();
    if unsafe { ConvertSidToStringSidW(token_user.User.Sid, &raw mut string_sid) } == 0 {
        return Err(last_error());
    }
    let string_sid = LocalWide(string_sid);
    let mut length = 0_usize;
    while unsafe { *string_sid.0.add(length) } != 0 {
        length += 1;
        if length > 184 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "current user SID is oversized",
            ));
        }
    }
    String::from_utf16(unsafe { slice::from_raw_parts(string_sid.0, length) })
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "current user SID is invalid"))
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut value = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if value.is_empty() || value.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Windows path is invalid",
        ));
    }
    value.push(0);
    Ok(value)
}

fn raw_handle(file: &File) -> HANDLE {
    file.as_raw_handle().cast()
}

fn file_from_handle(handle: HANDLE) -> io::Result<File> {
    if handle == INVALID_HANDLE_VALUE || handle.is_null() {
        return Err(last_error());
    }
    Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
}

fn win32_status(status: u32) -> io::Result<()> {
    if status == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status.cast_signed()))
    }
}

fn last_error() -> io::Error {
    io::Error::last_os_error()
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::UNPROTECTED_DACL_SECURITY_INFORMATION;

    #[test]
    fn creates_and_verifies_owner_only_files_and_directories() {
        let root = tempfile::tempdir().expect("tempdir");
        let directory = root.path().join("private");
        create_private_directory(&directory).expect("create directory");
        assert!(verify_private_directory(&directory).expect("verify directory"));

        let file_path = directory.join("secret.dat");
        let file = open_private_file(&file_path, true, true, true).expect("create file");
        assert!(verify_private_acl(&file, PrivateObjectKind::File).expect("verify file"));
        assert!(file_has_single_link(&file).expect("link count"));
    }

    #[test]
    fn publishes_a_private_file_by_its_open_handle() {
        let root = tempfile::tempdir().expect("tempdir");
        let temporary = root.path().join("managed.tmp");
        let published = root.path().join("managed.toml");
        let file = open_private_file(&temporary, false, true, true).expect("create file");

        publish_private_file(&file, &published).expect("publish file");

        assert!(!temporary.exists());
        assert!(published.is_file());
        assert!(file_has_single_link(&file).expect("published link count"));
        assert!(verify_private_acl(&file, PrivateObjectKind::File).expect("verify file"));
    }

    #[test]
    fn rejects_unprotected_or_world_accessible_dacls() {
        let root = tempfile::tempdir().expect("tempdir");
        let file_path = root.path().join("unsafe.dat");
        let file = open_private_file(&file_path, true, true, true).expect("create file");
        let owner = current_user_sid_string().expect("current owner");
        let descriptor = LocalDescriptor::from_sddl(&format!("O:{owner}D:(A;;FA;;;WD)"))
            .expect("world descriptor");
        let (_, dacl) = descriptor.owner_and_dacl().expect("dacl");
        let status = unsafe {
            SetSecurityInfo(
                raw_handle(&file),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | UNPROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            )
        };
        win32_status(status).expect("replace dacl");
        assert!(!verify_private_acl(&file, PrivateObjectKind::File).expect("reject dacl"));
    }
}
