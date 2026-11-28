use copiago_core::{
    clip::{FileOffer, PasteboardChange},
    files::{fallback_name, safe_name},
    i18n::Language,
};
use plist::Value;

use super::content::Snapshot;

pub(super) const FILE_DESCRIPTOR: &str = "com.apple.DocumentManager.FPItem.File";
pub(super) const FINODE_DESCRIPTOR: &str = "com.apple.DocumentManager.FINode.File";
const PHOTO_IDENTIFIER: &str = "com.apple.mobileslideshow.asset.localidentifier";

pub(super) fn content_type(snapshot: &Snapshot) -> Option<&str> {
    if !snapshot
        .types()
        .any(|uti| matches!(uti, FILE_DESCRIPTOR | FINODE_DESCRIPTOR))
    {
        return None;
    }
    // Python sorts the declared types before choosing the file representation.
    snapshot
        .types()
        .filter(|uti| !matches!(*uti, FILE_DESCRIPTOR | FINODE_DESCRIPTOR | PHOTO_IDENTIFIER))
        .min()
        .filter(|uti| !uti.is_empty())
}

pub(super) fn describe(
    mut snapshot: Snapshot,
    uti: String,
    change: Option<PasteboardChange>,
    language: impl FnOnce() -> Language,
) -> FileOffer {
    let mut entry = snapshot.take_entry(&uti).unwrap_or_default();
    let data = entry.remove("data").and_then(Value::into_data);
    let size = data.as_ref().map_or_else(
        || {
            entry
                .get("size")
                .and_then(Value::as_unsigned_integer)
                .unwrap_or_default()
        },
        |bytes| bytes.len() as u64,
    );

    let extension = extension_for(&uti);
    let name = [FILE_DESCRIPTOR, FINODE_DESCRIPTOR]
        .into_iter()
        .find_map(|kind| {
            let bytes = snapshot.take_entry(kind)?.remove("data")?.into_data()?;
            let name = if kind == FINODE_DESCRIPTOR {
                finode_name(&bytes)?
            } else {
                descriptor_name(&bytes, extension)?
            };
            safe_name(&name, extension.unwrap_or("bin"))
        })
        .unwrap_or_else(|| fallback_name(language(), extension.unwrap_or("bin")));

    FileOffer {
        name,
        uti,
        size,
        change: change.or(snapshot.change),
        data,
    }
}

fn finode_name(bytes: &[u8]) -> Option<String> {
    let archive = Value::from_reader(std::io::Cursor::new(bytes)).ok()?;
    let archive = archive.as_dictionary()?;
    let objects = archive.get("$objects")?.as_array()?;
    let resolve = |uid: &Value| objects.get(usize::try_from(uid.as_uid()?.get()).ok()?);

    // FINode archives an NSURL. Follow its references so unrelated strings and
    // bookmark data in the same object table cannot supply the display name.
    let top = archive.get("$top")?.as_dictionary()?;
    let node = resolve(top.get("root")?)?.as_dictionary()?;
    let url = resolve(node.get("FI URL")?)?.as_dictionary()?;
    let relative = resolve(url.get("NS.relative")?)?.as_string()?;
    let location = relative.strip_prefix("file://")?.split(['?', '#']).next()?;
    let (_, name) = location.rsplit_once('/')?;
    let mut encoded = name.bytes();
    let mut decoded = Vec::with_capacity(name.len());
    while let Some(byte) = encoded.next() {
        decoded.push(if byte == b'%' {
            let high = char::from(encoded.next()?).to_digit(16)?;
            let low = char::from(encoded.next()?).to_digit(16)?;
            (high * 16 + low) as u8
        } else {
            byte
        });
    }
    String::from_utf8(decoded).ok()
}

fn descriptor_name(archive: &[u8], expected_extension: Option<&str>) -> Option<String> {
    let mut archive = Value::from_reader(std::io::Cursor::new(archive))
        .ok()?
        .into_dictionary()?;
    let mut names = archive
        .remove("$objects")?
        .into_array()?
        .into_iter()
        .filter_map(Value::into_string)
        .filter(|name| {
            !name.contains('/')
                && name.rsplit_once('.').is_some_and(|(_, extension)| {
                    (1..=8).contains(&extension.len())
                        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
        });

    let first = names.next()?;
    let Some(expected) = expected_extension else {
        return Some(first);
    };
    let has_expected_extension = |name: &str| {
        name.rsplit_once('.')
            .is_some_and(|(_, extension)| extension.eq_ignore_ascii_case(expected))
    };
    if has_expected_extension(&first) {
        return Some(first);
    }
    names
        .find(|name| has_expected_extension(name))
        .or(Some(first))
}

fn extension_for(uti: &str) -> Option<&'static str> {
    Some(match uti {
        "com.adobe.pdf" => "pdf",
        "org.openxmlformats.wordprocessingml.document" => "docx",
        "org.openxmlformats.spreadsheetml.sheet" => "xlsx",
        "org.openxmlformats.presentationml.presentation" => "pptx",
        "com.microsoft.word.doc" => "doc",
        "com.microsoft.excel.xls" => "xls",
        "com.microsoft.powerpoint.ppt" => "ppt",
        "com.apple.keynote.key" => "key",
        "com.apple.iwork.pages.pages" => "pages",
        "com.apple.iwork.numbers.numbers" => "numbers",
        "public.zip-archive" => "zip",
        "public.html" => "html",
        "public.plain-text" | "public.utf8-plain-text" => "txt",
        "public.rtf" => "rtf",
        "public.json" => "json",
        "public.comma-separated-values-text" => "csv",
        "public.mpeg-4" => "mp4",
        "com.apple.quicktime-movie" => "mov",
        "public.mp3" => "mp3",
        "public.mpeg-4-audio" => "m4a",
        "public.jpeg" => "jpg",
        "public.png" => "png",
        "public.heic" => "heic",
        _ => return None,
    })
}
