use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use std::ptr::NonNull;

use windows_sys::Win32::Foundation::LocalFree;

pub(crate) fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a NUL character",
        ));
    }
    wide.push(0);
    Ok(wide)
}

pub(crate) fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

pub(crate) struct LocalAllocation<T>(NonNull<T>);

impl<T> LocalAllocation<T> {
    /// The pointer must be null or an owned allocation released by LocalFree.
    pub(crate) unsafe fn from_raw(ptr: *mut T) -> Option<Self> {
        NonNull::new(ptr).map(Self)
    }

    pub(crate) fn as_ptr(&self) -> *mut T {
        self.0.as_ptr()
    }
}

impl<T> Drop for LocalAllocation<T> {
    fn drop(&mut self) {
        // SAFETY: from_raw takes exclusive ownership of a LocalFree allocation.
        unsafe { LocalFree(self.as_ptr().cast()) };
    }
}
