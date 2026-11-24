use std::{ops::ControlFlow, path::Path};

use copiago_core::ddi::DdiStage;
use idevice::{rsd::RsdHandshake, tcp::handle::AdapterHandle, usbmuxd::UsbmuxdAddr};
use tokio::sync::mpsc;

use crate::{
    DeviceCommand, DeviceEvent, PasteboardError, UsbProbeError, WifiProbeError,
    pasteboard::{PASTEBOARD_SERVICE, PasteboardState, watch_rsd},
    usb::{RsdTunnel, open_paired_usb_tunnel, paired_usb_id},
    wifi::{PairedDevice, WifiProbeOptions, WifiTunnel, find_paired_device, open_wifi_tunnel},
};

use super::{Backend, DeviceRunOptions, Failure, Transport};

mod device_info;

pub(super) struct NativeDevice<'a> {
    pub data_dir: &'a Path,
    pub addr: &'a UsbmuxdAddr,
    pub options: DeviceRunOptions,
}

impl Backend for NativeDevice<'_> {
    fn paired_device(&self) -> Result<PairedDevice, WifiProbeError> {
        find_paired_device(self.data_dir)
    }

    async fn usb_id(&self, device: &PairedDevice) -> Result<Option<u32>, UsbProbeError> {
        paired_usb_id(self.addr, device).await
    }

    async fn session(
        &self,
        device: &PairedDevice,
        transport: Transport,
        clipboard: &mut PasteboardState,
        commands: &mut mpsc::Receiver<DeviceCommand>,
        emit: impl Fn(DeviceEvent) -> ControlFlow<()>,
    ) -> Result<(), Failure> {
        let mut tunnel = self.open(device, transport).await?;
        let wireless = transport == Transport::Wifi;
        let (adapter, rsd) = tunnel.parts();
        let result = if !rsd.services.contains_key(PASTEBOARD_SERVICE) {
            match emit(DeviceEvent::DeveloperServicesMissing { wireless }) {
                ControlFlow::Continue(()) => Err(Failure::Setup(DdiStage::Install)),
                ControlFlow::Break(()) => Ok(()),
            }
        } else {
            let info = device_info::read(adapter, rsd, self.data_dir).await;
            if self.options.persist_device_data {
                let _ = info.save(self.data_dir);
            }
            if emit(DeviceEvent::DeviceInfo(info)).is_break() {
                Ok(())
            } else {
                watch_rsd(
                    adapter,
                    rsd,
                    wireless,
                    self.options.language,
                    clipboard,
                    Some(commands),
                    emit,
                )
                .await
                .map(|_| ())
                .map_err(|error| match error {
                    PasteboardError::ServiceMissing => Failure::Setup(DdiStage::Install),
                    _ => Failure::Session,
                })
            }
        };
        tunnel.close().await;
        result
    }
}

impl NativeDevice<'_> {
    async fn open(&self, device: &PairedDevice, transport: Transport) -> Result<Tunnel, Failure> {
        match transport {
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
        }
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
