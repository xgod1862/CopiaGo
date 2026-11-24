//! Naming, storage and retention for received iPhone files.

use std::{
    fs::{self, File, Metadata},
    io::{self, ErrorKind},
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{self, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime},
};

use windows_sys::Win32::{
    Foundation::SYSTEMTIME,
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_WRITE_ATTRIBUTES,
    },
    System::SystemInformation::GetLocalTime,
};

use crate::{
    i18n::Language,
    paths,
    settings::{Settings, SyncKind},
    tr,
};

const MIB: u64 = 1024 * 1024;
pub const MAX_FILE_BYTES: u64 = 512 * MIB;
const KEEP_FOR: Duration = Duration::from_secs(24 * 3600);
const KEEP_BYTES: u64 = 2 * 1024 * MIB;
const RECEIVED: &str = "received";
const STAGING_PREFIX: &str = ".incoming-";
static NEXT_TRANSFER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OfferDecision {
    Ignore,
    TooLarge,
    Receive,
    Confirm(Confirmation),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Confirmation {
    AutoSyncOff,
    UnknownSize,
    OverThreshold,
}

pub fn decide_offer(settings: &Settings, size: u64) -> OfferDecision {
    if !settings.pull_types.contains(&SyncKind::File) {
        OfferDecision::Ignore
    } else if size > MAX_FILE_BYTES {
        OfferDecision::TooLarge
    } else if !settings.pull {
        OfferDecision::Confirm(Confirmation::AutoSyncOff)
    } else if settings.file_ask_mb == 0 {
        OfferDecision::Receive
    } else if size == 0 {
        OfferDecision::Confirm(Confirmation::UnknownSize)
    } else if size > u64::from(settings.file_ask_mb) * MIB {
        OfferDecision::Confirm(Confirmation::OverThreshold)
    } else {
        OfferDecision::Receive
    }
}

/// Preserves the original suffix, substituting the file type's extension when missing.
pub fn safe_name(name: &str, fallback_extension: &str) -> Option<String> {
    let cleaned = name.replace(
        |character: char| character <= '\u{1f}' || "<>:\"/\\|?*".contains(character),
        "_",
    );
    let cleaned = cleaned.trim_matches([' ', '.']);
    let (stem, extension) = cleaned
        .rsplit_once('.')
        .unwrap_or((cleaned, fallback_extension));
    if stem.is_empty() {
        return None;
    }
    // The component before the first dot is also checked for DOS device names.
    let device = stem
        .split('.')
        .next()?
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let reserved = matches!(
        device.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || device
        .strip_prefix("COM")
        .or_else(|| device.strip_prefix("LPT"))
        .is_some_and(|number| {
            matches!(
                number,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    let stem: String = reserved
        .then_some('_')
        .into_iter()
        .chain(stem.chars())
        .take(120)
        .collect();
    Some(format!("{stem}.{extension}"))
}

pub fn fallback_name(language: Language, extension: &str) -> String {
    tr!(
        language,
        "iPhone 文件 {timestamp}.{extension}",
        timestamp = local_timestamp(),
        extension = extension
    )
    .into_owned()
}

fn local_timestamp() -> String {
    let mut local = std::mem::MaybeUninit::<SYSTEMTIME>::uninit();
    // SAFETY: GetLocalTime initializes the SYSTEMTIME at this writable pointer.
    let local = unsafe {
        GetLocalTime(local.as_mut_ptr());
        local.assume_init()
    };
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond,
    )
}

/// The file handle is closed before its private staging directory is removed on drop.
pub struct StagedFile {
    file: File,
    directory: TransferDirectory,
    final_directory: PathBuf,
    name: String,
}

impl StagedFile {
    pub fn new(data_dir: &Path, name: String) -> io::Result<Self> {
        if safe_name(&name, "bin").as_deref() != Some(name.as_str())
            || name.encode_utf16().count() > 255
        {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "invalid received file name",
            ));
        }
        let root = path::absolute(data_dir)?.join(RECEIVED);
        paths::ensure_data_dir(&root)?;
        let root = plain_directory(&root)?;

        let timestamp = local_timestamp();
        let (directory, final_directory) = loop {
            let id = NEXT_TRANSFER.fetch_add(1, Ordering::Relaxed);
            let title = format!("{timestamp}-{}-{id}", std::process::id());
            let final_directory = root.join(&title);
            let staging = root.join(format!("{STAGING_PREFIX}{title}"));
            match paths::create_private_dir(&staging) {
                Ok(()) => {
                    break (
                        TransferDirectory {
                            path: staging,
                            keep: false,
                        },
                        final_directory,
                    );
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        let file = File::create_new(directory.path.join(&name))?;
        Ok(Self {
            file,
            directory,
            final_directory,
            name,
        })
    }

    pub fn writer(&mut self) -> &mut File {
        &mut self.file
    }

    /// Validates the staged length and synchronizes the contents before the rename.
    pub fn sync(self, bytes: u64) -> io::Result<SyncedFile> {
        if bytes > MAX_FILE_BYTES || self.file.metadata()?.len() != bytes {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "invalid received file length",
            ));
        }
        self.file.sync_all()?;
        let Self {
            file,
            directory,
            final_directory,
            name,
        } = self;
        drop(file);
        Ok(SyncedFile {
            directory,
            final_directory,
            name,
            bytes,
        })
    }
}

pub struct SyncedFile {
    directory: TransferDirectory,
    final_directory: PathBuf,
    name: String,
    bytes: u64,
}

impl SyncedFile {
    /// Makes the complete transfer visible under its timestamped name in one rename.
    pub fn publish(mut self) -> io::Result<ReceivedFile> {
        let now = SystemTime::now();
        let root = self
            .final_directory
            .parent()
            .expect("transfer has a parent");
        cleanup_at(root, now, KEEP_BYTES)?;
        set_modified(&self.directory.path, now)?;
        fs::rename(&self.directory.path, &self.final_directory)?;
        self.directory.path = self.final_directory;
        Ok(ReceivedFile {
            path: self.directory.path.join(self.name),
            directory: self.directory,
            bytes: self.bytes,
        })
    }
}

/// A completed transfer. Retain it after handing the path to the clipboard or another consumer.
pub struct ReceivedFile {
    path: PathBuf,
    directory: TransferDirectory,
    bytes: u64,
}

impl ReceivedFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    pub fn keep(mut self) {
        self.directory.keep = true;
    }
}

struct TransferDirectory {
    path: PathBuf,
    keep: bool,
}

impl Drop for TransferDirectory {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Applies Python's retention policy to an existing received directory without creating it.
pub fn cleanup(data_dir: &Path) -> io::Result<()> {
    cleanup_at(&data_dir.join(RECEIVED), SystemTime::now(), KEEP_BYTES)
}

fn cleanup_at(root: &Path, now: SystemTime, limit: u64) -> io::Result<()> {
    let root = match plain_directory(root) {
        Ok(root) => root,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut transfers = Vec::new();
    let mut total = 0u64;
    for entry in fs::read_dir(root)?.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() || is_reparse(&metadata) {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        if now.duration_since(modified).is_ok_and(|age| age > KEEP_FOR) {
            let _ = fs::remove_dir_all(&path);
        } else if !entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(STAGING_PREFIX.as_bytes())
            && let Ok(size) = directory_size(&path)
        {
            total = total.saturating_add(size);
            transfers.push((modified, path, size));
        }
    }
    transfers.sort_unstable();
    for (_, path, size) in transfers {
        if total <= limit {
            break;
        }
        if fs::remove_dir_all(&path).is_ok() {
            total -= size;
        }
    }
    Ok(())
}

fn plain_directory(path: &Path) -> io::Result<&Path> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || is_reparse(&metadata) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "invalid received directory",
        ));
    }
    Ok(path)
}

fn is_reparse(metadata: &Metadata) -> bool {
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

fn directory_size(path: &Path) -> io::Result<u64> {
    let mut total = 0u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if is_reparse(&metadata) {
            continue;
        }
        let size = if metadata.is_dir() {
            directory_size(&entry.path())?
        } else {
            metadata.len()
        };
        total = total.saturating_add(size);
    }
    Ok(total)
}

fn set_modified(path: &Path, time: SystemTime) -> io::Result<()> {
    File::options()
        .access_mode(FILE_WRITE_ATTRIBUTES)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)?
        .set_modified(time)
}

#[cfg(test)]
mod tests;
