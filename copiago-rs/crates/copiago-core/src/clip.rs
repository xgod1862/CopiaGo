//! Clipboard payloads shared by the device and desktop integrations.

use sha2::{Digest, Sha256};

/// One item ready to sync, or a file advertised by the phone.
#[derive(Clone, PartialEq, Eq)]
pub enum Clip {
    Text(String),
    Image { bytes: Vec<u8>, uti: String },
    File(FileOffer),
}

impl Clip {
    /// SHA-256 of the payload bytes, matching Python's clipboard fingerprint.
    pub fn fingerprint(&self) -> [u8; 32] {
        let bytes = match self {
            Self::Text(text) => text.as_bytes(),
            Self::Image { bytes, .. } => bytes,
            Self::File(offer) => offer.data.as_deref().unwrap_or_default(),
        };
        Sha256::digest(bytes).into()
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprint_uses_sha256_of_the_payload_without_format_or_name() {
        let text = "你好😀";
        let expected = "3fe21f1528f50ff70f66dcc897c54e682d482fb0b40155d0b6b3b90d5dd0c17f";
        let image = Clip::Image {
            bytes: text.as_bytes().to_vec(),
            uti: "arbitrary-metadata".to_owned(),
        };
        let mut offer = FileOffer {
            name: "sample.txt".to_owned(),
            uti: "public.plain-text".to_owned(),
            size: text.len() as u64,
            change: None,
            data: Some(text.as_bytes().to_vec()),
        };
        for clip in [
            Clip::Text(text.to_owned()),
            image,
            Clip::File(offer.clone()),
        ] {
            assert_eq!(hex_digest(&clip), expected);
        }
        offer.data = None;
        assert_eq!(
            hex_digest(&Clip::File(offer)),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    fn hex_digest(clip: &Clip) -> String {
        clip.fingerprint()
            .into_iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }
}
