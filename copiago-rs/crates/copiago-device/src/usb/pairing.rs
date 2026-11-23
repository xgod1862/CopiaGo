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
    single_usb_provider, trusted_lockdown,
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
}

impl PairingError {
    pub(crate) fn message(self) -> Message {
        Message::new(match self {
            Self::Usb(UsbProbeError::NoUsbDevice) => {
                "未找到 USB 连接的 iPhone。请用数据线连接并解锁后重试"
            }
            Self::Usb(UsbProbeError::MultipleUsbDevices(_)) => "请暂时只连接一台 iPhone",
            Self::Usb(UsbProbeError::Usbmuxd) => "等待 Apple 设备连接服务或 iPhone…",
            Self::Usb(UsbProbeError::DeviceLocked) => "请先解锁 iPhone，再点「连接 iPhone」",
            Self::Usb(UsbProbeError::DeveloperModeRequired) => {
                "需要开启开发者模式：在 iPhone「设置 → 隐私与安全性 → 开发者模式」中开启并重启手机，解锁后点「继续连接」"
            }
            Self::Denied => {
                "iPhone 上选择了「不信任」。请重新插拔数据线，在提示时点「信任」，再点「连接 iPhone」"
            }
            Self::Pending | Self::Usb(UsbProbeError::Timeout) => {
                "未完成配对：请在 iPhone 上点「信任」并输入锁屏密码，然后再点「连接 iPhone」"
            }
            Self::ReadOnly => "当前为只读模式，无法配对",
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
    prompt: impl Fn(Message) -> ControlFlow<()>,
) -> Result<DeviceInfo, PairingError> {
    // idevice diagnostics include the underlying pairing payloads.
    pair(addr, data_dir, prompt)
        .with_subscriber(Dispatch::none())
        .await
}

async fn pair(
    addr: &UsbmuxdAddr,
    data_dir: &Path,
    prompt: impl Fn(Message) -> ControlFlow<()>,
) -> Result<DeviceInfo, PairingError> {
    let provider = single_usb_provider(addr).await.map_err(PairingError::Usb)?;
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
    let (mut lockdown, record, legacy_tls) = match trusted_lockdown(&provider, data_dir).await {
        Ok((lockdown, record, legacy_tls, _)) => (lockdown, record, legacy_tls),
        Err(UsbProbeError::PairingRecordUnavailable | UsbProbeError::TrustRequired) => {
            create_lockdown_pairing(&provider, &local_path, &hostname, &host_id).await?
        }
        Err(error) => return Err(PairingError::Usb(error)),
    };
    if prompt(message!("已确认 iPhone 信任，正在准备无线配对…")).is_break() {
        return Err(PairingError::Stopped);
    }
    let info = device_info::read_lockdown(&mut lockdown, data_dir).await;
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
