use std::ffi::c_void;
use std::fs::{self, File};
use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;
use std::ptr::{addr_of, null_mut};

use windows_sys::Win32::Globalization::lstrlenW;
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{
    GetTokenInformation, SECURITY_ATTRIBUTES, SECURITY_MAX_SID_SIZE, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, FILE_FLAG_BACKUP_SEMANTICS, GetVolumeInformationByHandleW,
};
use windows_sys::Win32::System::SystemServices::FILE_PERSISTENT_ACLS;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::win32::{LocalAllocation, parent_dir, wide_path};

/// Creates the data directory with an inheritable, protected DACL for the current user and SYSTEM.
/// Parent folders may be shared. Existing directory permissions are preserved.
pub fn ensure_data_dir(path: &Path) -> io::Result<()> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => return require_persistent_acls(path),
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "data path is a file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing directory name"))?;
    let parent = parent_dir(path);
    fs::create_dir_all(parent)?;
    let parent = fs::canonicalize(parent)?;
    require_persistent_acls(&parent)?;

    let path = parent.join(name);
    let wide = wide_path(&path)?;
    let descriptor = private_descriptor()?;
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };

    // SAFETY: The NUL-terminated path and security descriptor remain live through the call.
    let created = unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) };
    if created != 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::AlreadyExists && path.is_dir() {
        return require_persistent_acls(&path);
    }
    Err(error)
}

fn require_persistent_acls(path: &Path) -> io::Result<()> {
    let directory = File::options()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?;
    let mut flags = 0;
    // SAFETY: The handle is live. The flags output is writable, and all other outputs are optional.
    let succeeded = unsafe {
        GetVolumeInformationByHandleW(
            directory.as_raw_handle(),
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    if flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the data directory requires a filesystem with Windows ACL support",
        ));
    }
    Ok(())
}

fn private_descriptor() -> io::Result<LocalAllocation<c_void>> {
    let sid = current_user_sid()?;
    // P protects the DACL from parent inheritance. OI and CI let files and folders inherit FA.
    let sddl = if sid == "S-1-5-18" {
        "D:P(A;OICI;FA;;;SY)".to_owned()
    } else {
        format!("D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)")
    };
    let wide: Vec<_> = sddl.encode_utf16().chain([0]).collect();
    let mut descriptor = null_mut();
    // SAFETY: wide is NUL-terminated and the output pointer is writable.
    let succeeded = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: A successful conversion returns an owned LocalFree allocation.
    unsafe { LocalAllocation::from_raw(descriptor) }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing security descriptor"))
}

fn current_user_sid() -> io::Result<String> {
    #[repr(C)]
    struct TokenUserBuffer {
        user: TOKEN_USER,
        sid: [u8; SECURITY_MAX_SID_SIZE as usize],
    }

    let mut token = null_mut();
    // SAFETY: GetCurrentProcess returns a valid pseudo-handle and token is writable.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: OpenProcessToken succeeded and returned an owned handle.
    let token = unsafe { OwnedHandle::from_raw_handle(token) };
    let mut buffer = MaybeUninit::<TokenUserBuffer>::uninit();
    let mut len = 0;
    // SAFETY: The buffer is aligned for TOKEN_USER and has room for the structure and SID.
    let read = unsafe {
        GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size_of::<TokenUserBuffer>() as u32,
            &mut len,
        )
    };
    if read == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut string_sid = null_mut();
    // SAFETY: TokenUser initializes the TOKEN_USER at the front of the live buffer and its SID.
    let converted = unsafe {
        let info = addr_of!((*buffer.as_ptr()).user).read();
        ConvertSidToStringSidW(info.User.Sid, &mut string_sid)
    };
    if converted == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: Successful conversion gives an owned LocalFree allocation.
    let string_sid = unsafe { LocalAllocation::from_raw(string_sid) }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing SID string"))?;
    // SAFETY: The allocated SID string is NUL-terminated and lives through the conversion.
    let sid = unsafe {
        let pointer = string_sid.as_ptr();
        std::slice::from_raw_parts(pointer, lstrlenW(pointer) as usize)
    };
    Ok(String::from_utf16_lossy(sid))
}
