use std::{io::Cursor, time::Duration};

use copiago_core::{
    clip::{Clip, PasteboardChange},
    diagnostic,
    i18n::{Language, Message},
};
use plist::{Dictionary, Value};
use tokio::time::timeout;

use crate::DeviceEvent;

use super::{
    MAX_ALTERNATE_IMAGE_BYTES, PasteboardBackend, PasteboardError, PullPolicy, file_offer, pull,
};

pub(super) const INLINE_TEXT: [&str; 3] =
    ["public.utf8-plain-text", "public.plain-text", "public.text"];
pub(super) const PNG_UTI: &str = "public.png";
const JPEG_UTI: &str = "public.jpeg";
const HEIC_UTI: &str = "public.heic";
// JPEG first: converting a camera photo to PNG on the phone takes far longer and
// yields several times the bytes (a 12 MP photo is roughly 20-35 MB as PNG).
const HEIC_ALTERNATIVES: [&str; 2] = [JPEG_UTI, PNG_UTI];
// The phone transcodes a HEIC photo on request. Two seconds was too short for a
// full-resolution camera photo; these budgets are estimates pending device timing.
pub(super) const ALTERNATE_TIMEOUT: Duration = Duration::from_secs(12);
pub(super) const RESOLVE_TIMEOUT: Duration = Duration::from_secs(8);
// These also justify the lightweight content request. The current Python
// reader still extracts text only from the inline UTF-8 representations.
const DECLARED_TEXT: [&str; 3] = [
    "public.utf16-plain-text",
    "public.utf16-external-plain-text",
    "public.url",
];
const IMAGE_FORMATS: [(&str, &str); 6] = [
    (PNG_UTI, "image/png"),
    (JPEG_UTI, "image/jpeg"),
    (HEIC_UTI, "image/heic"),
    ("public.tiff", "image/tiff"),
    ("com.compuserve.gif", "image/gif"),
    ("org.webmproject.webp", "image/webp"),
];
const WEB_ARCHIVE: &str = "com.apple.webarchive";
pub(super) const DESCRIPTION_BYTES: i64 = 256 * 1024;

#[derive(Default)]
pub(super) struct Snapshot {
    pub change: Option<PasteboardChange>,
    items: Vec<Item>,
}

struct Item {
    index: usize,
    types: Vec<String>,
    entries: Dictionary,
}

impl Snapshot {
    pub(super) fn from_reply(reply: Value) -> Result<Self, PasteboardError> {
        let mut reply = reply
            .into_dictionary()
            .ok_or(PasteboardError::RequestFailed)?;
        let Some(mut board) = reply.remove("pasteboard").and_then(Value::into_dictionary) else {
            return Ok(Self::default());
        };
        let change = change_id(&board);
        let items = board
            .remove("items")
            .and_then(Value::into_array)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(index, value)| {
                // Keep the wire position for comparisons across PULL replies.
                let mut item = value.into_dictionary().unwrap_or_default();
                Item {
                    index,
                    types: item
                        .remove("types")
                        .and_then(Value::into_array)
                        .unwrap_or_default()
                        .into_iter()
                        .filter_map(Value::into_string)
                        .collect(),
                    entries: item
                        .remove("data")
                        .and_then(Value::into_dictionary)
                        .unwrap_or_default(),
                }
            })
            .collect();
        Ok(Self { change, items })
    }

    pub(super) fn types(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .flat_map(|item| item.types.iter().map(String::as_str))
    }

    pub(super) fn has_different_change(&self, other: &Self) -> bool {
        matches!((&self.change, &other.change), (Some(left), Some(right)) if left != right)
    }

    pub(super) fn file_item_index(&self, uti: &str) -> Option<usize> {
        self.items.iter().find_map(|item| {
            item.entries.get(uti)?.as_dictionary()?;
            Some(item.index)
        })
    }

    pub(super) fn into_item_types(self) -> Vec<Vec<String>> {
        self.items.into_iter().map(|item| item.types).collect()
    }

    pub(super) fn take_entry(&mut self, uti: &str) -> Option<Dictionary> {
        self.items.iter_mut().find_map(|item| item.take_entry(uti))
    }

    fn declares_image(&self) -> bool {
        self.types()
            .any(|name| IMAGE_FORMATS.iter().any(|(uti, _)| *uti == name))
    }

    fn declares_text(&self) -> bool {
        self.types()
            .any(|name| INLINE_TEXT.contains(&name) || DECLARED_TEXT.contains(&name))
    }

    fn item_at(&self, index: usize) -> Option<&Item> {
        self.items.iter().find(|item| item.index == index)
    }

    fn take_image(&mut self) -> Option<(usize, Clip)> {
        for item in &mut self.items {
            if let Some(image) = IMAGE_FORMATS
                .into_iter()
                .find_map(|(uti, _)| item.take_image(uti))
            {
                return Some((item.index, image));
            }
        }

        self.items.iter_mut().find_map(|item| {
            let archive = item.take_inline(WEB_ARCHIVE)?.into_data()?;
            Some((item.index, archive_image(archive)?))
        })
    }

    fn take_text(&mut self) -> Option<Clip> {
        let text = self.items.iter_mut().find_map(|item| {
            INLINE_TEXT.into_iter().find_map(|uti| {
                let text = match item.take_inline(uti)? {
                    Value::Data(bytes) => String::from_utf8(bytes).ok()?,
                    Value::String(text) => text,
                    _ => return None,
                };
                (!text.is_empty()).then_some(text)
            })
        })?;
        (!text
            .trim_matches(['\u{fffc}', ' ', '\t', '\r', '\n'])
            .is_empty())
        .then_some(Clip::Text(text))
    }

    fn unsupported_kind(&self) -> Option<Message> {
        if self.types().any(is_video) {
            Some(Message::new("视频"))
        } else if self.types().any(is_file_like) {
            Some(Message::new("文件"))
        } else {
            self.types().next().map(|_| Message::new("文件或其他内容"))
        }
    }

    /// What the user copied, for a failure message. Declared types are enough here;
    /// the data itself may be missing.
    fn copied_kind(&self) -> Message {
        Message::new(if self.declares_image() {
            "图片"
        } else if self.types().any(is_file_like) || file_offer::content_type(self).is_some() {
            "文件"
        } else if self.declares_text() {
            "文本"
        } else {
            "内容"
        })
    }

    fn read_failed(self, stage: &str, timed_out: bool) -> Update {
        self.log_types(stage);
        Update {
            event: Some(DeviceEvent::ReadFailed {
                kind: self.copied_kind(),
                timed_out,
            }),
            change: self.change,
            skipped: true,
        }
    }

    fn log_types(&self, stage: &str) {
        // UTI strings come from the peer. Only emit fixed categories and counts.
        let mut counts = [0_u32; 5];
        for name in self.types() {
            let kind = if INLINE_TEXT.contains(&name) || DECLARED_TEXT.contains(&name) {
                0
            } else if IMAGE_FORMATS.iter().any(|(uti, _)| *uti == name) || name == WEB_ARCHIVE {
                1
            } else if is_video(name) {
                2
            } else if is_file_like(name) {
                3
            } else {
                4
            };
            counts[kind] += 1;
        }
        let [text, image, video, file, other] = counts;
        diagnostic::event(format_args!(
            "{stage} types=text:{text},image:{image},video:{video},file:{file},other:{other}"
        ));
    }

    fn into_update(self, clip: Option<Clip>) -> Update {
        let event = match clip {
            Some(Clip::File(offer)) => Some(DeviceEvent::FileOffer(offer)),
            Some(clip) => Some(DeviceEvent::Clip {
                clip,
                while_disconnected: false,
            }),
            None => self.unsupported_kind().map(|kind| {
                self.log_types("unsupported");
                DeviceEvent::Unsupported { kind }
            }),
        };
        Update {
            change: self.change,
            event,
            skipped: false,
        }
    }

    /// Abandons this change but tells the user, instead of dropping it silently.
    fn timed_out(self) -> Update {
        self.read_failed("pull timeout", true)
    }

    fn skip(self) -> Update {
        Update {
            change: self.change,
            event: None,
            skipped: true,
        }
    }
}

impl Item {
    fn take_entry(&mut self, uti: &str) -> Option<Dictionary> {
        self.entries.remove(uti)?.into_dictionary()
    }

    fn take_inline(&mut self, uti: &str) -> Option<Value> {
        self.take_entry(uti)?.remove("data")
    }

    fn take_image(&mut self, uti: &str) -> Option<Clip> {
        let bytes = self.take_inline(uti)?.into_data()?;
        (!bytes.is_empty()).then(|| Clip::Image {
            bytes,
            uti: uti.to_owned(),
        })
    }

    fn declares(&self, uti: &str) -> bool {
        self.types.iter().any(|ty| ty == uti)
    }

    fn permits_alternate(&self, uti: &str) -> bool {
        self.declares(uti)
            && self
                .entries
                .get(uti)
                .and_then(Value::as_dictionary)
                .and_then(|entry| entry.get("size"))
                .and_then(Value::as_unsigned_integer)
                .is_none_or(|size| size <= MAX_ALTERNATE_IMAGE_BYTES as u64)
    }
}

pub(super) fn change_id(board: &Dictionary) -> Option<PasteboardChange> {
    let metadata = board.get("metadata")?.as_dictionary()?;
    metadata_change_id(metadata)
}

pub(super) fn metadata_change_id(metadata: &Dictionary) -> Option<PasteboardChange> {
    let count = metadata.get("changeCount")?.as_signed_integer()?;
    // Keep a typed representation: equal-looking text and binary nonces are
    // different. RemoteXPC maps XPC null to an empty plist string.
    let nonce = metadata
        .get("nonce")
        .filter(|value| value.as_string() != Some(""))
        .and_then(|value| {
            let mut binary = Vec::new();
            value.to_writer_binary(&mut binary).ok()?;
            Some(binary)
        });
    Some(PasteboardChange { nonce, count })
}

fn archive_image(archive: Vec<u8>) -> Option<Clip> {
    let mut archive = Value::from_reader(Cursor::new(archive))
        .ok()?
        .into_dictionary()?;
    let resources = archive.remove("WebSubresources")?.into_array()?;
    resources.into_iter().find_map(|resource| {
        let mut resource = resource.into_dictionary()?;
        let mime = resource.get("WebResourceMIMEType")?.as_string()?;
        let (uti, _) = IMAGE_FORMATS.iter().find(|(_, format)| *format == mime)?;
        let bytes = resource.remove("WebResourceData")?.into_data()?;
        (!bytes.is_empty()).then(|| Clip::Image {
            bytes,
            uti: (*uti).to_owned(),
        })
    })
}

fn is_video(uti: &str) -> bool {
    uti.contains("movie")
        || uti.contains("video")
        || matches!(uti, "public.mpeg-4" | "public.avi" | "public.mpeg")
}

fn is_file_like(uti: &str) -> bool {
    matches!(uti, "public.file-url" | "public.url")
        || uti.starts_with("com.adobe.pdf")
        || uti.starts_with("public.zip")
}

async fn heic_alternate(
    backend: &mut impl PasteboardBackend,
    item: &Item,
    change: &PasteboardChange,
) -> Option<Clip> {
    let mut candidates = HEIC_ALTERNATIVES
        .into_iter()
        .filter(|uti| item.declares(uti))
        .peekable();
    candidates.peek()?;
    timeout(ALTERNATE_TIMEOUT, async {
        let mut current = backend.alternate_listing().await.ok()?;
        if current.change.as_ref() != Some(change) {
            return None;
        }
        while let Some(uti) = candidates.next() {
            if !current.item_at(item.index)?.permits_alternate(uti) {
                continue;
            }
            let bytes = timeout(RESOLVE_TIMEOUT, backend.resolve_alternate(item.index, uti))
                .await
                .ok()
                .and_then(Result::ok)
                .filter(|bytes| looks_like_alternate(uti, bytes));
            if bytes.is_none() && candidates.peek().is_none() {
                break;
            }
            // A RESOLVE DATA reply does not carry the change ID. Check it again
            // before accepting these bytes or resolving the next representation.
            current = backend.alternate_listing().await.ok()?;
            if current.change.as_ref() != Some(change) {
                return None;
            }
            if let Some(bytes) = bytes
                && current.item_at(item.index)?.declares(uti)
            {
                return Some(Clip::Image {
                    bytes,
                    uti: uti.to_owned(),
                });
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

fn looks_like_alternate(uti: &str, bytes: &[u8]) -> bool {
    match uti {
        PNG_UTI => bytes.len() >= 33 && bytes.starts_with(b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR"),
        JPEG_UTI => bytes.len() > 3 && bytes.starts_with(b"\xff\xd8\xff"),
        _ => false,
    }
}

pub(super) struct Update {
    pub change: Option<PasteboardChange>,
    pub event: Option<DeviceEvent>,
    pub skipped: bool,
}

pub(super) async fn read_clip(
    backend: &mut impl PasteboardBackend,
    listing: Snapshot,
    language: impl FnOnce() -> Language,
) -> Result<Update, PasteboardError> {
    if let Some(uti) = file_offer::content_type(&listing).map(str::to_owned) {
        return match pull(backend, PullPolicy::Threshold(DESCRIPTION_BYTES)).await {
            // Remember the listing's ID so the next poll can discover the
            // newer change and fetch its own description.
            Ok(described) if listing.has_different_change(&described) => Ok(listing.skip()),
            Ok(described) => {
                let offer = file_offer::describe(described, uti, listing.change.clone(), language);
                Ok(listing.into_update(Some(Clip::File(offer))))
            }
            Err(PasteboardError::RequestTimeout) => Ok(listing.timed_out()),
            Err(error) => Err(error),
        };
    }

    if !listing.declares_text() && !listing.declares_image() {
        return Ok(listing.into_update(None));
    }

    let mut secondary = match pull(backend, PullPolicy::PromiseSecondary).await {
        Ok(snapshot) => snapshot,
        Err(PasteboardError::RequestTimeout) => return Ok(listing.timed_out()),
        Err(error) => return Err(error),
    };
    if let Some((index, mut image)) = secondary.take_image() {
        if matches!(&image, Clip::Image { uti, .. } if uti == HEIC_UTI)
            && let Some(change) = secondary.change.as_ref()
            && let Some(item) = secondary.item_at(index)
            && let Some(alternative) = heic_alternate(backend, item, change).await
        {
            image = alternative;
        }
        return Ok(secondary.into_update(Some(image)));
    }

    // Some apps advertise raster bytes as a secondary type. If resolution
    // fails, keep the previous Windows clipboard even if text was inline.
    if secondary.declares_image() {
        return match pull(backend, PullPolicy::AllResolved).await {
            Ok(mut resolved) => match resolved.take_image() {
                Some((_, image)) => Ok(resolved.into_update(Some(image))),
                // Declared as an image but no bytes: not an unsupported format.
                None => Ok(resolved.read_failed("image unavailable", false)),
            },
            Err(PasteboardError::RequestTimeout) => Ok(secondary.timed_out()),
            Err(error) => Err(error),
        };
    }

    let text = secondary.take_text();
    Ok(secondary.into_update(text))
}
