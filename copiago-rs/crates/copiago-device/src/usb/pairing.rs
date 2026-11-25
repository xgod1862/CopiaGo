use std::{io::ErrorKind, ops::ControlFlow, path::Path};

use copiago_core::{
    DeviceInfo, atomic, dpapi,
    i18n::{Message, message},
    paths::{DEVICE_DATA_DIR, ensure_data_dir},
};
use idevice::remote_pairing::{
    RemotePairingLockdownService, RpPairingFile, errors::RemotePairingError,
};
use idevice::usbmuxd::errors::UsbmuxdError;
use plist::{Dictionary, Value};
use tracing::{Dispatch, instrument::WithSubscriber};

use super::{
    IdeviceError, IdeviceProvider, IdeviceService, LockdownClient, LockdownRecord,
    USBMUX_PAIR_RECORD_TIMEOUT, UsbProbeError, UsbmuxdAddr, UsbmuxdProvider, local_record_path,
    reveal_developer_menu, single_usb_provider, trusted_lockdown,
};
use crate::{
    device_info,
    wifi::{host_identifier, save_remote_record, windows_hostname},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PairingError {
    Usb(UsbProbeError),
    Denied,
    Pending,
    Failed,
    ReadOnly,
    Stopped,
    /// The phone runs a system older than [`MIN_IOS_MAJOR`].
    UnsupportedIos,
}

/// RemotePairing and the CoreDevice tunnel that CopiaGo uses exist from iOS 17.
const MIN_IOS_MAJOR: u32 = 17;

impl PairingError {
    pub(crate) fn message(self) -> Message {
        Message::new(match self {
            Self::Usb(UsbProbeError::NoUsbDevice) => super::NO_USB_IPHONE,
            Self::Usb(UsbProbeError::MultipleUsbDevices(_)) => "请暂时只连接一台 iPhone",
            Self::Usb(UsbProbeError::Usbmuxd) => super::APPLE_SERVICE_UNAVAILABLE,
            Self::Usb(UsbProbeError::DeviceLocked) => "请先解锁 iPhone，再点「连接 iPhone」",
            Self::Usb(UsbProbeError::DeveloperModeRequired) => {
                "需要开启开发者模式：在 iPhone「设置 → 隐私与安全性 → 开发者模式」中开启并重启手机，解锁后点「继续连接」"
            }
            Self::Usb(UsbProbeError::DeveloperMenu) => super::DEVELOPER_MENU_RETRY,
            Self::Denied => {
                "iPhone 上选择了「不信任」。请重新插拔数据线，在提示时点「信任」，再点「连接 iPhone」"
            }
            Self::Pending | Self::Usb(UsbProbeError::Timeout) => {
                "未完成配对：请在 iPhone 上点「信任」并输入锁屏密码，然后再点「连接 iPhone」"
            }
            Self::ReadOnly => "当前为只读模式，无法配对",
            Self::UnsupportedIos => {
                "CopiaGo 需要 iOS 17 或更高版本，这台 iPhone 的系统版本过低。请在 iPhone「设置 → 通用 → 软件更新」升级后，再点「连接 iPhone」。"
            }
            _ => "配对未完成：请保持 iPhone 解锁并连着数据线，然后再点「连接 iPhone」",
        })
    }
}

impl From<IdeviceError> for PairingError {
    fn from(error: IdeviceError) -> Self {
        match error {
            IdeviceError::UserDeniedPairing
            | IdeviceError::CanceledByUser
            | IdeviceError::RemotePairing(RemotePairingError::PairingRejected(_)) => Self::Denied,
            IdeviceError::PairingDialogResponsePending | IdeviceError::Timeout => Self::Pending,
            IdeviceError::DeviceLocked | IdeviceError::PasswordProtected => {
                Self::Usb(UsbProbeError::DeviceLocked)
            }
            IdeviceError::DeveloperModeNotEnabled => {
                Self::Usb(UsbProbeError::DeveloperModeRequired)
            }
            IdeviceError::Usbmuxd(UsbmuxdError::ConnectionRefused) => {
                Self::Usb(UsbProbeError::Usbmuxd)
            }
            IdeviceError::Socket(error) if error.kind() == ErrorKind::ConnectionRefused => {
                Self::Usb(UsbProbeError::Usbmuxd)
            }
            _ => Self::Failed,
        }
    }
}

pub(crate) async fn pair_over_usb(
    addr: &UsbmuxdAddr,
    data_dir: &Path,
    allow_developer_setup: bool,
    prompt: impl Fn(Message) -> ControlFlow<()>,
) -> Result<DeviceInfo, PairingError> {
    // idevice diagnostics include the underlying pairing payloads.
    async {
        let provider = single_usb_provider(addr).await.map_err(PairingError::Usb)?;
        let result = pair(&provider, data_dir, prompt).await;
        if allow_developer_setup
            && matches!(
                result,
                Err(PairingError::Usb(UsbProbeError::DeveloperModeRequired))
            )
            && reveal_developer_menu(&provider, data_dir).await.is_err()
        {
            return Err(PairingError::Usb(UsbProbeError::DeveloperMenu));
        }
        result
    }
    .with_subscriber(Dispatch::none())
    .await
}

async fn pair(
    provider: &UsbmuxdProvider,
    data_dir: &Path,
    prompt: impl Fn(Message) -> ControlFlow<()>,
) -> Result<DeviceInfo, PairingError> {
    let local_path = local_record_path(data_dir, &provider.udid).ok_or(PairingError::Failed)?;
    let remote_path = local_path.with_file_name(format!("remote_{}.plist", provider.udid));
    ensure_data_dir(data_dir).map_err(|_| PairingError::Failed)?;
    ensure_data_dir(&data_dir.join(DEVICE_DATA_DIR)).map_err(|_| PairingError::Failed)?;

    if prompt(message!(
        "正在等待 iPhone 信任此电脑。请解锁手机，按提示点「信任」并输入锁屏密码。"
    ))
    .is_break()
    {
        return Err(PairingError::Stopped);
    }
    let hostname = windows_hostname().map_err(|_| PairingError::Failed)?;
    let host_id = host_identifier(&hostname);
    let (mut lockdown, record, legacy_tls) =
        ensure_usb_trust(provider, data_dir, &local_path, &hostname, &host_id).await?;
    if prompt(message!("已确认 iPhone 信任，正在准备无线配对…")).is_break() {
        return Err(PairingError::Stopped);
    }
    let info = device_info::read_lockdown(&mut lockdown, data_dir).await;
    // Older phones would otherwise fail at the next step with a generic message that
    // no retry can fix. An unreadable version is not treated as too old.
    if ios_major(&info.os_version).is_some_and(|major| major < MIN_IOS_MAJOR) {
        return Err(PairingError::UnsupportedIos);
    }
    let _ = info.save(data_dir);
    enable_wifi_connections(&mut lockdown).await?;

    let (port, use_tls) = lockdown
        .start_service(RemotePairingLockdownService::service_name())
        .await?;
    let mut connection = provider.connect(port).await?;
    if use_tls {
        connection.start_session(&record.0, legacy_tls).await?;
    }
    drop(record);
    let mut client = RemotePairingLockdownService::new(connection).into_client(&hostname)?;
    let mut pairing = RpPairingFile::generate(&hostname);
    pairing.identifier = host_id;
    client
        .connect(&mut pairing, || async { "000000".into() })
        .await?;

    // The post-pairing unlock key is optional for the tunnel. Some peers close the pair stream.
    let command = Dictionary::from_iter([("createRemoteUnlockKey", Dictionary::new())]);
    let body = Dictionary::from_iter([("_0", command)]);
    let unlock_request = Value::Dictionary(Dictionary::from_iter([("request", body)]));
    let unlock_key = tokio::time::timeout(
        USBMUX_PAIR_RECORD_TIMEOUT,
        client.send_receive_encrypted_request(unlock_request),
    )
    .await
    .ok()
    .and_then(Result::ok)
    .and_then(Value::into_dictionary)
    .and_then(|mut fields| fields.remove("createRemoteUnlockKey"))
    .and_then(Value::into_dictionary)
    .and_then(|mut fields| fields.remove("hostKey"))
    .and_then(Value::into_string)
    .unwrap_or_default();
    save_remote_record(&remote_path, pairing, unlock_key).map_err(|_| PairingError::Failed)?;
    Ok(info)
}

/// Recover only USB trust. A cable swap or multiple attached phones must not
/// cause a trust request for a different device. Keep the wireless key intact.
pub(crate) async fn renew_usb_trust(
    addr: &UsbmuxdAddr,
    data_dir: &Path,
    serial: &str,
) -> Result<(), PairingError> {
    async {
        let provider = single_usb_provider(addr).await.map_err(PairingError::Usb)?;
        if provider.udid != serial {
            return Err(PairingError::Usb(UsbProbeError::NoUsbDevice));
        }
        let path = local_record_path(data_dir, serial).ok_or(PairingError::Failed)?;
        let hostname = windows_hostname().map_err(|_| PairingError::Failed)?;
        let host_id = host_identifier(&hostname);
        ensure_usb_trust(&provider, data_dir, &path, &hostname, &host_id).await?;
        Ok(())
    }
    .with_subscriber(Dispatch::none())
    .await
}

async fn ensure_usb_trust(
    provider: &UsbmuxdProvider,
    data_dir: &Path,
    local_path: &Path,
    hostname: &str,
    host_id: &str,
) -> Result<(LockdownClient, LockdownRecord, bool), PairingError> {
    match trusted_lockdown(provider, data_dir).await {
        Ok((lockdown, record, legacy_tls, _)) => Ok((lockdown, record, legacy_tls)),
        Err(UsbProbeError::PairingRecordUnavailable | UsbProbeError::TrustRequired) => {
            create_lockdown_pairing(provider, local_path, hostname, host_id).await
        }
        Err(error) => Err(PairingError::Usb(error)),
    }
}

async fn create_lockdown_pairing(
    provider: &UsbmuxdProvider,
    local_path: &Path,
    hostname: &str,
    host_id: &str,
) -> Result<(LockdownClient, LockdownRecord, bool), PairingError> {
    let mut mux = provider.addr.connect(provider.tag).await?;
    let buid = mux.get_buid().await?;
    let mut lockdown = LockdownClient::connect(provider).await?;
    let record = LockdownRecord(lockdown.pair(host_id, buid, Some(hostname)).await?);
    let mut bytes = zeroize::Zeroizing::new(record.0.clone().serialize()?);
    let sealed = dpapi::protect(&bytes).map_err(|_| PairingError::Failed)?;
    atomic::write(local_path, &sealed).map_err(|_| PairingError::Failed)?;
    // A local record remains usable when the Apple device service cannot cache it.
    let _ = tokio::time::timeout(
        USBMUX_PAIR_RECORD_TIMEOUT,
        mux.save_pair_record(&provider.udid, std::mem::take(&mut *bytes)),
    )
    .await;
    let legacy_tls = lockdown.start_session(&record.0).await?;
    Ok((lockdown, record, legacy_tls))
}

fn ios_major(version: &str) -> Option<u32> {
    version.split('.').next()?.trim().parse().ok()
}

async fn enable_wifi_connections(lockdown: &mut LockdownClient) -> Result<(), IdeviceError> {
    const KEY: &str = "EnableWifiConnections";
    const DOMAIN: &str = "com.apple.mobile.wireless_lockdown";
    let enabled = match lockdown.get_value(Some(KEY), Some(DOMAIN)).await {
        Ok(value) => value.as_boolean().unwrap_or(false),
        Err(IdeviceError::UnknownErrorType(error))
            if error == "MissingValue" || error.starts_with("MissingValue (") =>
        {
            false
        }
        Err(IdeviceError::UnexpectedResponse(error))
            if error == "missing Value in GetValue response" =>
        {
            false
        }
        Err(error) => return Err(error),
    };
    if !enabled {
        lockdown.set_value(KEY, true.into(), Some(DOMAIN)).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
