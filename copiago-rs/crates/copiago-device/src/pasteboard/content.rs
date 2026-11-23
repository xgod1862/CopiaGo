use std::io::Cursor;

use copiago_core::{
    clip::{Clip, PasteboardChange},
    i18n::{Language, Message},
};
use plist::{Dictionary, Value};

use crate::DeviceEvent;

use super::{PasteboardBackend, PasteboardError, PullPolicy, file_offer, pull};

pub(super) const INLINE_TEXT: [&str; 3] =
    ["public.utf8-plain-text", "public.plain-text", "public.text"];
pub(super) const PNG_UTI: &str = "public.png";
// These also justify the lightweight content request. The current Python
// reader still extracts text only from the inline UTF-8 representations.
const DECLARED_TEXT: [&str; 3] = [
    "public.utf16-plain-text",
    "public.utf16-external-plain-text",
    "public.url",
];
const IMAGE_FORMATS: [(&str, &str); 6] = [
    (PNG_UTI, "image/png"),
    ("public.jpeg", "image/jpeg"),
    ("public.heic", "image/heic"),
    ("public.tiff", "image/tiff"),
    ("com.compuserve.gif", "image/gif"),
    ("org.webmproject.webp", "image/webp"),
];
const WEB_ARCHIVE: &str = "com.apple.webarchive";
const DESCRIPTION_BYTES: i64 = 256 * 1024;

#[derive(Default)]
pub(super) struct Snapshot {
    pub change: Option<PasteboardChange>,
    items: Vec<Item>,
}

struct Item {
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
            .filter_map(Value::into_dictionary)
            .map(|mut item| Item {
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
            })
            .collect();
        Ok(Self { change, items })
    }

    pub(super) fn types(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .flat_map(|item| item.types.iter().map(String::as_str))
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

    fn take_image(&mut self) -> Option<Clip> {
        for item in &mut self.items {
            for (uti, _) in IMAGE_FORMATS {
                if let Some(bytes) = item.take_inline(uti).and_then(Value::into_data)
                    && !bytes.is_empty()
                {
                    return Some(Clip::Image {
                        bytes,
                        uti: uti.to_owned(),
                    });
                }
            }
        }

        self.items.iter_mut().find_map(|item| {
            let archive = item.take_inline(WEB_ARCHIVE)?.into_data()?;
            archive_image(archive)
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

    fn into_update(self, clip: Option<Clip>) -> Update {
        let event = match clip {
            Some(Clip::File(offer)) => Some(DeviceEvent::FileOffer(offer)),
            Some(clip) => Some(DeviceEvent::Clip {
                clip,
                while_disconnected: false,
            }),
            None => self
                .unsupported_kind()
                .map(|kind| DeviceEvent::Unsupported { kind }),
        };
        Update {
            change: self.change,
            event,
            timed_out: false,
        }
    }

    fn skip(self) -> Update {
        Update {
            change: self.change,
            event: None,
            timed_out: true,
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
}

pub(super) fn change_id(board: &Dictionary) -> Option<PasteboardChange> {
    let metadata = board.get("metadata")?.as_dictionary()?;
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

pub(super) struct Update {
    pub change: Option<PasteboardChange>,
    pub event: Option<DeviceEvent>,
    pub timed_out: bool,
}

pub(super) async fn read_clip(
    backend: &mut impl PasteboardBackend,
    listing: Snapshot,
    language: Language,
) -> Result<Update, PasteboardError> {
    if let Some(uti) = file_offer::content_type(&listing).map(str::to_owned) {
        return match pull(backend, PullPolicy::Threshold(DESCRIPTION_BYTES)).await {
            Ok(described) => {
                let offer = file_offer::describe(described, uti, listing.change.clone(), language);
                Ok(listing.into_update(Some(Clip::File(offer))))
            }
            Err(PasteboardError::RequestTimeout) => Ok(listing.skip()),
            Err(error) => Err(error),
        };
    }

    if !listing.declares_text() && !listing.declares_image() {
        return Ok(listing.into_update(None));
    }

    let mut secondary = match pull(backend, PullPolicy::PromiseSecondary).await {
        Ok(snapshot) => snapshot,
        Err(PasteboardError::RequestTimeout) => return Ok(listing.skip()),
        Err(error) => return Err(error),
    };
    if let Some(image) = secondary.take_image() {
        return Ok(secondary.into_update(Some(image)));
    }

    // Some apps advertise raster bytes as a secondary type. If resolution
    // fails, keep the previous Windows clipboard even if text was inline.
    if secondary.declares_image() {
        return match pull(backend, PullPolicy::AllResolved).await {
            Ok(mut resolved) => {
                let image = resolved.take_image();
                Ok(resolved.into_update(image))
            }
            Err(PasteboardError::RequestTimeout) => Ok(secondary.into_update(None)),
            Err(error) => Err(error),
        };
    }

    let text = secondary.take_text();
    Ok(secondary.into_update(text))
}
