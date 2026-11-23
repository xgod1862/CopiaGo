//! Diagnostics for file streaming on an already-paired iPhone.

use std::{fmt, path::Path, time::Duration};

use idevice::{rsd::RsdHandshake, tcp::handle::AdapterHandle, usbmuxd::UsbmuxdAddr};
use tokio::{
    io::AsyncWrite,
    time::{Instant, timeout},
};

use crate::{
    UsbProbeError, WifiProbeError,
    pasteboard::{
        PASTEBOARD_SERVICE,
        file_transfer::{MAX_FILE_BYTES, describe_file, receive_file, simulation},
    },
    usb::{open_paired_usb_tunnel, paired_usb_id},
    wifi::{WifiProbeOptions, find_paired_device, open_wifi_tunnel},
};

pub use crate::pasteboard::file_transfer::{FileTransferError, Method};

/// A physical link, selected explicitly so the probe never measures a fallback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Link {
    Wifi,
    Usb,
}

/// Timing and byte count for one file request. The caller measures disk flush.
#[derive(Clone, Copy, Debug)]
pub struct TransferReport {
    pub bytes: u64,
    pub prepare: Duration,
    pub transfer: Duration,
}

#[derive(Debug)]
pub enum BenchmarkError {
    PairingRecord(WifiProbeError),
    Wifi(WifiProbeError),
    Usb(UsbProbeError),
    NoMatchingUsb,
    ServiceMissing,
    SampleMismatch,
    Transfer(FileTransferError),
}

impl fmt::Display for BenchmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PairingRecord(error) | Self::Wifi(error) => error.fmt(formatter),
            Self::Usb(error) => error.fmt(formatter),
            Self::NoMatchingUsb => {
                formatter.write_str("the paired iPhone is not connected over USB")
            }
            Self::ServiceMissing => formatter.write_str("pasteboard service is not advertised"),
            Self::SampleMismatch => {
                formatter.write_str("current Files offer does not match the local test sample")
            }
            Self::Transfer(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for BenchmarkError {}

/// Receive a prepared Files sample from one already-paired iPhone.
///
/// The data directory supplies pairing records and is read-only. The expected
/// sample name and byte count must match the current offer before fetching it.
/// The caller owns the unpublished destination and must verify the content.
pub async fn receive(
    data_dir: &Path,
    link: Link,
    method: Method,
    sample_name: &str,
    sample_bytes: u64,
    out: &mut (impl AsyncWrite + Unpin),
) -> Result<TransferReport, BenchmarkError> {
    if sample_name.is_empty() || !(1..=MAX_FILE_BYTES).contains(&sample_bytes) {
        return Err(BenchmarkError::SampleMismatch);
    }
    let started = Instant::now();
    let paired = find_paired_device(data_dir).map_err(BenchmarkError::PairingRecord)?;
    let sample = Sample {
        name: sample_name,
        bytes: sample_bytes,
        method,
        started,
    };
    match link {
        Link::Wifi => {
            let (mut tunnel, _) = open_wifi_tunnel(&paired, WifiProbeOptions::default())
                .await
                .map_err(BenchmarkError::Wifi)?;
            let result = sample.receive(&mut tunnel.adapter, &tunnel.rsd, out).await;
            let closed = tunnel.close().await;
            let report = result?;
            closed.map_err(BenchmarkError::Wifi)?;
            Ok(report)
        }
        Link::Usb => {
            let address = UsbmuxdAddr::default();
            let id = paired_usb_id(&address, &paired)
                .await
                .map_err(BenchmarkError::Usb)?
                .ok_or(BenchmarkError::NoMatchingUsb)?;
            let mut tunnel = open_paired_usb_tunnel(&address, data_dir, &paired, id)
                .await
                .map_err(BenchmarkError::Usb)?;
            let result = sample.receive(&mut tunnel.adapter, &tunnel.rsd, out).await;
            let _ = timeout(Duration::from_secs(5), tunnel.adapter.close()).await;
            result
        }
    }
}

struct Sample<'a> {
    name: &'a str,
    bytes: u64,
    method: Method,
    started: Instant,
}

impl Sample<'_> {
    async fn receive(
        &self,
        adapter: &mut AdapterHandle,
        rsd: &RsdHandshake,
        out: &mut (impl AsyncWrite + Unpin),
    ) -> Result<TransferReport, BenchmarkError> {
        let service = rsd
            .services
            .get(PASTEBOARD_SERVICE)
            .ok_or(BenchmarkError::ServiceMissing)?;
        let offer = describe_file(adapter, service.port)
            .await
            .map_err(BenchmarkError::Transfer)?;
        if offer.name != self.name
            || offer.size != self.bytes
            || offer.change.is_none()
            || offer.data.is_some()
        {
            return Err(BenchmarkError::SampleMismatch);
        }
        let started = Instant::now();
        let bytes = receive_file(adapter, service.port, &offer, self.method, out)
            .await
            .map_err(BenchmarkError::Transfer)?;
        Ok(TransferReport {
            bytes,
            prepare: started.duration_since(self.started),
            transfer: started.elapsed(),
        })
    }
}

/// Receive a simulated XPC reply and return the peer's SHA-256 for verification.
/// Timing includes the simulated peer and does not measure a physical link.
pub async fn simulate(
    method: Method,
    bytes: u64,
    out: &mut (impl AsyncWrite + Unpin),
) -> Result<(TransferReport, [u8; 32]), FileTransferError> {
    if bytes > MAX_FILE_BYTES {
        return Err(FileTransferError::TooLarge);
    }
    let started = Instant::now();
    let (bytes, digest) = timeout(
        Duration::from_secs(120),
        simulation::run(method, bytes, out),
    )
    .await
    .map_err(|_| FileTransferError::TimedOut)??;
    Ok((
        TransferReport {
            bytes,
            prepare: Duration::ZERO,
            transfer: started.elapsed(),
        },
        digest,
    ))
}
