//! Atomic replacement of a file within its existing parent directory.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::ptr::null;
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_DELETE, FILE_SHARE_READ, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    MoveFileExW, ReplaceFileW,
};
use zeroize::Zeroizing;

use crate::win32::{parent_dir, wide_path};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Writes and flushes a sibling temporary file, then replaces the destination.
pub fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    install(path, data, |source, destination| {
        let source = wide_path(source)?;
        let destination = wide_path(destination)?;
        // SAFETY: Both buffers are live, NUL-terminated paths on the same volume.
        let moved = unsafe {
            MoveFileExW(
                source.as_ptr(),
                destination.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        };
        if moved == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    })
}

/// Replaces an existing file after comparing its contents with the prepared snapshot.
/// The comparison happens after the replacement has been flushed to disk.
pub fn replace_existing_if_unchanged(path: &Path, expected: &[u8], data: &[u8]) -> io::Result<()> {
    install(path, data, |source, destination| {
        // Deny in-place writes while checking the file, but allow a concurrent
        // forget operation to remove it. ReplaceFileW requires a live target.
        let existing = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .open(destination)?;
        let limit = expected.len() + 1;
        let mut bytes = Zeroizing::new(Vec::with_capacity(limit));
        (&existing).take(limit as u64).read_to_end(&mut bytes)?;
        if bytes.as_slice() != expected {
            return Err(io::Error::other("existing file changed"));
        }

        replace_existing(source, destination)
    })
}

fn replace_existing(source: &Path, destination: &Path) -> io::Result<()> {
    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    // SAFETY: Both paths are live, NUL-terminated paths in the same directory.
    // No backup path or reserved parameters are supplied.
    let replaced = unsafe {
        ReplaceFileW(
            destination.as_ptr(),
            source.as_ptr(),
            null(),
            0,
            null(),
            null(),
        )
    };
    if replaced == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn install(
    path: &Path,
    data: &[u8],
    replace: impl FnOnce(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
    // On Windows, canonicalize returns an extended-length path for the Win32 call.
    let parent = fs::canonicalize(parent_dir(path))?;
    let (mut temporary, mut file) = TemporaryFile::create(&parent, name)?;

    let result = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    result?;

    replace(&temporary.path, &parent.join(name))?;
    temporary.delete_on_drop = false;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TestDir;

    #[test]
    fn replacing_a_deleted_destination_never_recreates_the_name() {
        let directory = TestDir::new();
        let destination = directory.path().join("current.bin");
        fs::write(&destination, b"stored record").unwrap();
        let verified = OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
            .open(&destination)
            .unwrap();

        let result = install(&destination, b"migrated record", |source, target| {
            fs::remove_file(target).unwrap();
            replace_existing(source, target)
        });

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        drop(verified);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 0);
    }
}

struct TemporaryFile {
    path: PathBuf,
    delete_on_drop: bool,
}

impl TemporaryFile {
    fn create(parent: &Path, destination: &OsStr) -> io::Result<(Self, File)> {
        loop {
            let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let name = format!(".copiago-{}-{id}.tmp", std::process::id());
            if destination.eq_ignore_ascii_case(OsStr::new(&name)) {
                continue;
            }
            let path = parent.join(name);
            match File::create_new(&path) {
                Ok(file) => {
                    return Ok((
                        Self {
                            path,
                            delete_on_drop: true,
                        },
                        file,
                    ));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if self.delete_on_drop {
            let _ = fs::remove_file(&self.path);
        }
    }
}
