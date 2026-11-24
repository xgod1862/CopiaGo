//! A synthetic peer for exercising the file reader without a phone.

use idevice::xpc::{XPCMessage, XPCObject};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, duplex};

use super::{
    FileTransferError as Error, Method, fetch_request,
    frames::{
        FRAME_BYTES, PREFACE, REPLY, ROOT, WRAPPER_MAGIC, frame_length, frame_stream, write_frame,
    },
    receive,
};

const SAMPLE_UTI: &str = "public.zip-archive";
const SAMPLE_ITEM: usize = 1;
const INITIAL_WINDOW: u32 = 1024 * 1024;

pub(crate) async fn run(
    method: Method,
    size: u64,
    out: &mut (impl AsyncWrite + Unpin),
) -> Result<(u64, [u8; 32]), Error> {
    let (mut client, mut server) = duplex(128 * 1024);
    let client = receive(
        &mut client,
        method,
        SAMPLE_UTI,
        SAMPLE_ITEM,
        Some(size),
        None,
        out,
    );
    let server = async {
        let request = fetch_request(method, SAMPLE_UTI, SAMPLE_ITEM, Some(size));
        let mut peer = Peer::handshake(&mut server, request).await?;
        peer.send_file(method, size).await
    };
    tokio::try_join!(client, server)
}

struct Peer<'a, S> {
    socket: &'a mut S,
    connection: u32,
    reply: u32,
}

impl<'a, S: AsyncRead + AsyncWrite + Unpin> Peer<'a, S> {
    async fn handshake(socket: &'a mut S, request: XPCObject) -> Result<Self, Error> {
        let mut preface = [0; PREFACE.len()];
        socket
            .read_exact(&mut preface)
            .await
            .map_err(Error::from_read)?;
        if preface != PREFACE {
            return Err(Error::InvalidReply);
        }
        let mut peer = Self {
            socket,
            connection: 65_535,
            reply: 65_535,
        };
        loop {
            let (kind, stream, body) = read_frame(peer.socket).await?;
            match (kind, stream) {
                (4, 0) => {
                    for option in body.as_chunks::<6>().0 {
                        if option[..2] == [0, 4] {
                            peer.reply = u32::from_be_bytes(option[2..].try_into().unwrap());
                        }
                    }
                }
                (8, 0) => peer.connection += increment(&body)?,
                (0, ROOT) => {
                    let message = XPCMessage::decode(&body).map_err(|_| Error::InvalidReply)?;
                    if message.flags & 0x0001_0000 != 0 {
                        if message.message != Some(request)
                            || peer.connection != INITIAL_WINDOW
                            || peer.reply != INITIAL_WINDOW
                        {
                            return Err(Error::InvalidReply);
                        }
                        return Ok(peer);
                    }
                }
                _ => {}
            }
        }
    }

    async fn send_file(&mut self, method: Method, size: u64) -> Result<[u8; 32], Error> {
        write_frame(self.socket, 4, 0, 0, &[])
            .await
            .map_err(Error::from_read)?;
        let keepalive = XPCMessage {
            flags: 1,
            message: None,
            message_id: None,
        }
        .encode(1)
        .map_err(|_| Error::InvalidReply)?;
        self.send(ROOT, 0, &keepalive).await?;
        self.send(REPLY, 0, &keepalive[..5]).await?;
        self.send(REPLY, 0, &keepalive[5..]).await?;

        let (prefix, suffix) = envelope(method, size);
        // Split wrapper and object headers across frames independently.
        for part in prefix.chunks(17) {
            self.send(REPLY, 0, part).await?;
        }
        let mut sha = Sha256::new();
        let mut block = [0; FRAME_BYTES];
        // The payload is deterministic and carries an offset on each block so
        // duplicated or reordered frames cannot pass a digest comparison.
        let mut state = 6_124_739u32;
        for byte in &mut block {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            *byte = state as u8;
        }
        for offset in (0..size).step_by(block.len()) {
            block[..8].copy_from_slice(&offset.to_le_bytes());
            let count = (size - offset).min(block.len() as u64) as usize;
            let part = &block[..count];
            sha.update(part);
            self.send(REPLY, 0, part).await?;
        }
        self.send(REPLY, 1, &suffix).await?;
        Ok(sha.finalize().into())
    }

    async fn send(&mut self, stream: u32, flags: u8, bytes: &[u8]) -> Result<(), Error> {
        let len = bytes.len() as u32;
        while self.connection < len || (stream == REPLY && self.reply < len) {
            let (kind, stream, body) = read_frame(self.socket).await?;
            if kind == 8 {
                match stream {
                    0 => self.connection += increment(&body)?,
                    REPLY => self.reply += increment(&body)?,
                    _ => {}
                }
            }
        }
        self.connection -= len;
        if stream == REPLY {
            self.reply -= len;
        }
        write_frame(self.socket, 0, flags, stream, bytes)
            .await
            .map_err(Error::from_read)
    }
}

async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<(u8, u32, Vec<u8>), Error> {
    let mut header = [0; 9];
    reader
        .read_exact(&mut header)
        .await
        .map_err(Error::from_read)?;
    let size = frame_length(&header);
    if size > FRAME_BYTES {
        return Err(Error::InvalidReply);
    }
    let mut body = vec![0; size];
    reader
        .read_exact(&mut body)
        .await
        .map_err(Error::from_read)?;
    Ok((header[3], frame_stream(&header), body))
}

fn increment(bytes: &[u8]) -> Result<u32, Error> {
    let bytes = bytes.try_into().map_err(|_| Error::InvalidReply)?;
    Ok(u32::from_be_bytes(bytes))
}

// Encode a small reply with a gap for the payload. Lengths include the gap,
// so generating a large synthetic response never allocates the whole message.
fn envelope(method: Method, size: u64) -> (Vec<u8>, Vec<u8>) {
    let mut wire = WRAPPER_MAGIC.to_le_bytes().to_vec();
    wire.extend_from_slice(&0x0002_0101u32.to_le_bytes());
    wire.extend_from_slice(&0u64.to_le_bytes());
    wire.extend_from_slice(&1u64.to_le_bytes());
    wire.extend_from_slice(&0x42133742u32.to_le_bytes());
    wire.extend_from_slice(&5u32.to_le_bytes());
    let padded = size.next_multiple_of(4) as u32;
    let gap = container(&mut wire, 0xf000, 2, padded, |wire| {
        let gap = match method {
            Method::Threshold => {
                key(wire, "pasteboard");
                container(wire, 0xf000, 1, padded, |wire| {
                    key(wire, "items");
                    container(wire, 0xe000, 2, padded, |wire| {
                        container(wire, 0xf000, 0, 0, |_| 0);
                        container(wire, 0xf000, 1, padded, |wire| {
                            key(wire, "data");
                            container(wire, 0xf000, 1, padded, |wire| {
                                key(wire, SAMPLE_UTI);
                                container(wire, 0xf000, 1, padded, |wire| data(wire, size))
                            })
                        })
                    })
                })
            }
            Method::Resolve => data(wire, size),
        };
        key(wire, "command");
        let command = match method {
            Method::Threshold => "PULL_REPLY",
            Method::Resolve => "DATA",
        };
        let encoded = XPCObject::String(command.to_owned())
            .encode()
            .expect("synthetic XPC string is encodable");
        wire.extend_from_slice(&encoded[8..]);
        gap
    });
    let body = (wire.len() - 24) as u64 + u64::from(padded);
    wire[8..16].copy_from_slice(&body.to_le_bytes());
    let trailing = wire.split_off(gap);
    let mut suffix = vec![0; padded as usize - size as usize];
    suffix.extend_from_slice(&trailing);
    (wire, suffix)
}

fn container(
    wire: &mut Vec<u8>,
    tag: u32,
    entries: u32,
    missing: u32,
    contents: impl FnOnce(&mut Vec<u8>) -> usize,
) -> usize {
    wire.extend_from_slice(&tag.to_le_bytes());
    let length_field = wire.len();
    wire.extend_from_slice(&[0; 4]);
    wire.extend_from_slice(&entries.to_le_bytes());
    let gap = contents(wire);
    let length = (wire.len() - length_field - 4) as u32 + missing;
    wire[length_field..length_field + 4].copy_from_slice(&length.to_le_bytes());
    gap
}

fn key(wire: &mut Vec<u8>, key: &str) {
    wire.extend_from_slice(key.as_bytes());
    wire.resize(wire.len() + 4 - key.len() % 4, 0);
}

fn data(wire: &mut Vec<u8>, size: u64) -> usize {
    key(wire, "data");
    wire.extend_from_slice(&0x8000u32.to_le_bytes());
    wire.extend_from_slice(&(size as u32).to_le_bytes());
    wire.len()
}

#[cfg(test)]
mod tests;
