//! USB discovery and the CoreDevice tunnel.

use std::fmt;
use std::fs;
use std::future::pending;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::Duration;

use copiago_core::{dpapi, i18n::Language, paths::DEVICE_DATA_DIR};
use futures_util::StreamExt;
use idevice::{
    IdeviceError, IdeviceService,
    core_device_proxy::CoreDeviceProxy,
    lockdown::LockdownClient,
    pairing_file::PairingFile,
    provider::{IdeviceProvider, UsbmuxdProvider},
    rsd::RsdHandshake,
    tcp::handle::AdapterHandle,
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdDevice, UsbmuxdListenEvent},
};
use tokio::{
    sync::mpsc,
    time::{sleep, timeout},
};
use tracing::{Dispatch, instrument::WithSubscriber};
use zeroize::{Zeroize, Zeroizing};

use crate::{
    DeviceCommand, DeviceEvent, PasteboardError,
    pasteboard::{PASTEBOARD_SERVICE, PasteboardState, WatchOptions, watch_rsd},
    wifi::PairedDevice,
};

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const USBMUX_PAIR_RECORD_TIMEOUT: Duration = Duration::from_secs(3);
const LISTEN_RETRY: Duration = Duration::from_secs(4);

mod pairing;
pub(crate) use pairing::{PairingError, pair_over_usb};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PairingRecordSource {
    Usbmuxd,
    LocalFile,
}

/// Aggregate service information from the USB tunnel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UsbProbe {
    pub service_count: usize,
    pub pasteboard_advertised: bool,
    pub pairing_record_source: PairingRecordSource,
}

/// USB probe failures with device and connection details omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbProbeError {
    Usbmuxd,
    NoUsbDevice,
    MultipleUsbDevices(usize),
    PairingRecordUnavailable,
    TrustRequired,
    DeviceLocked,
    DeveloperModeRequired,
    Lockdown,
    CoreDeviceProxy,
    Rsd,
    Timeout,
}

impl fmt::Display for UsbProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Usbmuxd => "failed to list devices through usbmuxd",
            Self::NoUsbDevice => "no USB device found",
            Self::MultipleUsbDevices(count) => {
                return write!(
                    formatter,
                    "Found {count} USB devices. Connect only one iPhone."
                );
            }
            Self::PairingRecordUnavailable => "no readable lockdown pairing record",
            Self::TrustRequired => "iPhone rejected the lockdown pairing record",
            Self::DeviceLocked => "iPhone is locked",
            Self::DeveloperModeRequired => "iPhone requires Developer Mode",
            Self::Lockdown => "failed to open a lockdown session",
            Self::CoreDeviceProxy => "failed to open the CoreDeviceProxy tunnel",
            Self::Rsd => "failed to discover tunnel services through RSD",
            Self::Timeout => "USB probe timed out",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for UsbProbeError {}

/// Failures from opening a USB tunnel or watching the remote clipboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsbWatchError {
    Usb(UsbProbeError),
    Pasteboard(PasteboardError),
}

impl fmt::Display for UsbWatchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usb(error) => error.fmt(formatter),
            Self::Pasteboard(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for UsbWatchError {}

/// Lists USB devices and reads the advertised services of one already-paired iPhone.
/// Pass the CopiaGo data root containing the device-data directory.
/// Run on a Tokio runtime with I/O and time enabled.
pub async fn probe_usb(data_dir: &Path) -> Result<UsbProbe, UsbProbeError> {
    // Upstream debug tracing can include entire plists, including pairing records.
    timeout(
        PROBE_TIMEOUT,
        probe_on_addr(&UsbmuxdAddr::default(), data_dir),
    )
    .with_subscriber(Dispatch::none())
    .await
    .map_err(|_| UsbProbeError::Timeout)?
}

async fn probe_on_addr(addr: &UsbmuxdAddr, data_dir: &Path) -> Result<UsbProbe, UsbProbeError> {
    let (mut tunnel, pairing_record_source) = open_single_usb_tunnel(addr, data_dir).await?;
    let summary = UsbProbe {
        service_count: tunnel.rsd.services.len(),
        pasteboard_advertised: tunnel.rsd.services.contains_key(PASTEBOARD_SERVICE),
        pairing_record_source,
    };
    let _ = tunnel.adapter.close().await;
    Ok(summary)
}

/// Watch clipboard changes from a single already-paired USB iPhone.
/// Run on a Tokio runtime with I/O and time enabled. Return
/// [`ControlFlow::Break`] from the callback to close the watch.
pub async fn watch_usb(
    data_dir: &Path,
    language: Language,
    emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<(), UsbWatchError> {
    run_usb(data_dir, language, None, emit).await.map(|_| ())
}

/// Synchronize with a single already-paired USB iPhone.
///
/// Handles `DeviceCommand::Push` and `DeviceCommand::FetchOffer` while connected.
/// Returns any other command to the caller and closes the session.
/// A closed command channel or a callback break ends the session with `None`.
pub async fn sync_usb(
    data_dir: &Path,
    language: Language,
    commands: &mut mpsc::Receiver<DeviceCommand>,
    emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<Option<DeviceCommand>, UsbWatchError> {
    run_usb(data_dir, language, Some(commands), emit).await
}

async fn run_usb(
    data_dir: &Path,
    language: Language,
    mut commands: Option<&mut mpsc::Receiver<DeviceCommand>>,
    emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<Option<DeviceCommand>, UsbWatchError> {
    let (mut tunnel, _) = timeout(
        PROBE_TIMEOUT,
        open_single_usb_tunnel(&UsbmuxdAddr::default(), data_dir),
    )
    .with_subscriber(Dispatch::none())
    .await
    .unwrap_or(Err(UsbProbeError::Timeout))
    .map_err(UsbWatchError::Usb)?;
    let result = watch_rsd(
        &mut tunnel.adapter,
        &mut tunnel.rsd,
        WatchOptions {
            data_dir,
            wireless: false,
            language,
            follow_saved_language: false,
        },
        &mut PasteboardState::default(),
        async || match &mut commands {
            Some(receiver) => receiver.recv().await,
            None => pending().await,
        },
        emit,
    )
    .await;
    let _ = tunnel.adapter.close().await;
    result.map_err(UsbWatchError::Pasteboard)
}

async fn open_single_usb_tunnel(
    addr: &UsbmuxdAddr,
    data_dir: &Path,
) -> Result<(RsdTunnel, PairingRecordSource), UsbProbeError> {
    let provider = single_usb_provider(addr).await?;
    open_usb_tunnel(&provider, data_dir).await
}

async fn single_usb_provider(addr: &UsbmuxdAddr) -> Result<UsbmuxdProvider, UsbProbeError> {
    let devices = list_usb_devices(addr).await?;
    match devices.as_slice() {
        [device] => Ok(device.to_provider(addr.clone(), "CopiaGo")),
        [] => Err(UsbProbeError::NoUsbDevice),
        _ => Err(UsbProbeError::MultipleUsbDevices(devices.len())),
    }
}

pub(crate) async fn paired_usb_id(
    addr: &UsbmuxdAddr,
    device: &PairedDevice,
) -> Result<Option<u32>, UsbProbeError> {
    // A fresh snapshot includes any USB attaches since the Wi-Fi attempt began.
    let attached = timeout(PROBE_TIMEOUT, list_usb_devices(addr))
        .with_subscriber(Dispatch::none())
        .await
        .map_err(|_| UsbProbeError::Timeout)??;
    Ok(attached
        .into_iter()
        .find(|usb| usb.udid == device.serial)
        .map(|usb| usb.device_id))
}

pub(crate) async fn open_paired_usb_tunnel(
    addr: &UsbmuxdAddr,
    data_dir: &Path,
    paired: &PairedDevice,
    id: u32,
) -> Result<RsdTunnel, UsbProbeError> {
    let device = UsbmuxdDevice {
        connection_type: Connection::Usb,
        udid: paired.serial.clone(),
        device_id: id,
    };
    let provider = device.to_provider(addr.clone(), "CopiaGo");
    let (tunnel, _) = timeout(PROBE_TIMEOUT, open_usb_tunnel(&provider, data_dir))
        .with_subscriber(Dispatch::none())
        .await
        .map_err(|_| UsbProbeError::Timeout)??;
    if !paired.matches_rsd(&tunnel.rsd) {
        return Err(UsbProbeError::Rsd);
    }
    Ok(tunnel)
}

/// UsbMux attach eligibility is read at fallback time. A detach can also end
/// the corresponding active USB session before its next pasteboard poll.
pub(crate) async fn listen_usb_detach(addr: &UsbmuxdAddr, detached: mpsc::Sender<u32>) {
    loop {
        let _ = listen_once(addr, &detached)
            .with_subscriber(Dispatch::none())
            .await;
        sleep(LISTEN_RETRY).await;
    }
}

async fn listen_once(
    addr: &UsbmuxdAddr,
    detached: &mpsc::Sender<u32>,
) -> Result<(), UsbProbeError> {
    let mut mux = timeout(PROBE_TIMEOUT, addr.connect(0))
        .await
        .map_err(|_| UsbProbeError::Timeout)?
        .map_err(|_| UsbProbeError::Usbmuxd)?;
    let mut stream = timeout(PROBE_TIMEOUT, mux.listen())
        .await
        .map_err(|_| UsbProbeError::Timeout)?
        .map_err(|_| UsbProbeError::Usbmuxd)?;
    while let Some(event) = stream.next().await {
        if let UsbmuxdListenEvent::Disconnected(id) = event.map_err(|_| UsbProbeError::Usbmuxd)?
            && detached.send(id).await.is_err()
        {
            break;
        }
    }
    Ok(())
}

async fn list_usb_devices(addr: &UsbmuxdAddr) -> Result<Vec<UsbmuxdDevice>, UsbProbeError> {
    let mut mux = addr.connect(0).await.map_err(|_| UsbProbeError::Usbmuxd)?;
    Ok(mux
        .get_devices()
        .await
        .map_err(|_| UsbProbeError::Usbmuxd)?
        .into_iter()
        .filter(|device| device.connection_type == Connection::Usb)
        .collect())
}

struct LockdownRecord(PairingFile);

impl Drop for LockdownRecord {
    fn drop(&mut self) {
        self.0.host_private_key.zeroize();
        self.0.root_private_key.zeroize();
        self.0.escrow_bag.zeroize();
    }
}

async fn pairing_record(
    provider: &UsbmuxdProvider,
    data_dir: &Path,
) -> Result<(LockdownRecord, PairingRecordSource), UsbProbeError> {
    if let Ok(Ok(record)) = timeout(USBMUX_PAIR_RECORD_TIMEOUT, provider.get_pairing_file()).await {
        return Ok((LockdownRecord(record), PairingRecordSource::Usbmuxd));
    }

    let record = local_pairing_record(data_dir, &provider.udid)?;
    Ok((record, PairingRecordSource::LocalFile))
}

fn local_pairing_record(data_dir: &Path, udid: &str) -> Result<LockdownRecord, UsbProbeError> {
    let path = local_record_path(data_dir, udid).ok_or(UsbProbeError::PairingRecordUnavailable)?;
    let file = Zeroizing::new(fs::read(path).map_err(|_| UsbProbeError::PairingRecordUnavailable)?);
    let record = if file.starts_with(dpapi::MAGIC) {
        let plaintext = Zeroizing::new(
            dpapi::unprotect(&file).map_err(|_| UsbProbeError::PairingRecordUnavailable)?,
        );
        PairingFile::from_bytes(&plaintext)
    } else {
        PairingFile::from_bytes(&file)
    }
    .map_err(|_| UsbProbeError::PairingRecordUnavailable)?;
    Ok(LockdownRecord(record))
}

fn local_record_path(data_dir: &Path, udid: &str) -> Option<PathBuf> {
    // Legacy and modern UDIDs use hex digits and may contain hyphens.
    if udid.is_empty()
        || !udid
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
    {
        return None;
    }
    Some(data_dir.join(DEVICE_DATA_DIR).join(format!("{udid}.plist")))
}

async fn trusted_lockdown(
    provider: &UsbmuxdProvider,
    data_dir: &Path,
) -> Result<(LockdownClient, LockdownRecord, bool, PairingRecordSource), UsbProbeError> {
    let (mut record, mut source) = pairing_record(provider, data_dir).await?;
    let mut lockdown = LockdownClient::connect(provider)
        .await
        .map_err(|_| UsbProbeError::Lockdown)?;
    let mut session = lockdown.start_session(&record.0).await;
    if matches!(session, Err(IdeviceError::InvalidHostID)) && source == PairingRecordSource::Usbmuxd
    {
        record = local_pairing_record(data_dir, &provider.udid)
            .map_err(|_| UsbProbeError::TrustRequired)?;
        source = PairingRecordSource::LocalFile;
        lockdown = LockdownClient::connect(provider)
            .await
            .map_err(|_| UsbProbeError::Lockdown)?;
        session = lockdown.start_session(&record.0).await;
    }
    let legacy_tls = session.map_err(|error| device_error(error, UsbProbeError::Lockdown))?;
    Ok((lockdown, record, legacy_tls, source))
}

async fn open_usb_tunnel(
    provider: &UsbmuxdProvider,
    data_dir: &Path,
) -> Result<(RsdTunnel, PairingRecordSource), UsbProbeError> {
    let (mut lockdown, record, legacy_tls, source) = trusted_lockdown(provider, data_dir).await?;
    let (port, use_tls) = lockdown
        .start_service(CoreDeviceProxy::service_name())
        .await
        .map_err(|error| device_error(error, UsbProbeError::CoreDeviceProxy))?;
    let mut connection = provider
        .connect(port)
        .await
        .map_err(|_| UsbProbeError::CoreDeviceProxy)?;
    if use_tls {
        connection
            .start_session(&record.0, legacy_tls)
            .await
            .map_err(|_| UsbProbeError::CoreDeviceProxy)?;
    }
    drop(record);

    let proxy = CoreDeviceProxy::new(connection)
        .await
        .map_err(|_| UsbProbeError::CoreDeviceProxy)?;
    drop(lockdown);
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy
        .create_software_tunnel()
        .map_err(|_| UsbProbeError::CoreDeviceProxy)?
        .to_async_handle();
    let stream = adapter
        .connect(rsd_port)
        .await
        .map_err(|_| UsbProbeError::Rsd)?;
    let rsd = RsdHandshake::new(stream)
        .await
        .map_err(|_| UsbProbeError::Rsd)?;
    Ok((RsdTunnel { adapter, rsd }, source))
}

pub(crate) struct RsdTunnel {
    pub(crate) adapter: AdapterHandle,
    pub(crate) rsd: RsdHandshake,
}

fn device_error(error: IdeviceError, step: UsbProbeError) -> UsbProbeError {
    match error {
        IdeviceError::InvalidHostID => UsbProbeError::TrustRequired,
        IdeviceError::DeviceLocked | IdeviceError::PasswordProtected => UsbProbeError::DeviceLocked,
        IdeviceError::DeveloperModeNotEnabled => UsbProbeError::DeveloperModeRequired,
        _ => step,
    }
}

#[cfg(test)]
mod tests;
