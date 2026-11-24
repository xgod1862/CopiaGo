use std::{ops::ControlFlow, path::Path};

use copiago_core::{
    DeviceInfo,
    ddi::{DdiStage, Progress},
    i18n::{Message, message},
};
use copiago_ddi::setup::{SetupError, SetupUpdate, prepare_developer_services};
use idevice::{rsd::RsdHandshake, tcp::handle::AdapterHandle, usbmuxd::UsbmuxdAddr};

use crate::{
    ConnState, DeviceEvent, PasteboardError, UsbProbeError, WifiProbeError, device_info,
    pasteboard::{PASTEBOARD_SERVICE, PasteboardState, WatchOptions, watch_rsd},
    usb::{PairingError, RsdTunnel, open_paired_usb_tunnel, pair_over_usb, paired_usb_id},
    wifi::{
        PairedDevice, WifiProbeOptions, WifiTunnel, find_paired_device, forget_remote_records,
        open_wifi_tunnel,
    },
};

use super::{Backend, ClipboardQueue, DeviceRunOptions, Failure, Transport};

#[cfg(test)]
mod tests;

pub(super) struct NativeDevice<'a> {
    pub data_dir: &'a Path,
    pub addr: &'a UsbmuxdAddr,
    pub options: DeviceRunOptions,
}

impl Backend for NativeDevice<'_> {
    fn paired_device(&self) -> Result<PairedDevice, WifiProbeError> {
        find_paired_device(self.data_dir)
    }

    async fn pair(
        &self,
        prompt: impl Fn(Message) -> ControlFlow<()>,
    ) -> Result<DeviceInfo, PairingError> {
        if !self.options.persist_device_data {
            return Err(PairingError::ReadOnly);
        }
        pair_over_usb(self.addr, self.data_dir, prompt).await
    }

    fn forget(&self) -> std::io::Result<()> {
        if !self.options.persist_device_data {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "read-only device worker",
            ));
        }
        forget_remote_records(self.data_dir)
    }

    async fn usb_id(&self, device: &PairedDevice) -> Result<Option<u32>, UsbProbeError> {
        paired_usb_id(self.addr, device).await
    }

    async fn session(
        &self,
        device: &PairedDevice,
        transport: Transport,
        clipboard: &mut PasteboardState,
        commands: &ClipboardQueue,
        emit: impl Fn(DeviceEvent) -> ControlFlow<()>,
    ) -> Result<(), Failure> {
        let wireless = transport == Transport::Wifi;
        let opened = open_pasteboard(
            async || self.open(device, transport).await,
            async |tunnel: &mut Tunnel| {
                let (adapter, rsd) = tunnel.parts();
                prepare_developer_services(adapter, rsd, self.data_dir, |update| {
                    setup_progress(update, &emit)
                })
                .await
            },
            wireless,
            self.options.persist_device_data && self.options.allow_developer_setup,
            &emit,
        )
        .await?;
        let Some(mut tunnel) = opened else {
            return Ok(());
        };
        let (adapter, rsd) = tunnel.parts();
        let info = device_info::read_rsd(adapter, rsd, self.data_dir).await;
        if self.options.persist_device_data {
            let _ = info.save(self.data_dir);
        }
        let result = if emit(DeviceEvent::DeviceInfo(info)).is_break() {
            Ok(())
        } else {
            watch_rsd(
                adapter,
                rsd,
                WatchOptions {
                    data_dir: self.data_dir,
                    wireless,
                    language: self.options.language,
                    follow_saved_language: self.options.follow_saved_language,
                },
                clipboard,
                async || Some(commands.recv().await),
                emit,
            )
            .await
            .map(|_| ())
            .map_err(|error| match error {
                PasteboardError::ServiceMissing => Failure::setup(DdiStage::Install),
                _ => Failure::Session,
            })
        };
        tunnel.close().await;
        result
    }
}

impl NativeDevice<'_> {
    async fn open(&self, device: &PairedDevice, transport: Transport) -> Result<Tunnel, Failure> {
        let open = match transport {
            Transport::Wifi => {
                let options = WifiProbeOptions {
                    write_identifier: self.options.persist_device_data,
                    ..WifiProbeOptions::default()
                };
                let (tunnel, _) = open_wifi_tunnel(device, options)
                    .await
                    .map_err(Failure::Wifi)?;
                Ok(Tunnel::Wifi(tunnel))
            }
            Transport::Usb(id) => open_paired_usb_tunnel(self.addr, self.data_dir, device, id)
                .await
                .map(Tunnel::Usb)
                .map_err(Failure::Usb),
        };
        if open.is_ok() {
            let transport = if transport == Transport::Wifi {
                "wifi"
            } else {
                "usb"
            };
            copiago_core::diagnostic::event(format_args!("tunnel_open transport={transport}"));
        }
        open
    }
}

async fn open_pasteboard<T: PasteboardTunnel>(
    mut open: impl AsyncFnMut() -> Result<T, Failure>,
    prepare: impl AsyncFnOnce(&mut T) -> Result<(), SetupError>,
    wireless: bool,
    allow_setup: bool,
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
) -> Result<Option<T>, Failure> {
    let mut tunnel = open().await?;
    if tunnel.has_pasteboard() {
        return Ok(Some(tunnel));
    }
    let missing = emit(DeviceEvent::DeveloperServicesMissing { wireless });
    let result = if missing.is_break() {
        Err(SetupError::Canceled)
    } else if allow_setup {
        prepare(&mut tunnel).await
    } else {
        Err(SetupError::Failed {
            stage: DdiStage::Install,
            timed_out: false,
        })
    };
    tunnel.close().await;
    match result {
        Ok(()) => {}
        Err(SetupError::Canceled) => return Ok(None),
        Err(SetupError::DeveloperModeRequired) => return Err(Failure::DeveloperModeRequired),
        Err(SetupError::Failed { stage, timed_out }) => {
            return Err(Failure::Setup { stage, timed_out });
        }
    }

    if emit(DeviceEvent::Status(
        ConnState::Connecting,
        message!("正在重新连接并验证剪贴板服务…"),
    ))
    .is_break()
    {
        return Ok(None);
    }
    let reopened = open().await?;
    if reopened.has_pasteboard() {
        return Ok(Some(reopened));
    }
    reopened.close().await;
    Err(Failure::setup(DdiStage::Install))
}

fn setup_progress(
    update: SetupUpdate,
    emit: &impl Fn(DeviceEvent) -> ControlFlow<()>,
) -> ControlFlow<()> {
    let (message, progress) = match update {
        SetupUpdate::Checking => (message!("正在检查 iPhone 开发者服务…"), None),
        SetupUpdate::Downloading => (
            message!("正在下载 iPhone 开发组件…"),
            Some((DdiStage::Download, Progress::default())),
        ),
        SetupUpdate::Download(event) => {
            let message = if event.switching_source {
                message!(
                    "正在换用备用下载源 · 文件 {index}/{count}",
                    index = event.index,
                    count = event.count
                )
            } else if let Some(total) = event.progress.total_bytes {
                message!(
                    "正在下载文件 {index}/{count} · {received} / {total}",
                    index = event.index,
                    count = event.count,
                    received = human_size(event.progress.received_bytes),
                    total = human_size(total),
                )
            } else {
                message!(
                    "正在下载文件 {index}/{count} · 已收到 {received}",
                    index = event.index,
                    count = event.count,
                    received = human_size(event.progress.received_bytes),
                )
            };
            (message, Some((DdiStage::Download, event.progress)))
        }
        SetupUpdate::Preparing => (message!("正在准备安装开发者镜像…"), None),
        SetupUpdate::Signing => (
            message!("正在请求 Apple 签名…"),
            Some((DdiStage::Sign, Progress::default())),
        ),
        SetupUpdate::Installing => (
            message!("正在将开发者镜像安装到 iPhone…"),
            Some((DdiStage::Install, Progress::default())),
        ),
    };
    emit(DeviceEvent::Status(ConnState::Connecting, message))?;
    if let Some((stage, progress)) = progress {
        emit(DeviceEvent::SetupProgress(stage, progress))?;
    }
    ControlFlow::Continue(())
}

fn human_size(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * KIB;
    const GIB: u64 = 1024 * MIB;
    match bytes {
        GIB.. => format!("{:.1} GB", bytes as f64 / GIB as f64),
        MIB.. => format!("{:.1} MB", bytes as f64 / MIB as f64),
        KIB.. => format!("{:.0} KB", bytes as f64 / KIB as f64),
        _ => format!("{bytes} B"),
    }
}

enum Tunnel {
    Wifi(WifiTunnel),
    Usb(RsdTunnel),
}

impl Tunnel {
    fn parts(&mut self) -> (&mut AdapterHandle, &mut RsdHandshake) {
        match self {
            Self::Wifi(tunnel) => (&mut tunnel.adapter, &mut tunnel.rsd),
            Self::Usb(tunnel) => (&mut tunnel.adapter, &mut tunnel.rsd),
        }
    }
}

trait PasteboardTunnel {
    fn has_pasteboard(&self) -> bool;
    async fn close(self);
}

impl PasteboardTunnel for Tunnel {
    fn has_pasteboard(&self) -> bool {
        let services = match self {
            Self::Wifi(tunnel) => &tunnel.rsd.services,
            Self::Usb(tunnel) => &tunnel.rsd.services,
        };
        services.contains_key(PASTEBOARD_SERVICE)
    }

    async fn close(self) {
        match self {
            Self::Wifi(tunnel) => {
                let _ = tunnel.close().await;
            }
            Self::Usb(mut tunnel) => {
                let _ = tunnel.adapter.close().await;
            }
        }
    }
}
