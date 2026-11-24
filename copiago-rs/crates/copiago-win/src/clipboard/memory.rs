use std::io;
use std::ptr::NonNull;
use std::slice;

use windows_sys::Win32::Foundation::{GlobalFree, HGLOBAL};
use windows_sys::Win32::System::Memory::{
    GMEM_MOVEABLE, GMEM_ZEROINIT, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};

use super::retry;

pub(super) struct OwnedMemory {
    handle: NonNull<std::ffi::c_void>,
    len: usize,
}

impl OwnedMemory {
    pub(super) fn new(len: usize) -> io::Result<Self> {
        let handle = retry(|| {
            // SAFETY: The requested length bounds the zero-initialized allocation.
            NonNull::new(unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, len) })
                .ok_or_else(io::Error::last_os_error)
        })?;
        Ok(Self { handle, len })
    }

    pub(super) fn from_bytes(bytes: &[u8]) -> io::Result<Self> {
        let mut memory = Self::new(bytes.len())?;
        memory.with_bytes(|buffer| buffer.copy_from_slice(bytes))?;
        Ok(memory)
    }

    pub(super) fn with_bytes<T>(
        &mut self,
        operation: impl FnOnce(&mut [u8]) -> T,
    ) -> io::Result<T> {
        // SAFETY: The allocation is exclusively owned and remains live throughout
        // the lock. It was zero-initialized at allocation.
        let locked = unsafe { LockedMemory::new(self.handle()) }?;
        if self.len > locked.len {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "short clipboard allocation",
            ));
        }
        // SAFETY: The memory is uniquely borrowed, locked and initialized.
        let data = unsafe { slice::from_raw_parts_mut(locked.pointer.as_ptr(), self.len) };
        Ok(operation(data))
    }

    pub(super) fn handle(&self) -> HGLOBAL {
        self.handle.as_ptr()
    }

    pub(super) fn transfer(self) {
        std::mem::forget(self);
    }
}

impl Drop for OwnedMemory {
    fn drop(&mut self) {
        // SAFETY: Clipboard ownership was not transferred and the memory is unlocked.
        unsafe { GlobalFree(self.handle()) };
    }
}

pub(super) struct LockedMemory {
    handle: HGLOBAL,
    pointer: NonNull<u8>,
    len: usize,
}

impl LockedMemory {
    /// The handle must name a live HGLOBAL allocation that remains owned
    /// by the caller or the open clipboard until this guard is dropped.
    pub(super) unsafe fn new(handle: HGLOBAL) -> io::Result<Self> {
        let len = retry(|| {
            // SAFETY: The caller guarantees the handle is live.
            let size = unsafe { GlobalSize(handle) };
            if size == 0 {
                Err(io::Error::last_os_error())
            } else if size > isize::MAX as usize {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid clipboard buffer size",
                ))
            } else {
                Ok(size)
            }
        })?;
        let pointer = retry(|| {
            // SAFETY: The caller guarantees the handle is live.
            NonNull::new(unsafe { GlobalLock(handle) }.cast()).ok_or_else(io::Error::last_os_error)
        })?;
        Ok(Self {
            handle,
            pointer,
            len,
        })
    }

    pub(super) fn bytes(&self) -> &[u8] {
        // SAFETY: The handle is locked, and the data slice is borrowed from the guard.
        unsafe { slice::from_raw_parts(self.pointer.as_ptr(), self.len) }
    }
}

impl Drop for LockedMemory {
    fn drop(&mut self) {
        // SAFETY: This guard holds a matching GlobalLock on the still-live handle.
        unsafe { GlobalUnlock(self.handle) };
    }
}
