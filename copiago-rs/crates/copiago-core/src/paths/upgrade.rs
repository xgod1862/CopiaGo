use std::ffi::OsStr;
use std::fs::{self, File};
use std::io::{self, ErrorKind};
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;

use super::{create_private_dir, ensure_data_dir};
use crate::win32::parent_dir;

static NEXT_MIGRATION: AtomicUsize = AtomicUsize::new(0);

pub(super) fn prepare(target: PathBuf, legacy: PathBuf) -> io::Result<PathBuf> {
    let use_target = if target != legacy && legacy.is_dir() {
        prepare_with_legacy(&target, &legacy)
    } else {
        ensure_data_dir(&target)
    };
    if use_target.is_ok() {
        return Ok(target);
    }
    ensure_data_dir(&legacy)?;
    Ok(legacy)
}

fn prepare_with_legacy(target: &Path, legacy: &Path) -> io::Result<()> {
    match fs::read_dir(target) {
        Ok(mut entries) => {
            if entries.next().transpose()?.is_some() {
                return ensure_data_dir(target);
            }
            reject_reparse_point(&fs::symlink_metadata(target)?)?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    reject_reparse_point(&fs::symlink_metadata(legacy)?)?;
    let parent = parent_dir(target);
    fs::create_dir_all(parent)?;
    let staging = create_staging(parent)?;
    let published = copy_and_publish(legacy, &staging, target);
    if published.is_err() {
        let _ = fs::remove_dir_all(&staging);
    }
    published?;

    // Keep an inactive backup of the source after publishing the new directory.
    let backup = legacy.with_extension("before-rust-upgrade");
    if !backup.exists() {
        let _ = fs::rename(legacy, backup);
    }
    Ok(())
}

fn create_staging(parent: &Path) -> io::Result<PathBuf> {
    loop {
        let id = NEXT_MIGRATION.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".copiago-migration-{}-{id}", std::process::id()));
        match create_private_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
}

fn copy_and_publish(legacy: &Path, staging: &Path, target: &Path) -> io::Result<()> {
    copy_contents(legacy, staging, true)?;
    // Removing only an empty target protects data created since the earlier check.
    match fs::remove_dir(target) {
        Ok(()) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::rename(staging, target)
}

fn copy_contents(legacy: &Path, target: &Path, top: bool) -> io::Result<()> {
    for entry in fs::read_dir(legacy)? {
        let entry = entry?;
        let name = entry.file_name();
        if top && (name == "received" || Path::new(&name).extension() == Some(OsStr::new("tmp"))) {
            continue;
        }
        let path = entry.path();
        let metadata = entry.metadata()?;
        reject_reparse_point(&metadata)?;
        let output = target.join(name);
        if metadata.is_dir() {
            fs::create_dir(&output)?;
            copy_contents(&path, &output, false)?;
        } else if metadata.is_file() {
            let mut source = File::open(&path)?;
            let mut destination = File::create_new(output)?;
            io::copy(&mut source, &mut destination)?;
            destination.sync_all()?;
        }
    }
    Ok(())
}

fn reject_reparse_point(metadata: &fs::Metadata) -> io::Result<()> {
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            ErrorKind::Unsupported,
            "cannot migrate redirected data entries",
        ));
    }
    Ok(())
}
