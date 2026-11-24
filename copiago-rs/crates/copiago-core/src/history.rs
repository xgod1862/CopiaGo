//! The current user's Python-compatible clipboard history.

use std::ffi::CStr;
use std::fs;
use std::io::{self, ErrorKind, Read};
use std::mem::MaybeUninit;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use windows_sys::Win32::{Foundation::SYSTEMTIME, System::SystemInformation::GetLocalTime};
use zeroize::Zeroizing;

use crate::clip::{Clip, FileOffer};
use crate::dpapi::{protect_history, unprotect_history};
use crate::files::ReceivedFile;
use crate::settings::SyncKind;
use crate::win32::parent_dir;

use self::sqlite::{Connection, Param};

mod sqlite;
#[cfg(test)]
mod tests;

const FILE_NAME: &str = "history.sqlite3";
const MAX_ITEMS: usize = 50;
const MAX_BYTES: i64 = 128 * 1024 * 1024;
const MAX_ITEM_BYTES: usize = 32 * 1024 * 1024;
const TEXT_PREVIEW_CHARS: usize = 96;
// Count characters only for texts whose encrypted cell is at most this large.
const COUNT_TEXT_BYTES: i64 = 1024 * 1024;
const THUMBNAIL_WIDTH: u32 = 88;
const THUMBNAIL_HEIGHT: u32 = 56;

/// The list in which a clip was recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Phone,
    Pc,
}

impl Source {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Phone => "phone",
            Self::Pc => "pc",
        }
    }
}

/// List metadata and the decrypted preview. `created` is local time to the minute.
/// `preview` is UTF-8 text, a PNG thumbnail, or UTF-8 `name\nsize` for a file.
#[derive(Default)]
pub struct Entry {
    pub id: i64,
    pub created: String,
    pub kind: String,
    pub preview: Vec<u8>,
    /// Payload size in bytes, estimated from the encrypted cell to within a cipher block.
    pub size: u64,
    /// Character count of a text item, when it is small enough to read while listing.
    pub chars: Option<usize>,
}

/// The full stored payload. The database schema does not keep pasteboard UTIs or offer metadata.
#[derive(PartialEq, Eq)]
pub enum StoredClip {
    Text(String),
    Image(Vec<u8>),
    File { name: String, bytes: Vec<u8> },
}

/// Opens a short-lived connection for each operation so both app versions can share the database.
pub struct History {
    path: PathBuf,
}

impl History {
    /// Selects `history.sqlite3` in the resolved CopiaGo data directory.
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(FILE_NAME),
        }
    }

    /// Records a clip for the selected source. The caller checks the corresponding save setting.
    /// Returns true for an inserted clip or a duplicate of that source's latest clip.
    /// Returns false for an empty clip, a file pending receipt, or a payload over 32 MiB.
    /// For images, `thumbnail` returns a PNG scaled to fit the supplied width and height.
    pub fn add(
        &self,
        source: Source,
        clip: &Clip,
        thumbnail: impl FnOnce(&[u8], u32, u32) -> io::Result<Vec<u8>>,
    ) -> io::Result<bool> {
        let (kind, raw) = match clip {
            Clip::Text(text) => (SyncKind::Text, text.as_bytes()),
            Clip::Image { bytes, .. } => (SyncKind::Image, bytes.as_slice()),
            Clip::File(offer) => match offer.data.as_deref() {
                Some(bytes) => (SyncKind::File, bytes),
                None => return Ok(false),
            },
        };
        if raw.len() > MAX_ITEM_BYTES || (raw.is_empty() && kind != SyncKind::File) {
            return Ok(false);
        }

        let preview = Zeroizing::new(match clip {
            Clip::Text(text) => text_preview(text),
            Clip::Image { .. } => thumbnail(raw, THUMBNAIL_WIDTH, THUMBNAIL_HEIGHT)?,
            Clip::File(offer) => format!("{}\n{}", offer.name, raw.len()).into_bytes(),
        });
        let db = self.connect(true)?;
        db.transaction(|db| {
            if is_duplicate(db, source, kind, raw)? {
                return Ok(true);
            }
            let protected = protect_history(raw)?;
            let protected_preview = protect_history(&preview)?;
            let created = local_minute();
            db.execute(
                c"INSERT INTO items (created, kind, preview, payload, source)
                  VALUES (?, ?, ?, ?, ?)",
                &[
                    Param::Text(&created),
                    Param::Text(kind.as_str()),
                    Param::Blob(&protected_preview),
                    Param::Blob(&protected),
                    Param::Text(source.as_str()),
                ],
            )?;
            prune(db, source)?;
            Ok(true)
        })
    }

    /// Record a published file, checking the per-item limit before reading it from disk.
    pub fn add_received(&self, name: String, received: &ReceivedFile) -> io::Result<bool> {
        if received.bytes() > MAX_ITEM_BYTES as u64 {
            return Ok(false);
        }
        let mut bytes = Vec::with_capacity(received.bytes() as usize);
        fs::File::open(received.path())?
            .take(received.bytes() + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != received.bytes() {
            return Err(ErrorKind::InvalidData.into());
        }
        self.add(
            Source::Phone,
            &Clip::File(FileOffer {
                name,
                uti: String::new(),
                size: received.bytes(),
                change: None,
                data: Some(bytes),
            }),
            |_, _, _| unreachable!("history requests thumbnails only for images"),
        )
    }

    /// Lists the selected source newest first, skipping previews DPAPI cannot decrypt.
    pub fn entries(&self, source: Source) -> io::Result<Vec<Entry>> {
        let Some(db) = self.existing()? else {
            return Ok(Vec::new());
        };
        let mut statement = db.prepare(
            c"SELECT id, created, kind, preview, length(payload),
                     CASE WHEN kind = 'text' AND length(payload) <= ? THEN payload END
              FROM items WHERE source = ? ORDER BY id DESC",
            &[
                Param::Integer(COUNT_TEXT_BYTES),
                Param::Text(source.as_str()),
            ],
        )?;
        let overhead = dpapi_overhead();
        let mut entries = Vec::new();
        while let Some(mut row) = statement.next()? {
            let Ok(preview) = unprotect_history(row.blob(3)?) else {
                continue;
            };
            let stored = u64::try_from(row.integer(4)?).unwrap_or_default();
            let text = row.blob(5)?;
            let chars = (!text.is_empty())
                .then(|| unprotect_history(text).ok())
                .flatten()
                .map(|raw| {
                    String::from_utf8_lossy(&Zeroizing::new(raw))
                        .chars()
                        .count()
                });
            entries.push(Entry {
                id: row.integer(0)?,
                created: row.text(1)?.to_owned(),
                kind: row.text(2)?.to_owned(),
                preview,
                size: stored.saturating_sub(overhead),
                chars,
            });
        }
        Ok(entries)
    }

    /// Decrypts one stored clip by its database row id.
    pub fn get(&self, item_id: i64) -> io::Result<Option<StoredClip>> {
        let Some(db) = self.existing()? else {
            return Ok(None);
        };
        let mut statement = db.prepare(
            c"SELECT kind, payload, preview FROM items WHERE id = ?",
            &[Param::Integer(item_id)],
        )?;
        let Some(mut row) = statement.next()? else {
            return Ok(None);
        };
        let raw = unprotect_history(row.blob(1)?)?;
        let clip = match row.text(0)? {
            "image" => StoredClip::Image(raw),
            "file" => {
                let preview = Zeroizing::new(unprotect_history(row.blob(2)?)?);
                let name = std::str::from_utf8(&preview)
                    .map_err(|_| invalid_utf8())?
                    .split('\n')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                StoredClip::File { name, bytes: raw }
            }
            _ => StoredClip::Text(String::from_utf8(raw).map_err(|_| invalid_utf8())?),
        };
        Ok(Some(clip))
    }

    pub fn remove(&self, item_id: i64) -> io::Result<()> {
        self.delete(
            c"DELETE FROM items WHERE id = ?",
            &[Param::Integer(item_id)],
        )
    }

    /// Clears the selected source, or both sources when no source is specified.
    pub fn clear(&self, source: Option<Source>) -> io::Result<()> {
        match source {
            Some(source) => self.delete(
                c"DELETE FROM items WHERE source = ?",
                &[Param::Text(source.as_str())],
            ),
            None => self.delete(c"DELETE FROM items", &[]),
        }
    }

    fn delete(&self, sql: &'static CStr, params: &[Param<'_>]) -> io::Result<()> {
        if let Some(db) = self.existing()? {
            db.execute(sql, params)?;
        }
        Ok(())
    }

    fn existing(&self) -> io::Result<Option<Connection>> {
        match fs::metadata(&self.path) {
            Ok(metadata) if metadata.is_file() => self.connect(false).map(Some),
            Ok(_) => Ok(None),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn connect(&self, create: bool) -> io::Result<Connection> {
        if create {
            crate::paths::ensure_data_dir(parent_dir(&self.path))?;
        }
        let db = Connection::open(&self.path, create)?;
        db.execute(c"PRAGMA secure_delete = ON", &[])?;
        db.execute(
            c"CREATE TABLE IF NOT EXISTS items (
                id INTEGER PRIMARY KEY, created TEXT NOT NULL, kind TEXT NOT NULL,
                preview BLOB NOT NULL, payload BLOB NOT NULL,
                source TEXT NOT NULL DEFAULT 'phone'
            )",
            &[],
        )?;
        if !has_source(&db)? {
            db.transaction(|db| {
                // Another connection may have completed the migration while this one waited.
                if !has_source(db)? {
                    db.execute(
                        c"ALTER TABLE items ADD COLUMN source TEXT NOT NULL DEFAULT 'phone'",
                        &[],
                    )?;
                }
                Ok(())
            })?;
        }
        Ok(db)
    }
}

fn has_source(db: &Connection) -> io::Result<bool> {
    let mut columns = db.prepare(c"PRAGMA table_info(items)", &[])?;
    while let Some(mut column) = columns.next()? {
        if column.text(1)? == "source" {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Bytes DPAPI adds to a payload, so the cell length estimates the original size.
/// Cipher padding makes the result accurate to half a block either way.
fn dpapi_overhead() -> u64 {
    static OVERHEAD: OnceLock<u64> = OnceLock::new();
    *OVERHEAD.get_or_init(|| {
        // An empty input still encrypts one full padding block of 16 bytes.
        protect_history(&[]).map_or(0, |blob| (blob.len() as u64).saturating_sub(8))
    })
}

fn text_preview(text: &str) -> Vec<u8> {
    // Python str.split() also treats the four information separators as whitespace.
    let mut words = text
        .split(|ch: char| ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}'))
        .filter(|word| !word.is_empty());
    let first = words.next().unwrap_or_default();
    first
        .chars()
        .chain(words.flat_map(|word| std::iter::once(' ').chain(word.chars())))
        .take(TEXT_PREVIEW_CHARS)
        .collect::<String>()
        .into_bytes()
}

fn local_minute() -> String {
    // winsqlite3.dll can omit the SQLite localtime modifier.
    let mut time = MaybeUninit::<SYSTEMTIME>::uninit();
    // SAFETY: GetLocalTime initializes the SYSTEMTIME at this writable pointer.
    let time = unsafe {
        GetLocalTime(time.as_mut_ptr());
        time.assume_init()
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute
    )
}

fn is_duplicate(db: &Connection, source: Source, kind: SyncKind, raw: &[u8]) -> io::Result<bool> {
    let mut statement = db.prepare(
        c"SELECT kind, payload FROM items WHERE source = ? ORDER BY id DESC LIMIT 1",
        &[Param::Text(source.as_str())],
    )?;
    let Some(mut previous) = statement.next()? else {
        return Ok(false);
    };
    if previous.text(0)? != kind.as_str() {
        return Ok(false);
    }
    // Older entries from an unavailable Windows user should not prevent any further recording.
    Ok(unprotect_history(previous.blob(1)?)
        .is_ok_and(|previous| Zeroizing::new(previous).as_slice() == raw))
}

fn prune(db: &Connection, source: Source) -> io::Result<()> {
    let mut rows = db.prepare(
        c"SELECT id, length(preview) + length(payload) FROM items WHERE source = ? ORDER BY id DESC",
        &[Param::Text(source.as_str())],
    )?;
    let mut total = 0;
    for index in 0..=MAX_ITEMS {
        let Some(mut row) = rows.next()? else {
            break;
        };
        total += row.integer(1)?;
        if index == MAX_ITEMS || total > MAX_BYTES {
            let cutoff = row.integer(0)?;
            drop(rows);
            return db.execute(
                c"DELETE FROM items WHERE source = ? AND id <= ?",
                &[Param::Text(source.as_str()), Param::Integer(cutoff)],
            );
        }
    }
    Ok(())
}

fn invalid_utf8() -> io::Error {
    io::Error::new(ErrorKind::InvalidData, "history contains invalid UTF-8")
}
