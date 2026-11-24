use std::{
    fs::{File, OpenOptions},
    io,
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::Path,
    ptr::null,
    slice,
};

use windows_sys::Win32::{
    Storage::FileSystem::FILE_SHARE_READ,
    System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, MEMORY_MAPPED_VIEW_ADDRESS, MapViewOfFile,
        PAGE_READONLY, UnmapViewOfFile,
    },
};

pub(super) struct MappedImage {
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    len: usize,
    // Deny writes and replacement of the file until the view has been unmapped.
    _file: File,
}

impl MappedImage {
    pub(super) fn open(path: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(path)?;
        let len = usize::try_from(file.metadata()?.len())
            .ok()
            .filter(|&len| (1..=isize::MAX as usize).contains(&len))
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid developer image size")
            })?;
        // SAFETY: The file handle is live, and the optional security attributes and name are null.
        let mapping = unsafe {
            CreateFileMappingW(file.as_raw_handle(), null(), PAGE_READONLY, 0, 0, null())
        };
        if mapping.is_null() {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: The successful call returned a new kernel handle.
        let mapping = unsafe { OwnedHandle::from_raw_handle(mapping) };
        // SAFETY: The handle describes the full file, and len is its locked length.
        let view = unsafe { MapViewOfFile(mapping.as_raw_handle(), FILE_MAP_READ, 0, 0, len) };
        if view.Value.is_null() {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            view,
            len,
            _file: file,
        })
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: The view covers len initialized file bytes. The live file denies modifications,
        // and the slice borrows this view for at most its mapped lifetime.
        unsafe { slice::from_raw_parts(self.view.Value.cast(), self.len) }
    }
}

impl Drop for MappedImage {
    fn drop(&mut self) {
        // SAFETY: The view belongs to this instance and is unmapped exactly once.
        unsafe { UnmapViewOfFile(self.view) };
    }
}
