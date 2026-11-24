//! Clipboard payloads shared by the device and desktop integrations.

/// One item ready to sync, or a file advertised by the phone.
#[derive(Clone, PartialEq, Eq)]
pub enum Clip {
    Text(String),
    Image { bytes: Vec<u8>, uti: String },
    File(FileOffer),
}

/// Identifies the phone pasteboard state an offer was created from.
#[derive(Clone, PartialEq, Eq)]
pub struct PasteboardChange {
    /// Opaque bytes representing the nonce when the phone supplies one.
    pub nonce: Option<Vec<u8>>,
    pub count: i64,
}

/// A file advertised by the phone, with inline data when available.
#[derive(Clone, PartialEq, Eq)]
pub struct FileOffer {
    pub name: String,
    pub uti: String,
    /// Size in bytes, or zero if the phone has not reported a size.
    pub size: u64,
    pub change: Option<PasteboardChange>,
    pub data: Option<Vec<u8>>,
}
