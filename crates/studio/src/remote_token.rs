//! Private token files, with platform-specific permission checks below.

use std::io::{self, Write};
use tempfile::NamedTempFile;

pub(super) fn create(token: &str) -> io::Result<NamedTempFile> {
    // Keep the file open until the listener stops, then delete it.
    #[cfg(windows)]
    let mut file = windows::create()?;
    #[cfg(not(windows))]
    let mut file = tempfile::Builder::new()
        .prefix("rustel-studio-remote-")
        .suffix(".token")
        .tempfile()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(target_os = "macos")]
    macos::require_private(file.as_file())?;
    writeln!(file, "{token}")?;
    file.flush()?;
    Ok(file)
}

#[cfg(target_os = "macos")]
mod macos {
    //! ACLs can override mode 0600. Check before writing the token:
    //! removing an ACL later would not revoke an already open handle.

    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd;
    use std::ptr;

    use libc::{c_int, c_void};

    const ACL_TYPE_EXTENDED: c_int = 0x100;
    const ACL_FIRST_ENTRY: c_int = 0;

    unsafe extern "C" {
        fn acl_get_fd_np(fd: c_int, acl_type: c_int) -> *mut c_void;
        fn acl_valid(acl: *mut c_void) -> c_int;
        fn acl_get_entry(acl: *mut c_void, entry_id: c_int, entry: *mut *mut c_void) -> c_int;
        fn acl_free(value: *mut c_void) -> c_int;
    }

    struct Acl(*mut c_void);

    impl Drop for Acl {
        fn drop(&mut self) {
            // SAFETY: this is the allocation returned by a successful ACL API call.
            unsafe { acl_free(self.0) };
        }
    }

    pub(super) fn require_private(file: &File) -> io::Result<()> {
        let mut filesystem = std::mem::MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: the open file descriptor is valid and the output is writable.
        if unsafe { libc::fstatfs(file.as_raw_fd(), filesystem.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fstatfs initialized the structure on success.
        require_private_volume(unsafe { filesystem.assume_init() }.f_flags)?;

        // SAFETY: the descriptor remains valid throughout this call.
        let raw_acl = unsafe { acl_get_fd_np(file.as_raw_fd(), ACL_TYPE_EXTENDED) };
        if raw_acl.is_null() {
            let error = io::Error::last_os_error();
            // Darwin reports ENOENT when an open file has no extended ACL.
            // Reject other errors: we cannot tell who has access.
            return if error.raw_os_error() == Some(libc::ENOENT) {
                Ok(())
            } else {
                Err(error)
            };
        }
        require_empty_acl(&Acl(raw_acl))
    }

    fn require_private_volume(flags: u32) -> io::Result<()> {
        if flags & libc::MNT_LOCAL as u32 == 0 || flags & libc::MNT_IGNORE_OWNERSHIP as u32 != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote control requires a local temporary filesystem that enforces ownership",
            ));
        }
        Ok(())
    }

    fn require_empty_acl(acl: &Acl) -> io::Result<()> {
        // SAFETY: Acl owns a live allocation returned by the native ACL APIs.
        if unsafe { acl_valid(acl.0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut entry = ptr::null_mut();
        // Darwin returns 0 when an entry exists, and -1/EINVAL at end of list.
        // SAFETY: the ACL is valid and entry points to writable output storage.
        if unsafe { acl_get_entry(acl.0, ACL_FIRST_ENTRY, &mut entry) } == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "remote control requires a temporary directory without inherited access-control entries",
            ));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINVAL) {
            Ok(())
        } else {
            Err(error)
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        unsafe extern "C" {
            fn acl_init(count: c_int) -> *mut c_void;
            fn acl_create_entry(acl: *mut *mut c_void, entry: *mut *mut c_void) -> c_int;
        }

        #[test]
        fn normal_private_temporary_file_is_accepted() {
            let file = tempfile::NamedTempFile::new().unwrap();
            require_private(file.as_file()).unwrap();
        }

        #[test]
        fn extended_entries_are_rejected_without_changing_file_permissions() {
            // Keep the test ACL in memory; do not grant actual file access.
            let mut acl = Acl(unsafe { acl_init(1) });
            assert!(!acl.0.is_null());
            require_empty_acl(&acl).unwrap();
            let mut entry = ptr::null_mut();
            assert_eq!(unsafe { acl_create_entry(&mut acl.0, &mut entry) }, 0);
            assert_eq!(
                require_empty_acl(&acl).unwrap_err().kind(),
                io::ErrorKind::PermissionDenied
            );
        }

        #[test]
        fn remote_or_ownership_ignoring_volumes_are_rejected() {
            require_private_volume(libc::MNT_LOCAL as u32).unwrap();
            assert!(require_private_volume(0).is_err());
            assert!(
                require_private_volume((libc::MNT_LOCAL | libc::MNT_IGNORE_OWNERSHIP) as u32)
                    .is_err()
            );
        }
    }
}

#[cfg(windows)]
mod windows {
    //! Grant access only to this account, even in a shared temp directory.

    use std::ffi::c_void;
    use std::fs::File;
    use std::io;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::ptr::null_mut;

    use tempfile::{NamedTempFile, TempPath};
    use windows_sys::Win32::Foundation::{
        GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree,
    };
    use windows_sys::Win32::Security::Authorization::{
        ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
        SDDL_REVISION_1,
    };
    use windows_sys::Win32::Security::{
        GetTokenInformation, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CREATE_NEW, CreateFileW, FILE_ATTRIBUTE_TEMPORARY, FILE_SHARE_DELETE, FILE_SHARE_READ,
        GetVolumeInformationByHandleW,
    };
    use windows_sys::Win32::System::SystemServices::FILE_PERSISTENT_ACLS;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    /// Own allocations returned by the security descriptor/SID conversion APIs.
    struct LocalAllocation(*mut c_void);

    impl Drop for LocalAllocation {
        fn drop(&mut self) {
            // SAFETY: these APIs allocate with LocalAlloc; each allocation has one
            // owner and remains alive through every call that borrows its pointer.
            unsafe { LocalFree(self.0) };
        }
    }

    fn private_descriptor() -> io::Result<LocalAllocation> {
        let mut token = null_mut();
        // SAFETY: valid pseudo process handle and writable result pointer.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful OpenProcessToken transfers ownership of a real handle.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };
        let mut bytes = 0;
        // SAFETY: null/zero buffer queries the size; no data is dereferenced.
        unsafe { GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes) };
        if bytes < size_of::<TOKEN_USER>() as u32 {
            return Err(io::Error::last_os_error());
        }
        // usize alignment accommodates TOKEN_USER and its embedded SID storage.
        let mut storage = vec![0_usize; (bytes as usize).div_ceil(size_of::<usize>())];
        // SAFETY: the allocation is aligned and at least the API's requested size.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                storage.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful GetTokenInformation initialized TOKEN_USER in storage.
        let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
        let mut sid_string = null_mut();
        // SAFETY: the SID is borrowed from the still-live token information buffer.
        if unsafe { ConvertSidToStringSidW(user.User.Sid, &mut sid_string) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let _sid_allocation = LocalAllocation(sid_string.cast());
        // SAFETY: successful conversion produces a nul-terminated wide string.
        let sid = unsafe {
            let mut length = 0;
            while *sid_string.add(length) != 0 {
                length += 1;
            }
            String::from_utf16(std::slice::from_raw_parts(sid_string, length))
        }
        .map_err(io::Error::other)?;
        // Use this account as owner. P blocks inherited permissions;
        // the single access entry grants this account full control.
        let descriptor: Vec<u16> = format!("O:{sid}D:P(A;;FA;;;{sid})")
            .encode_utf16()
            .chain([0])
            .collect();
        let mut security_descriptor = null_mut();
        // SAFETY: nul-terminated input and writable result pointer; retained below.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                descriptor.as_ptr(),
                SDDL_REVISION_1,
                &mut security_descriptor,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(LocalAllocation(security_descriptor))
    }

    pub(super) fn create() -> io::Result<NamedTempFile> {
        let descriptor = private_descriptor()?;
        let attributes = SECURITY_ATTRIBUTES {
            nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: descriptor.0,
            bInheritHandle: 0,
        };
        let directory = std::env::temp_dir();
        for _ in 0..16 {
            // This is a separate random filename, never the authentication token.
            let mut name = [0_u8; 16];
            getrandom::fill(&mut name).map_err(io::Error::other)?;
            let suffix: String = name.iter().map(|byte| format!("{byte:02x}")).collect();
            let path = directory.join(format!("rustel-studio-remote-{suffix}.token"));
            let wide_path: Vec<u16> = path.as_os_str().encode_wide().chain([0]).collect();
            // SAFETY: all pointers are valid for this call. CREATE_NEW fails if any
            // file/reparse point already occupies the name; ACL is applied atomically.
            let handle = unsafe {
                CreateFileW(
                    wide_path.as_ptr(),
                    GENERIC_READ | GENERIC_WRITE,
                    FILE_SHARE_READ | FILE_SHARE_DELETE,
                    &attributes,
                    CREATE_NEW,
                    FILE_ATTRIBUTE_TEMPORARY,
                    null_mut(),
                )
            };
            if handle == INVALID_HANDLE_VALUE {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(error);
            }
            // SAFETY: successful CreateFileW transfers one owned file handle.
            let file = unsafe { File::from_raw_handle(handle) };
            let temporary = NamedTempFile::from_parts(file, TempPath::try_from_path(path)?);
            let mut flags = 0;
            // SAFETY: query the created file's actual volume, avoiding path races.
            if unsafe {
                GetVolumeInformationByHandleW(
                    handle,
                    null_mut(),
                    0,
                    null_mut(),
                    null_mut(),
                    &mut flags,
                    null_mut(),
                    0,
                )
            } == 0
            {
                return Err(io::Error::last_os_error());
            }
            // Some filesystems ignore ACLs. Reject them before writing the token.
            if flags & FILE_PERSISTENT_ACLS == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "remote-control credentials require a temporary directory on an ACL-capable filesystem",
                ));
            }
            return Ok(temporary);
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique remote-control credential file",
        ))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;
        use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
        use windows_sys::Win32::Security::{
            ACCESS_ALLOWED_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
            GetSecurityDescriptorControl, OWNER_SECURITY_INFORMATION, SE_DACL_PRESENT,
            SE_DACL_PROTECTED,
        };
        use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
        use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;

        #[test]
        fn credential_file_has_a_protected_current_user_only_dacl_and_is_removed() {
            let mut file = create().unwrap();
            let path = file.path().to_owned();
            let mut owner = null_mut();
            let mut dacl = null_mut();
            let mut descriptor = null_mut();
            // SAFETY: the file handle is live and every output pointer is writable.
            let status = unsafe {
                GetSecurityInfo(
                    file.as_file().as_raw_handle(),
                    SE_FILE_OBJECT,
                    OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                    &mut owner,
                    null_mut(),
                    &mut dacl,
                    null_mut(),
                    &mut descriptor,
                )
            };
            assert_eq!(status, 0, "GetSecurityInfo failed with {status}");
            let descriptor = LocalAllocation(descriptor);
            assert!(!owner.is_null());
            assert!(!dacl.is_null(), "a null DACL permits access to everyone");
            let mut control = 0;
            let mut revision = 0;
            // SAFETY: descriptor owns the API-produced security descriptor.
            assert_ne!(
                unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) },
                0
            );
            assert_ne!(control & SE_DACL_PRESENT, 0);
            assert_ne!(control & SE_DACL_PROTECTED, 0);
            // SAFETY: dacl points into the still-live descriptor returned above.
            assert_eq!(unsafe { (*dacl).AceCount }, 1);
            let mut ace = null_mut();
            // SAFETY: the DACL contains one ACE and the output pointer is writable.
            assert_ne!(unsafe { GetAce(dacl, 0, &mut ace) }, 0);
            // SAFETY: the returned ACE starts with ACE_HEADER; our descriptor grants
            // access with an ACCESS_ALLOWED_ACE, checked before inspecting its body.
            let header = unsafe { &*ace.cast::<windows_sys::Win32::Security::ACE_HEADER>() };
            assert_eq!(u32::from(header.AceType), ACCESS_ALLOWED_ACE_TYPE);
            assert_eq!(header.AceFlags, 0, "the sole ACE must not be inherited");
            assert!(usize::from(header.AceSize) >= size_of::<ACCESS_ALLOWED_ACE>());
            // SAFETY: type/size checked above and descriptor remains live.
            let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
            assert_eq!(ace.Mask, FILE_ALL_ACCESS);
            let allowed_sid = std::ptr::addr_of!(ace.SidStart).cast_mut().cast();
            // SAFETY: both pointers refer to initialized SIDs in the descriptor.
            assert_ne!(unsafe { EqualSid(owner, allowed_sid) }, 0);

            let mut token = null_mut();
            // SAFETY: valid pseudo process handle and writable result pointer.
            assert_ne!(
                unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) },
                0
            );
            // SAFETY: successful OpenProcessToken transfers an owned handle.
            let token = unsafe { OwnedHandle::from_raw_handle(token) };
            let mut bytes = 0;
            // SAFETY: null/zero buffer queries required size.
            unsafe {
                GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut bytes)
            };
            assert!(bytes >= size_of::<TOKEN_USER>() as u32);
            let mut storage = vec![0_usize; (bytes as usize).div_ceil(size_of::<usize>())];
            // SAFETY: aligned storage is large enough for the queried token data.
            assert_ne!(
                unsafe {
                    GetTokenInformation(
                        token.as_raw_handle(),
                        TokenUser,
                        storage.as_mut_ptr().cast(),
                        bytes,
                        &mut bytes,
                    )
                },
                0
            );
            // SAFETY: successful query initialized TOKEN_USER and its SID storage.
            let user = unsafe { &*storage.as_ptr().cast::<TOKEN_USER>() };
            // SAFETY: both SIDs are initialized and their allocations remain live.
            assert_ne!(unsafe { EqualSid(owner, user.User.Sid) }, 0);

            file.write_all(b"test credential\n").unwrap();
            file.flush().unwrap();
            assert_eq!(std::fs::read_to_string(&path).unwrap(), "test credential\n");
            drop(file);
            assert!(
                !path.exists(),
                "dropping the credential must remove its file"
            );
        }
    }
}
