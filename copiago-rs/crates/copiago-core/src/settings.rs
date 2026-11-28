//! Reads and updates the Python-compatible settings.json in the selected data directory.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, ErrorKind};
use std::path::Path;
use std::sync::Mutex;

use serde_json::value::{RawValue, to_raw_value};

use crate::i18n::LanguagePreference;
use crate::{atomic, paths};

#[cfg(test)]
mod tests;

const FILE_NAME: &str = "settings.json";
const DEFAULT_FILE_ASK_MB: u8 = 20;
pub const FILE_ASK_MB_CHOICES: [u8; 4] = [10, 20, 50, 0];
pub const PULL_TYPES: [SyncKind; 3] = [SyncKind::Text, SyncKind::Image, SyncKind::File];
pub const PUSH_TYPES: [SyncKind; 2] = [SyncKind::Text, SyncKind::Image];

// Keep each read-modify-write operation together when callers update from different threads.
static WRITE_LOCK: Mutex<()> = Mutex::new(());

// Preserve the original JSON of unchanged values, including unfamiliar number formats.
type Document = BTreeMap<String, Box<RawValue>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncKind {
    Text,
    Image,
    File,
}

impl SyncKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::File => "file",
        }
    }
}

/// The effective settings after defaults have been applied to missing or invalid values.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Settings {
    pub language: LanguagePreference,
    pub history: bool,
    pub pc_history: bool,
    pub pull: bool,
    pub push: bool,
    pub pull_types: Vec<SyncKind>,
    pub push_types: Vec<SyncKind>,
    /// Also send copies another app marked as private, such as passwords copied from a
    /// password manager. They are never kept in PC history.
    pub push_marked: bool,
    /// Zero disables the size confirmation.
    pub file_ask_mb: u8,
    pub notifications: bool,
    pub update_check: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_document(&Document::new())
    }
}

/// A change to one key. Other settings are taken from the latest on-disk document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SettingsChange {
    Language(LanguagePreference),
    History(bool),
    PcHistory(bool),
    Pull(bool),
    Push(bool),
    PullTypes(Vec<SyncKind>),
    PushTypes(Vec<SyncKind>),
    PushMarked(bool),
    FileAskMb(u8),
    Notifications(bool),
    UpdateCheck(bool),
}

impl Settings {
    /// Returns defaults if the file is missing, unreadable, or not a JSON object.
    pub fn load(data_dir: &Path) -> Self {
        Self::read(data_dir).unwrap_or_default()
    }

    /// Reads an existing JSON object without creating the data directory or writing a file.
    pub fn read(data_dir: &Path) -> io::Result<Self> {
        let document = read_document(&data_dir.join(FILE_NAME))?;
        Ok(Self::from_document(&document))
    }

    /// Updates one key atomically and returns the effective settings after the update.
    /// A missing or malformed document is replaced with a new object.
    pub fn update(data_dir: &Path, change: SettingsChange) -> io::Result<Self> {
        let (key, value) = change.into_entry()?;
        let _guard = WRITE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let path = data_dir.join(FILE_NAME);
        let mut document = match read_document(&path) {
            Ok(document) => document,
            Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidData) => {
                Document::new()
            }
            Err(error) => return Err(error),
        };
        document.insert(key.to_owned(), value);
        let bytes = serde_json::to_vec(&document)?;
        paths::ensure_data_dir(data_dir)?;
        atomic::write(&path, &bytes)?;
        Ok(Self::from_document(&document))
    }

    fn from_document(document: &Document) -> Self {
        let language =
            serde_json::from_str::<String>(raw_setting(document, "language")).unwrap_or_default();
        Self {
            language: LanguagePreference::from_setting(&language),
            history: read_bool(document, "history", false),
            pc_history: read_bool(document, "pc_history", false),
            pull: read_bool(document, "pull", true),
            push: read_bool(document, "push", false),
            pull_types: read_types(raw_setting(document, "pull_types"), &PULL_TYPES),
            push_types: read_types(raw_setting(document, "push_types"), &PUSH_TYPES),
            push_marked: read_bool(document, "push_marked", true),
            file_ask_mb: read_file_ask_mb(raw_setting(document, "file_ask_mb")),
            notifications: read_bool(document, "notifications", true),
            update_check: read_bool(document, "update_check", true),
        }
    }
}

impl SettingsChange {
    fn into_entry(self) -> io::Result<(&'static str, Box<RawValue>)> {
        let (key, value) = match self {
            Self::Language(value) => ("language", to_raw_value(value.as_str())),
            Self::History(value) => ("history", to_raw_value(&value)),
            Self::PcHistory(value) => ("pc_history", to_raw_value(&value)),
            Self::Pull(value) => ("pull", to_raw_value(&value)),
            Self::Push(value) => ("push", to_raw_value(&value)),
            Self::PullTypes(kinds) => (
                "pull_types",
                to_raw_value(&stored_types(&PULL_TYPES, &kinds)),
            ),
            Self::PushTypes(kinds) => (
                "push_types",
                to_raw_value(&stored_types(&PUSH_TYPES, &kinds)),
            ),
            Self::PushMarked(value) => ("push_marked", to_raw_value(&value)),
            Self::FileAskMb(value) if FILE_ASK_MB_CHOICES.contains(&value) => {
                ("file_ask_mb", to_raw_value(&value))
            }
            Self::FileAskMb(_) => {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "unsupported file confirmation threshold",
                ));
            }
            Self::Notifications(value) => ("notifications", to_raw_value(&value)),
            Self::UpdateCheck(value) => ("update_check", to_raw_value(&value)),
        };
        Ok((key, value?))
    }
}

fn read_document(path: &Path) -> io::Result<Document> {
    let source = fs::read_to_string(path)?;
    // Windows PowerShell 5.1 and older editors write UTF-8 with a BOM, which
    // serde_json rejects; without this the whole file would silently fall back to defaults.
    serde_json::from_str(source.strip_prefix('\u{feff}').unwrap_or(&source)).map_err(|_| {
        io::Error::new(
            ErrorKind::InvalidData,
            "settings.json must contain a JSON object",
        )
    })
}

fn raw_setting<'a>(document: &'a Document, key: &str) -> &'a str {
    document.get(key).map_or("null", |value| value.get())
}

fn read_bool(document: &Document, key: &str, default: bool) -> bool {
    match raw_setting(document, key) {
        "true" => true,
        "false" => false,
        _ => default,
    }
}

fn read_types(raw: &str, available: &[SyncKind]) -> Vec<SyncKind> {
    let selected: Vec<_> = serde_json::from_str::<Vec<&RawValue>>(raw)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| serde_json::from_str::<String>(value.get()).ok())
        .collect();
    let known: Vec<_> = available
        .iter()
        .copied()
        .filter(|kind| selected.iter().any(|value| value == kind.as_str()))
        .collect();
    if known.is_empty() {
        available.to_vec()
    } else {
        known
    }
}

fn stored_types(available: &[SyncKind], selected: &[SyncKind]) -> Vec<&'static str> {
    available
        .iter()
        .filter(|kind| selected.contains(kind))
        .map(|kind| kind.as_str())
        .collect()
}

fn read_file_ask_mb(raw: &str) -> u8 {
    // Match numeric choices by value so an existing 10.0 or 1e1 is equivalent to 10.
    let Ok(value) = serde_json::from_str::<f64>(raw) else {
        return DEFAULT_FILE_ASK_MB;
    };
    FILE_ASK_MB_CHOICES
        .into_iter()
        .find(|&choice| f64::from(choice) == value)
        .unwrap_or(DEFAULT_FILE_ASK_MB)
}
