//! Receive pasteboard representations through bounded RemoteXPC frames.

use std::{fmt, io, time::Duration};

pub(crate) use copiago_core::files::MAX_FILE_BYTES;
use copiago_core::{
    clip::{FileOffer, PasteboardChange},
    i18n::Language,
};
use idevice::{ReadWrite, RemoteXpcClient, tcp::handle::AdapterHandle, xpc::XPCObject};
use tokio::{
    io::{self as async_io, AsyncWrite, AsyncWriteExt},
    time::timeout,
};
use tracing::{Dispatch, instrument::WithSubscriber};

use crate::FileProgress;

use super::{
    METADATA_TIMEOUT, PullPolicy,
    content::{DESCRIPTION_BYTES, Snapshot},
    file_offer,
    rsd::{GENERAL_PASTEBOARD, pull_request},
};

mod frames;
mod reply;
pub(crate) mod simulation;

use frames::ReplyFrames;
use reply::{Destination, FileOutput};

const MIB: u64 = 1024 * 1024;
const MAX_METADATA_BODY: u64 = 2 * MIB;

/// Pasteboard request used to fetch the file bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// PULL with thresholdData, as in the Python app.
    Threshold,
    /// Diagnostic comparison using RESOLVE for one item and UTI.
    Resolve,
}

/// File transfer failures with device and clipboard details omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileTransferError {
    Disconnected,
    Busy,
    Request,
    TimedOut,
    OfferExpired,
    DataUnavailable,
    UnsupportedPayload,
    TooLarge,
    SizeMismatch,
    Truncated,
    InvalidReply,
    Destination,
}

impl fmt::Display for FileTransferError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Disconnected => "iPhone is not connected",
            Self::Busy => "another file transfer is pending",
            Self::Request => "pasteboard file request failed",
            Self::TimedOut => "pasteboard file request timed out",
            Self::OfferExpired => "pasteboard changed since the file was offered",
            Self::DataUnavailable => "file data was not supplied inline",
            Self::UnsupportedPayload => "pasteboard returned a separate file stream descriptor",
            Self::TooLarge => "file or file reply exceeds the transfer limit",
            Self::SizeMismatch => "file byte count differs from the offer",
            Self::Truncated => "connection ended before the complete file reply",
            Self::InvalidReply => "invalid pasteboard file reply",
            Self::Destination => "could not write the file data",
        })
    }
}

impl std::error::Error for FileTransferError {}

impl FileTransferError {
    fn from_read(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::UnexpectedEof
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset => Self::Truncated,
            _ => Self::Request,
        }
    }
}

/// Look for the current Files offer using only the listing and bounded description.
pub(crate) async fn describe_file(
    adapter: &mut AdapterHandle,
    port: u16,
) -> Result<FileOffer, FileTransferError> {
    let listing = listing(adapter, port).await?;
    let uti = file_offer::content_type(&listing)
        .ok_or(FileTransferError::DataUnavailable)?
        .to_owned();
    let described = describe_snapshot(adapter, port).await?;
    if listing.has_different_change(&described) {
        return Err(FileTransferError::OfferExpired);
    }
    Ok(file_offer::describe(described, uti, listing.change, || {
        Language::ZhCn
    }))
}

pub(super) async fn describe_snapshot(
    adapter: &mut AdapterHandle,
    port: u16,
) -> Result<Snapshot, FileTransferError> {
    small_pull(
        adapter,
        port,
        PullPolicy::Threshold(DESCRIPTION_BYTES),
        Duration::from_secs(30),
    )
    .await
}

/// Writes a file representation to an unpublished destination on the current tunnel.
///
/// Call serially with other pasteboard requests. For a promise, this rechecks the
/// offer's change ID before the transfer. A threshold reply with a missing ID
/// requires one more listing before success. Zero size means unknown. The caller
/// owns the destination and must flush, publish on success and discard it on error
/// or cancellation. Bytes may already have been written before a late error.
pub(crate) async fn receive_file(
    adapter: &mut AdapterHandle,
    port: u16,
    offer: &FileOffer,
    method: Method,
    destination: &mut (impl AsyncWrite + Unpin),
    progress: &mut dyn FnMut(FileProgress),
) -> Result<u64, FileTransferError> {
    if offer.size > MAX_FILE_BYTES {
        return Err(FileTransferError::TooLarge);
    }
    if let Some(data) = &offer.data {
        check_size(
            data.len() as u64,
            MAX_FILE_BYTES,
            Some(offer.size).filter(|size| *size != 0),
        )?;
        destination
            .write_all(data)
            .await
            .map_err(|_| FileTransferError::Destination)?;
        return Ok(data.len() as u64);
    }

    let current = listing(adapter, port).await?;
    if offer.change.is_some() && current.change != offer.change {
        return Err(FileTransferError::OfferExpired);
    }
    let item_index = if method == Method::Resolve {
        current
            .file_item_index(&offer.uti)
            .ok_or(FileTransferError::DataUnavailable)?
    } else {
        0
    };
    let expected = Some(offer.size).filter(|size| *size != 0);
    let reply = timeout(transfer_timeout(expected), async {
        let mut stream = adapter
            .connect(port)
            .await
            .map_err(|_| FileTransferError::Request)?;
        receive(
            &mut stream,
            method,
            &offer.uti,
            item_index,
            offer.change.as_ref(),
            FileOutput {
                output: destination,
                expected,
                progress,
            },
        )
        .await
    })
    .with_subscriber(Dispatch::none())
    .await
    .map_err(|_| FileTransferError::TimedOut)??;
    reply
        .confirm_change(method, offer.change.as_ref(), async || {
            Ok(listing(adapter, port).await?.change)
        })
        .await
}

pub(super) async fn listing(
    adapter: &mut AdapterHandle,
    port: u16,
) -> Result<Snapshot, FileTransferError> {
    small_pull(adapter, port, PullPolicy::AllPromised, METADATA_TIMEOUT).await
}

/// Resolve one image representation. The caller bounds the timeout and checks the change ID.
pub(super) async fn resolve_alternate_image(
    adapter: &mut AdapterHandle,
    port: u16,
    item_index: usize,
    uti: &str,
) -> Result<Vec<u8>, FileTransferError> {
    async {
        let mut stream = adapter
            .connect(port)
            .await
            .map_err(|_| FileTransferError::Request)?;
        resolve_image_on_stream(&mut stream, item_index, uti).await
    }
    .with_subscriber(Dispatch::none())
    .await
}

async fn resolve_image_on_stream(
    stream: &mut impl ReadWrite,
    item_index: usize,
    uti: &str,
) -> Result<Vec<u8>, FileTransferError> {
    let method = Method::Resolve;
    send(stream, fetch_request(method, uti, item_index, None)).await?;
    let mut bytes = Vec::new();
    receive_reply(
        stream,
        method,
        uti,
        None,
        Destination::<async_io::Sink>::Image(&mut bytes),
    )
    .await?;
    Ok(bytes)
}

async fn small_pull(
    adapter: &mut AdapterHandle,
    port: u16,
    policy: PullPolicy,
    wait: Duration,
) -> Result<Snapshot, FileTransferError> {
    timeout(wait, async {
        let mut stream = adapter
            .connect(port)
            .await
            .map_err(|_| FileTransferError::Request)?;
        send(&mut stream, pull_request(policy)).await?;
        let mut frames = ReplyFrames::new(&mut stream);
        loop {
            let size = frames.wrapper_size(MAX_METADATA_BODY).await?;
            if size == 0 {
                continue;
            }
            let mut body = vec![0; size as usize];
            // Decode the small XPC body with idevice, keeping the same plist
            // representation and change-ID handling as the normal watcher.
            frames.read(&mut body).await?;
            let value = reply::decode_small(&body)?;
            let fields = value
                .as_dictionary()
                .ok_or(FileTransferError::InvalidReply)?;
            if fields.is_empty() {
                continue;
            }
            if fields.get("command").and_then(XPCObject::as_string) != Some("PULL_REPLY") {
                return Err(FileTransferError::InvalidReply);
            }
            return Snapshot::from_reply(value.to_plist())
                .map_err(|_| FileTransferError::InvalidReply);
        }
    })
    .with_subscriber(Dispatch::none())
    .await
    .map_err(|_| FileTransferError::TimedOut)?
}

fn transfer_timeout(expected: Option<u64>) -> Duration {
    Duration::from_secs_f64(
        (60.0 + (expected.unwrap_or(MAX_FILE_BYTES) + 1) as f64 / MIB as f64).min(900.0),
    )
}

fn fetch_request(method: Method, uti: &str, item_index: usize, expected: Option<u64>) -> XPCObject {
    match method {
        Method::Threshold => pull_request(PullPolicy::Threshold(
            (expected.unwrap_or(MAX_FILE_BYTES) + 1) as i64,
        )),
        Method::Resolve => idevice::xpc!({
            "command": "RESOLVE",
            "pasteboardName": GENERAL_PASTEBOARD,
            "itemIndex": item_index as i64,
            "type": uti,
        }),
    }
}

async fn send(stream: &mut impl ReadWrite, request: XPCObject) -> Result<(), FileTransferError> {
    // idevice 0.1.68 writes the handshake and these small requests without
    // reading. Give it only the write half so it cannot consume a reply frame.
    let mut client = RemoteXpcClient::new(async_io::join(async_io::empty(), stream))
        .await
        .map_err(|_| FileTransferError::Request)?;
    client
        .do_handshake()
        .await
        .map_err(|_| FileTransferError::Request)?;
    client
        .send_object(request, true)
        .await
        .map_err(|_| FileTransferError::Request)
}

#[derive(Debug, PartialEq, Eq)]
struct ReceivedFile {
    bytes: u64,
    change_verified: bool,
}

impl ReceivedFile {
    async fn confirm_change(
        self,
        method: Method,
        expected: Option<&PasteboardChange>,
        read_current: impl AsyncFnOnce() -> Result<Option<PasteboardChange>, FileTransferError>,
    ) -> Result<u64, FileTransferError> {
        // Older PULL_REPLY messages may omit the ID. The bytes remain staged
        // until we confirm the pasteboard stayed on the offer's change.
        if let Some(expected) = expected
            && method == Method::Threshold
            && !self.change_verified
            && read_current().await?.as_ref() != Some(expected)
        {
            return Err(FileTransferError::OfferExpired);
        }
        Ok(self.bytes)
    }
}

async fn receive(
    stream: &mut impl ReadWrite,
    method: Method,
    uti: &str,
    item_index: usize,
    change: Option<&PasteboardChange>,
    destination: FileOutput<'_, impl AsyncWrite + Unpin>,
) -> Result<ReceivedFile, FileTransferError> {
    let expected = destination.expected;
    if expected.is_some_and(|size| size > MAX_FILE_BYTES) {
        return Err(FileTransferError::TooLarge);
    }
    if uti.is_empty() || uti.len() >= reply::MAX_KEY_BYTES || uti.as_bytes().contains(&0) {
        return Err(FileTransferError::InvalidReply);
    }
    send(stream, fetch_request(method, uti, item_index, expected)).await?;
    receive_reply(stream, method, uti, change, Destination::File(destination)).await
}

async fn receive_reply(
    stream: &mut impl ReadWrite,
    method: Method,
    uti: &str,
    change: Option<&PasteboardChange>,
    mut destination: Destination<'_, impl AsyncWrite + Unpin>,
) -> Result<ReceivedFile, FileTransferError> {
    let max_body = destination.max_payload() + MAX_METADATA_BODY;
    let mut frames = ReplyFrames::new(stream);
    loop {
        let size = frames.wrapper_size(max_body).await?;
        if size != 0
            && let Some(received) =
                reply::scan(&mut frames, size, method, uti, change, &mut destination).await?
        {
            return Ok(received);
        }
    }
}

fn check_size(size: u64, limit: u64, expected: Option<u64>) -> Result<(), FileTransferError> {
    if size > limit {
        return Err(FileTransferError::TooLarge);
    }
    if expected.is_some_and(|expected| size != expected) {
        return Err(FileTransferError::SizeMismatch);
    }
    Ok(())
}
