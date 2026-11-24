use copiago_core::clip::PasteboardChange;
use idevice::xpc::XPCObject;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::pasteboard::content::metadata_change_id;

use super::{
    FileTransferError as Error, MAX_METADATA_BODY, Method, check_size, frames::ReplyFrames,
};

pub(super) const MAX_KEY_BYTES: usize = 1024;
const XPC_HEADER: [u8; 8] = [0x42, 0x37, 0x13, 0x42, 5, 0, 0, 0];
const NULL: u32 = 0x1000;
const BOOL: u32 = 0x2000;
const DATA: u32 = 0x8000;
const STRING: u32 = 0x9000;
const ARRAY: u32 = 0xe000;
const DICTIONARY: u32 = 0xf000;
const FILE_TRANSFER: u32 = 0x1a000;

#[derive(Clone, Copy)]
enum Path {
    Root,
    Board,
    Item,
    Representations,
    Entry,
}

/// Follow just the two known reply paths and stream the selected XPC Data.
/// Container sizes allow unrelated entries to be skipped without decoding them.
pub(super) async fn scan(
    frames: &mut ReplyFrames<'_, impl AsyncRead + AsyncWrite + Unpin>,
    size: u64,
    method: Method,
    uti: &str,
    expected: Option<u64>,
    change: Option<&PasteboardChange>,
    destination: &mut (impl AsyncWrite + Unpin),
) -> Result<Option<u64>, Error> {
    let mut scan = Scan {
        frames,
        offset: 0,
        method,
        uti,
        expected,
        change,
        destination,
        command_seen: false,
        entry_seen: false,
        payload_seen: false,
        written: None,
    };
    let mut header = [0; 8];
    scan.read(size, &mut header).await?;
    if header != XPC_HEADER || scan.word(size).await? != DICTIONARY {
        return Err(Error::InvalidReply);
    }
    let entries = scan.dictionary(Path::Root, size).await?;
    if scan.offset != size {
        return Err(Error::InvalidReply);
    }
    if entries == 0 {
        return Ok(None); // Empty-dictionary handshake or keepalive.
    }
    if !scan.command_seen {
        return Err(Error::InvalidReply);
    }
    scan.written.map(Some).ok_or(Error::DataUnavailable)
}

struct Scan<'a, 'stream, R, W> {
    frames: &'a mut ReplyFrames<'stream, R>,
    offset: u64,
    method: Method,
    uti: &'a str,
    expected: Option<u64>,
    change: Option<&'a PasteboardChange>,
    destination: &'a mut W,
    command_seen: bool,
    entry_seen: bool,
    payload_seen: bool,
    written: Option<u64>,
}

impl<R: AsyncRead + AsyncWrite + Unpin, W: AsyncWrite + Unpin> Scan<'_, '_, R, W> {
    async fn dictionary(&mut self, path: Path, limit: u64) -> Result<u32, Error> {
        let (end, entries) = self.container(limit).await?;
        let mut key = [0; MAX_KEY_BYTES];
        for _ in 0..entries {
            let length = self.key(end, &mut key).await?;
            let key = &key[..length];
            let tag = self.word(end).await?;
            match (path, key, tag) {
                (Path::Root, b"command", STRING) => self.command(end).await?,
                (Path::Root, b"command", _) => return Err(Error::InvalidReply),
                (Path::Root, b"data", _) if self.method == Method::Resolve => {
                    self.payload(end, tag).await?;
                }
                (Path::Root, b"pasteboard", DICTIONARY) if self.method == Method::Threshold => {
                    Box::pin(self.dictionary(Path::Board, end)).await?;
                }
                (Path::Board, b"items", ARRAY) => self.items(end).await?,
                (Path::Board, b"metadata", DICTIONARY) if self.change.is_some() => {
                    self.check_change(end).await?;
                }
                (Path::Item, b"data", DICTIONARY) if !self.entry_seen => {
                    Box::pin(self.dictionary(Path::Representations, end)).await?;
                }
                (Path::Representations, key, DICTIONARY)
                    if !self.entry_seen && key == self.uti.as_bytes() =>
                {
                    // Python selects the first item with a dictionary for the
                    // requested UTI, even if that entry stays promised.
                    self.entry_seen = true;
                    Box::pin(self.dictionary(Path::Entry, end)).await?;
                }
                (Path::Entry, b"data", _) => self.payload(end, tag).await?,
                _ => self.skip_value(end, tag).await?,
            }
        }
        if self.offset != end {
            return Err(Error::InvalidReply);
        }
        Ok(entries)
    }

    async fn items(&mut self, limit: u64) -> Result<(), Error> {
        let (end, items) = self.container(limit).await?;
        for _ in 0..items {
            let tag = self.word(end).await?;
            if tag == DICTIONARY && !self.entry_seen {
                Box::pin(self.dictionary(Path::Item, end)).await?;
            } else {
                self.skip_value(end, tag).await?;
            }
        }
        if self.offset != end {
            return Err(Error::InvalidReply);
        }
        Ok(())
    }

    async fn command(&mut self, end: u64) -> Result<(), Error> {
        let expected = match self.method {
            Method::Threshold => b"PULL_REPLY\0".as_slice(),
            Method::Resolve => b"DATA\0".as_slice(),
        };
        let length = u64::from(self.word(end).await?);
        if self.command_seen || length != expected.len() as u64 {
            return Err(Error::InvalidReply);
        }
        let mut bytes = [0; 12];
        let bytes = &mut bytes[..padded(length) as usize];
        self.read(end, bytes).await?;
        if &bytes[..length as usize] != expected {
            return Err(Error::InvalidReply);
        }
        self.command_seen = true;
        Ok(())
    }

    async fn payload(&mut self, end: u64, tag: u32) -> Result<(), Error> {
        if std::mem::replace(&mut self.payload_seen, true) {
            return Err(Error::InvalidReply);
        }
        match tag {
            NULL => Ok(()), // An expired or unfulfilled promise may be null.
            FILE_TRANSFER => Err(Error::UnsupportedPayload),
            DATA => {
                let length = u64::from(self.word(end).await?);
                check_size(length, self.expected)?;
                self.advance(end, padded(length))?;
                self.frames.copy(length, self.destination).await?;
                self.frames.skip(padded(length) - length).await?;
                self.written = Some(length);
                Ok(())
            }
            _ => Err(Error::InvalidReply),
        }
    }

    async fn check_change(&mut self, end: u64) -> Result<(), Error> {
        let length = u64::from(self.word(end).await?);
        if length > MAX_METADATA_BODY {
            return Err(Error::TooLarge);
        }
        let mut body = XPC_HEADER.to_vec();
        body.extend_from_slice(&DICTIONARY.to_le_bytes());
        body.extend_from_slice(&(length as u32).to_le_bytes());
        let header = body.len();
        body.resize(header + length as usize, 0);
        self.read(end, &mut body[header..]).await?;
        let value = decode_small(&body)?.to_plist();
        let change = value.as_dictionary().and_then(metadata_change_id);
        if change.is_some() && change.as_ref() != self.change {
            return Err(Error::OfferExpired);
        }
        Ok(())
    }

    async fn container(&mut self, limit: u64) -> Result<(u64, u32), Error> {
        let length = u64::from(self.word(limit).await?);
        let end = self.end(limit, length)?;
        let entries = self.word(end).await?;
        Ok((end, entries))
    }

    async fn key(&mut self, end: u64, key: &mut [u8]) -> Result<usize, Error> {
        for (index, byte) in key.iter_mut().enumerate() {
            self.read(end, std::slice::from_mut(byte)).await?;
            if *byte == 0 {
                let length = index as u64 + 1;
                self.skip(end, padded(length) - length).await?;
                return Ok(index);
            }
        }
        Err(Error::InvalidReply)
    }

    async fn skip_value(&mut self, end: u64, mut tag: u32) -> Result<(), Error> {
        while tag == FILE_TRANSFER {
            self.skip(end, 8).await?; // File-transfer message ID.
            tag = self.word(end).await?;
        }
        let bytes = match tag {
            STRING | DATA => padded(u64::from(self.word(end).await?)),
            ARRAY | DICTIONARY => u64::from(self.word(end).await?),
            _ => scalar_length(tag).ok_or(Error::InvalidReply)?,
        };
        self.skip(end, bytes).await
    }

    async fn word(&mut self, end: u64) -> Result<u32, Error> {
        let mut bytes = [0; 4];
        self.read(end, &mut bytes).await?;
        Ok(u32::from_le_bytes(bytes))
    }

    async fn read(&mut self, end: u64, out: &mut [u8]) -> Result<(), Error> {
        self.advance(end, out.len() as u64)?;
        self.frames.read(out).await
    }

    async fn skip(&mut self, end: u64, bytes: u64) -> Result<(), Error> {
        self.advance(end, bytes)?;
        self.frames.skip(bytes).await
    }

    fn advance(&mut self, end: u64, bytes: u64) -> Result<(), Error> {
        self.offset = self.end(end, bytes)?;
        Ok(())
    }

    fn end(&self, limit: u64, bytes: u64) -> Result<u64, Error> {
        self.offset
            .checked_add(bytes)
            .filter(|end| *end <= limit)
            .ok_or(Error::InvalidReply)
    }
}

fn padded(length: u64) -> u64 {
    length.next_multiple_of(4)
}

fn scalar_length(tag: u32) -> Option<u64> {
    Some(match tag {
        NULL => 0,
        BOOL => 4,
        0x3000 | 0x4000 | 0x5000 | 0x7000 => 8, // Integers, double, date.
        0xa000 => 16,                           // UUID.
        _ => return None,
    })
}

/// Validate length-prefixed fields before invoking the normal decoder. Its
/// Data/String decoder allocates from the declared length before reading bytes.
pub(super) fn decode_small(body: &[u8]) -> Result<XPCObject, Error> {
    let mut objects = body.strip_prefix(&XPC_HEADER).ok_or(Error::InvalidReply)?;
    check_object(&mut objects, 0)?;
    if !objects.is_empty() {
        return Err(Error::InvalidReply);
    }
    XPCObject::decode(body).map_err(|_| Error::InvalidReply)
}

fn check_object(input: &mut &[u8], depth: u8) -> Result<(), Error> {
    if depth >= 32 {
        return Err(Error::InvalidReply);
    }
    let tag = read_word(input)?;
    match tag {
        ARRAY | DICTIONARY => {
            let length = u64::from(read_word(input)?);
            let mut contents = consume(input, length)?;
            let entries = read_word(&mut contents)?;
            for _ in 0..entries {
                if tag == DICTIONARY {
                    let end = contents
                        .iter()
                        .position(|byte| *byte == 0)
                        .ok_or(Error::InvalidReply)?;
                    consume(&mut contents, padded(end as u64 + 1))?;
                }
                check_object(&mut contents, depth + 1)?;
            }
            if !contents.is_empty() {
                return Err(Error::InvalidReply);
            }
        }
        STRING | DATA => {
            let length = u64::from(read_word(input)?);
            consume(input, padded(length))?;
        }
        FILE_TRANSFER => {
            consume(input, 8)?;
            check_object(input, depth + 1)?;
        }
        _ => {
            consume(input, scalar_length(tag).ok_or(Error::InvalidReply)?)?;
        }
    }
    Ok(())
}

fn read_word(input: &mut &[u8]) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(consume(input, 4)?.try_into().unwrap()))
}

fn consume<'a>(input: &mut &'a [u8], length: u64) -> Result<&'a [u8], Error> {
    if length > input.len() as u64 {
        return Err(Error::InvalidReply);
    }
    let (head, tail) = input.split_at(length as usize);
    *input = tail;
    Ok(head)
}
