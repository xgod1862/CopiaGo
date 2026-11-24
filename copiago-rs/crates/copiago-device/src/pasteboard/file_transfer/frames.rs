use std::ops::Range;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::FileTransferError as Error;

pub(super) const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";
pub(super) const ROOT: u32 = 1;
pub(super) const REPLY: u32 = 3;
pub(super) const FRAME_BYTES: usize = 16 * 1024;
pub(super) const WRAPPER_MAGIC: u32 = 0x29b00b92;
const WINDOW_BATCH: u32 = 64 * 1024;
const END_STREAM: u8 = 1;
const PADDED: u8 = 8;

pub(super) struct ReplyFrames<'a, Stream> {
    stream: &'a mut Stream,
    buffer: Box<[u8; FRAME_BYTES]>,
    chunk: Range<usize>,
    window_credit: [u32; 2],
    reply_ended: bool,
}

impl<'a, Stream: AsyncRead + AsyncWrite + Unpin> ReplyFrames<'a, Stream> {
    pub(super) fn new(stream: &'a mut Stream) -> Self {
        Self {
            stream,
            buffer: Box::new([0; FRAME_BYTES]),
            chunk: 0..0,
            window_credit: [0; 2],
            reply_ended: false,
        }
    }

    pub(super) async fn wrapper_size(&mut self, max: u64) -> Result<u64, Error> {
        let mut wrapper = [0; 24];
        self.read(&mut wrapper).await?;
        if u32::from_le_bytes(wrapper[..4].try_into().unwrap()) != WRAPPER_MAGIC {
            return Err(Error::InvalidReply);
        }
        let size = u64::from_le_bytes(wrapper[8..16].try_into().unwrap());
        if size > max {
            return Err(Error::TooLarge);
        }
        Ok(size)
    }

    pub(super) async fn read(&mut self, mut out: &mut [u8]) -> Result<(), Error> {
        while !out.is_empty() {
            let chunk = self.take(out.len() as u64).await?;
            let (prefix, rest) = out.split_at_mut(chunk.len());
            prefix.copy_from_slice(chunk);
            out = rest;
        }
        Ok(())
    }

    pub(super) async fn skip(&mut self, mut count: u64) -> Result<(), Error> {
        while count != 0 {
            count -= self.take(count).await?.len() as u64;
        }
        Ok(())
    }

    pub(super) async fn copy(
        &mut self,
        mut count: u64,
        out: &mut (impl AsyncWrite + Unpin),
    ) -> Result<(), Error> {
        while count != 0 {
            let chunk = self.take(count).await?;
            out.write_all(chunk).await.map_err(|_| Error::Destination)?;
            count -= chunk.len() as u64;
        }
        Ok(())
    }

    async fn take(&mut self, count: u64) -> Result<&[u8], Error> {
        while self.chunk.is_empty() {
            if self.reply_ended {
                return Err(Error::Truncated);
            }
            self.replenish().await?;
            self.pump().await?;
        }
        let end = self.chunk.start + (self.chunk.len() as u64).min(count) as usize;
        let start = std::mem::replace(&mut self.chunk.start, end);
        Ok(&self.buffer[start..end])
    }

    async fn pump(&mut self) -> Result<(), Error> {
        let mut header = [0; 9];
        self.stream
            .read_exact(&mut header)
            .await
            .map_err(Error::from_read)?;
        let len = frame_length(&header);
        if len > self.buffer.len() {
            return Err(Error::InvalidReply);
        }
        self.stream
            .read_exact(&mut self.buffer[..len])
            .await
            .map_err(Error::from_read)?;
        let stream = frame_stream(&header);
        let flags = header[4];
        match header[3] {
            0 => self.data(stream, flags, len)?,
            1 if stream == REPLY && flags & END_STREAM != 0 => self.reply_ended = true,
            3 if stream == ROOT || stream == REPLY => return Err(Error::Truncated),
            4 if stream != 0 || !len.is_multiple_of(6) || (flags & 1 != 0 && len != 0) => {
                return Err(Error::InvalidReply);
            }
            4 if flags & 1 == 0 => {
                write_frame(self.stream, 4, 1, 0, &[])
                    .await
                    .map_err(Error::from_read)?;
                self.stream.flush().await.map_err(Error::from_read)?;
            }
            6 if stream != 0 || len != 8 => return Err(Error::InvalidReply),
            6 if flags & 1 == 0 => {
                write_frame(self.stream, 6, 1, 0, &self.buffer[..len])
                    .await
                    .map_err(Error::from_read)?;
                self.stream.flush().await.map_err(Error::from_read)?;
            }
            7 => return Err(Error::Truncated),
            // The connection is receive-only after the request. Other control
            // frames, including peer WINDOW_UPDATEs, do not affect the reader.
            _ => {}
        }
        Ok(())
    }

    fn data(&mut self, stream: u32, flags: u8, len: usize) -> Result<(), Error> {
        let index = match stream {
            ROOT => 0,
            REPLY => 1,
            _ => return Err(Error::InvalidReply),
        };
        let chunk = if flags & PADDED == 0 {
            0..len
        } else {
            let end = len
                .checked_sub(usize::from(self.buffer[0]))
                .ok_or(Error::InvalidReply)?;
            if end == 0 {
                return Err(Error::InvalidReply);
            }
            1..end
        };
        // RFC 9113 counts the entire DATA payload, including its padding, against
        // both the connection and stream windows. Acknowledge consumed data in
        // batches well below idevice's negotiated 1 MiB receive window.
        self.window_credit[index] += len as u32;
        if stream == REPLY {
            self.chunk = chunk;
            self.reply_ended = flags & END_STREAM != 0;
        }
        Ok(())
    }

    async fn replenish(&mut self) -> Result<(), Error> {
        let connection = self.window_credit.iter().sum::<u32>();
        if connection < WINDOW_BATCH {
            return Ok(());
        }
        write_frame(self.stream, 8, 0, 0, &connection.to_be_bytes())
            .await
            .map_err(Error::from_read)?;
        for (stream, credit) in [ROOT, REPLY]
            .into_iter()
            .zip(std::mem::take(&mut self.window_credit))
        {
            if credit != 0 {
                write_frame(self.stream, 8, 0, stream, &credit.to_be_bytes())
                    .await
                    .map_err(Error::from_read)?;
            }
        }
        self.stream.flush().await.map_err(Error::from_read)
    }
}

pub(super) fn frame_length(header: &[u8; 9]) -> usize {
    u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize
}

pub(super) fn frame_stream(header: &[u8; 9]) -> u32 {
    u32::from_be_bytes(header[5..].try_into().unwrap()) & 0x7fff_ffff
}

pub(super) async fn write_frame(
    stream: &mut (impl AsyncWrite + Unpin),
    kind: u8,
    flags: u8,
    id: u32,
    payload: &[u8],
) -> std::io::Result<()> {
    let mut header = [0; 9];
    header[..3].copy_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
    header[3] = kind;
    header[4] = flags;
    header[5..].copy_from_slice(&id.to_be_bytes());
    stream.write_all(&header).await?;
    stream.write_all(payload).await
}
