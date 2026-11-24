use std::{
    fs::File,
    io::{self, Write},
    path::Path,
    pin::Pin,
    task::{Context, Poll},
};

use copiago_core::{
    clip::FileOffer,
    files::{MAX_FILE_BYTES, ReceivedFile, StagedFile},
};
use tokio::io::AsyncWrite;

use super::file_transfer::FileTransferError;

pub(super) async fn receive(
    data_dir: &Path,
    offer: FileOffer,
    read: impl AsyncFnOnce(&FileOffer, &mut FileWriter<'_>) -> Result<u64, FileTransferError>,
) -> Result<ReceivedFile, FileTransferError> {
    if offer.size > MAX_FILE_BYTES
        || offer
            .data
            .as_ref()
            .is_some_and(|data| data.len() as u64 > MAX_FILE_BYTES)
    {
        return Err(FileTransferError::TooLarge);
    }
    let mut file = StagedFile::new(data_dir, offer.name.clone())
        .map_err(|_| FileTransferError::Destination)?;
    let bytes = read(&offer, &mut FileWriter(file.writer())).await?;
    // The file stays owned by the task if cancellation occurs during sync. The
    // dropped result then cleans staging without ever publishing the file.
    let synced = tokio::task::spawn_blocking(move || file.sync(bytes))
        .await
        .map_err(|_| FileTransferError::Destination)?
        .map_err(|_| FileTransferError::Destination)?;
    synced.publish().map_err(|_| FileTransferError::Destination)
}

// Incoming stream writes are bounded by the frame buffer. Keeping this handle
// on the device task also lets cancellation close it before removing staging.
pub(super) struct FileWriter<'a>(&'a mut File);

impl AsyncWrite for FileWriter<'_> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        Poll::Ready(self.0.write(bytes))
    }

    fn poll_flush(mut self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(self.0.flush())
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.poll_flush(cx)
    }
}

#[cfg(test)]
mod tests;
