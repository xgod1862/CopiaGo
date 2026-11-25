//! USB discovery and the CoreDevice tunnel.

use std::cell::Cell;
use std::fmt;
use std::fs;
use std::future::pending;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use copiago_core::{diagnostic, dpapi, i18n::Language, paths::DEVICE_DATA_DIR};
use futures_util::StreamExt;
use idevice::{
    Idevice, IdeviceError, IdeviceService,
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
/// Opening the CoreDevice tunnel of a paired phone. The Python release had no limit here
/// and slow ports, hubs and some cables need well over 10 s right after a trust prompt.
const USB_TUNNEL_TIMEOUT: Duration = Duration::from_secs(25);
const USBMUX_PAIR_RECORD_TIMEOUT: Duration = Duration::from_secs(3);
const LISTEN_RETRY: Duration = Duration::from_secs(4);
/// Status when the Apple Mobile Device Service (usbmuxd) cannot be reached or cannot list
/// devices. The host recognizes it to show the service diagnosis and repair controls.
pub const APPLE_SERVICE_UNAVAILABLE: &str =
    "无法连接电脑上的 Apple 设备连接服务（与手机和数据线无关）。";
/// Pairing found no iPhone among the Apple service's USB devices. The host recognizes it
/// to explain the cause from what Windows itself sees on USB.
pub const NO_USB_IPHONE: &str = "未找到 USB 连接的 iPhone。请用数据线连接并解锁后重试";
/// Developer Mode is off and the phone is not on USB, so CopiaGo could not ask it to show
/// the Settings switch. The switch may already be visible: once revealed, iOS keeps it.
pub(crate) const DEVELOPER_MODE_OFF_NO_USB: &str = "iPhone 的开发者模式没有开启。请在 iPhone「设置 → 隐私与安全性 → 开发者模式」中打开并按提示重启，解锁后点「继续连接」。如果「设置」里找不到这个开关，请用数据线连接并解锁 iPhone，再点「继续连接」，CopiaGo 会让它出现。";
/// Developer Mode is off and the request to show the switch failed although the phone is
/// on USB, so asking for a cable would not help.
pub(crate) const DEVELOPER_MENU_RETRY: &str = "iPhone 的开发者模式没有开启，CopiaGo 也没能让「设置」里出现这个开关。请在 iPhone「设置 → 隐私与安全性」底部查找「开发者模式」并打开；如果没有，请保持手机解锁，重新插拔数据线后点「继续连接」。";

mod pairing;
pub(crate) use pairing::{PairingError, pair_over_usb, renew_usb_trust};

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
    DeveloperMenu,
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
            Self::DeveloperMenu => "iPhone did not confirm the Developer Mode menu request",
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
    let attached = list_usb_devices(addr)
        .with_subscriber(Dispatch::none())
        .await?;
    Ok(attached
        .into_iter()
        .find(|usb| usb.udid == device.serial)
        .map(|usb| usb.device_id))
}

pub(crate) fn paired_usb_provider(
    addr: &UsbmuxdAddr,
    paired: &PairedDevice,
    id: u32,
) -> UsbmuxdProvider {
    UsbmuxdDevice {
        connection_type: Connection::Usb,
        udid: paired.serial.clone(),
        device_id: id,
    }
    .to_provider(addr.clone(), "CopiaGo")
}

pub(crate) async fn open_paired_usb_tunnel(
    addr: &UsbmuxdAddr,
    data_dir: &Path,
    paired: &PairedDevice,
    id: u32,
) -> Result<RsdTunnel, UsbProbeError> {
    let provider = paired_usb_provider(addr, paired, id);
    let step = Cell::new(TunnelStep::Lockdown);
    let started = Instant::now();
    let opened = timeout(
        USB_TUNNEL_TIMEOUT,
        open_usb_tunnel_traced(&provider, data_dir, &step),
    )
    .with_subscriber(Dispatch::none())
    .await
    .unwrap_or(Err(UsbProbeError::Timeout));
    let (tunnel, _) = opened.inspect_err(|error| {
        diagnostic::event(format_args!(
            "event=usb_tunnel_failed step={:?} error={error:?} ms={}",
            step.get(),
            started.elapsed().as_millis()
        ));
    })?;
    if !paired.matches_rsd(&tunnel.rsd) {
        diagnostic::event(format_args!("event=usb_tunnel_failed step=identity"));
        return Err(UsbProbeError::Rsd);
    }
    Ok(tunnel)
}

/// Furthest step reached while opening a USB tunnel, for the diagnostic log.
#[derive(Clone, Copy, Debug)]
enum TunnelStep {
    Lockdown,
    CoreDeviceProxy,
    SoftwareTunnel,
    RsdConnect,
    RsdHandshake,
}

/// Logs a library error before it is reduced to a coarse category. Long hexadecimal
/// runs such as identifiers are masked, because users paste the log into public issues.
fn traced(error: impl fmt::Debug, mapped: UsbProbeError) -> UsbProbeError {
    diagnostic::event(format_args!(
        "event=usb_step_error step={mapped:?} detail={}",
        sanitize(&format!("{error:?}"))
    ));
    mapped
}

fn sanitize(detail: &str) -> String {
    let mut output = String::with_capacity(detail.len().min(160));
    let mut run = String::new();
    for character in detail.chars().chain(Some(' ')) {
        if character.is_ascii_hexdigit() || character == '-' {
            run.push(character);
            continue;
        }
        output.push_str(if run.len() >= 12 { "<id>" } else { &run });
        run.clear();
        output.push(character);
    }
    output.pop();
    output.chars().take(160).collect()
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
    let devices = timeout(PROBE_TIMEOUT, async {
        let mut mux = addr.connect(0).await?;
        mux.get_devices().await
    })
    .await
    .map_err(|_| {
        diagnostic::event(format_args!("event=usbmux_list_timeout"));
        UsbProbeError::Usbmuxd
    })?
    .map_err(|_| UsbProbeError::Usbmuxd)?;
    Ok(devices
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
    let timed_out = match timeout(USBMUX_PAIR_RECORD_TIMEOUT, provider.get_pairing_file()).await {
        Ok(Ok(record)) => return Ok((LockdownRecord(record), PairingRecordSource::Usbmuxd)),
        Ok(Err(_)) => false,
        Err(_) => true,
    };
    if let Ok(record) = local_pairing_record(data_dir, &provider.udid) {
        return Ok((record, PairingRecordSource::LocalFile));
    }
    if !timed_out {
        return Err(UsbProbeError::PairingRecordUnavailable);
    }
    // A slow service is not lost trust. Treating it as such would create a new pairing,
    // show a trust prompt and overwrite the record other Apple software shares.
    diagnostic::event(format_args!("event=usbmux_pair_record_slow"));
    match timeout(PROBE_TIMEOUT, provider.get_pairing_file()).await {
        Ok(Ok(record)) => Ok((LockdownRecord(record), PairingRecordSource::Usbmuxd)),
        Ok(Err(_)) => Err(UsbProbeError::PairingRecordUnavailable),
        Err(_) => Err(UsbProbeError::Timeout),
    }
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

async fn connect_lockdown_service(
    provider: &UsbmuxdProvider,
    lockdown: &mut LockdownClient,
    record: &LockdownRecord,
    legacy_tls: bool,
    service: impl Into<String>,
    step: UsbProbeError,
) -> Result<Idevice, UsbProbeError> {
    let (port, use_tls) = lockdown
        .start_service(service)
        .await
        .map_err(|error| device_error(error, step))?;
    let mut connection = provider
        .connect(port)
        .await
        .map_err(|error| traced(error, step))?;
    if use_tls {
        connection
            .start_session(&record.0, legacy_tls)
            .await
            .map_err(|error| traced(error, step))?;
    }
    Ok(connection)
}

/// Request only the Settings entry, using the trusted USB pairing for this phone.
pub(crate) async fn reveal_developer_menu(
    provider: &UsbmuxdProvider,
    data_dir: &Path,
) -> Result<(), UsbProbeError> {
    let request = async {
        let (mut lockdown, record, legacy_tls, _) = trusted_lockdown(provider, data_dir).await?;
        let connection = connect_lockdown_service(
            provider,
            &mut lockdown,
            &record,
            legacy_tls,
            "com.apple.amfi.lockdown",
            UsbProbeError::DeveloperMenu,
        )
        .await?;
        let socket = connection
            .get_socket()
            .ok_or(UsbProbeError::DeveloperMenu)?;
        request_developer_menu(socket).await
    };
    let result = timeout(PROBE_TIMEOUT, request)
        .with_subscriber(Dispatch::none())
        .await
        .unwrap_or(Err(UsbProbeError::Timeout));
    if let Err(error) = result {
        copiago_core::diagnostic::event(format_args!("developer_menu_failed {error}"));
    }
    result
}

async fn request_developer_menu(
    mut socket: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
) -> Result<(), UsbProbeError> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    // idevice 0.1.68 accepts even "success": false. Verify the actual reply
    // before directing the user to Settings. Only action 0 reveals the menu.
    let mut body = Vec::new();
    let request = plist::Dictionary::from_iter([("action", 0_u64)]);
    plist::Value::Dictionary(request)
        .to_writer_xml(&mut body)
        .map_err(|_| UsbProbeError::DeveloperMenu)?;
    let error = |_| UsbProbeError::DeveloperMenu;
    socket.write_u32(body.len() as u32).await.map_err(error)?;
    socket.write_all(&body).await.map_err(error)?;
    socket.flush().await.map_err(error)?;
    let length = socket.read_u32().await.map_err(error)?;
    if !(1..=65_536).contains(&length) {
        return Err(UsbProbeError::DeveloperMenu);
    }
    body.resize(length as usize, 0);
    socket.read_exact(&mut body).await.map_err(error)?;
    let reply: plist::Dictionary =
        plist::from_bytes(&body).map_err(|_| UsbProbeError::DeveloperMenu)?;
    if reply.get("success").and_then(plist::Value::as_boolean) != Some(true)
        || reply.contains_key("Error")
    {
        return Err(UsbProbeError::DeveloperMenu);
    }
    Ok(())
}

async fn open_usb_tunnel(
    provider: &UsbmuxdProvider,
    data_dir: &Path,
) -> Result<(RsdTunnel, PairingRecordSource), UsbProbeError> {
    open_usb_tunnel_traced(provider, data_dir, &Cell::new(TunnelStep::Lockdown)).await
}

async fn open_usb_tunnel_traced(
    provider: &UsbmuxdProvider,
    data_dir: &Path,
    step: &Cell<TunnelStep>,
) -> Result<(RsdTunnel, PairingRecordSource), UsbProbeError> {
    step.set(TunnelStep::Lockdown);
    let (mut lockdown, record, legacy_tls, source) = trusted_lockdown(provider, data_dir).await?;
    step.set(TunnelStep::CoreDeviceProxy);
    let connection = connect_lockdown_service(
        provider,
        &mut lockdown,
        &record,
        legacy_tls,
        CoreDeviceProxy::service_name(),
        UsbProbeError::CoreDeviceProxy,
    )
    .await?;
    drop(record);

    let proxy = CoreDeviceProxy::new(connection)
        .await
        .map_err(|error| traced(error, UsbProbeError::CoreDeviceProxy))?;
    drop(lockdown);
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    step.set(TunnelStep::SoftwareTunnel);
    let mut adapter = proxy
        .create_software_tunnel()
        .map_err(|error| traced(error, UsbProbeError::CoreDeviceProxy))?
        .to_async_handle();
    step.set(TunnelStep::RsdConnect);
    let stream = adapter
        .connect(rsd_port)
        .await
        .map_err(|error| traced(error, UsbProbeError::Rsd))?;
    step.set(TunnelStep::RsdHandshake);
    let rsd = RsdHandshake::new(stream)
        .await
        .map_err(|error| traced(error, UsbProbeError::Rsd))?;
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
        error => traced(error, step),
    }
}

#[cfg(test)]
mod tests;
