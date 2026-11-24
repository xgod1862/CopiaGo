//! Atomic replacement of a file within its existing parent directory.

use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use windows_sys::Win32::Storage::FileSystem::{
    MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
};

use crate::win32::{parent_dir, wide_path};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// Writes and flushes a sibling temporary file, then replaces the destination.
pub fn write(path: &Path, data: &[u8]) -> io::Result<()> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?;
    // On Windows, canonicalize returns an extended-length path for the Win32 call.
    let parent = fs::canonicalize(parent_dir(path))?;
    let destination = wide_path(&parent.join(name))?;
    let (mut temporary, mut file) = TemporaryFile::create(&parent, name)?;

    let result = file.write_all(data).and_then(|()| file.sync_all());
    drop(file);
    result?;

    let source = wide_path(&temporary.path)?;
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
    temporary.delete_on_drop = false;
    Ok(())
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
