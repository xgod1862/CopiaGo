use std::fmt;
use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::oneshot;
use tokio::time::timeout;

use super::WifiProbeError;

const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) struct Lifetime {
    stop: oneshot::Sender<()>,
    closed: oneshot::Receiver<()>,
}

impl Lifetime {
    pub(super) async fn close(self) -> Result<(), WifiProbeError> {
        drop(self.stop);
        // The sender is dropped after jktcp releases both sockets.
        let _ = timeout(CLOSE_TIMEOUT, self.closed)
            .await
            .map_err(|_| WifiProbeError::CloseTimeout)?;
        Ok(())
    }
}

/// Owned by the jktcp task. Drop the data and RP control sockets in that order.
pub(super) struct Transport<S, C> {
    data: S,
    _control: C,
    _closed: oneshot::Sender<()>,
    stop: Option<oneshot::Receiver<()>>,
}

impl<S, C> Transport<S, C> {
    pub(super) fn new(data: S, control: C) -> (Self, Lifetime) {
        let (stop, stopped) = oneshot::channel();
        let (ended, closed) = oneshot::channel();
        let transport = Self {
            data,
            _control: control,
            _closed: ended,
            stop: Some(stopped),
        };
        (transport, Lifetime { stop, closed })
    }

    fn check_open(&mut self, cx: &mut Context<'_>) -> io::Result<()> {
        // Wake pending adapter I/O even when jktcp is inside a connection attempt.
        let open = self
            .stop
            .as_mut()
            .is_some_and(|stop| Pin::new(stop).poll(cx).is_pending());
        if open {
            Ok(())
        } else {
            self.stop = None;
            Err(io::ErrorKind::ConnectionAborted.into())
        }
    }
}

impl<S, C> fmt::Debug for Transport<S, C> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("Transport").finish_non_exhaustive()
    }
}

impl<S: AsyncRead + Unpin, C: Unpin> AsyncRead for Transport<S, C> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.check_open(cx)?;
        Pin::new(&mut this.data).poll_read(cx, buffer)
    }
}

impl<S: AsyncWrite + Unpin, C: Unpin> AsyncWrite for Transport<S, C> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        this.check_open(cx)?;
        Pin::new(&mut this.data).poll_write(cx, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.check_open(cx)?;
        Pin::new(&mut this.data).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        this.check_open(cx)?;
        Pin::new(&mut this.data).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests;
