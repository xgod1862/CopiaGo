//! iPhone connections and clipboard synchronization.

#[cfg(windows)]
mod connection;
#[cfg(windows)]
mod contract;
#[cfg(windows)]
mod device_info;
#[cfg(windows)]
pub mod discovery;
#[cfg(windows)]
mod pasteboard;
#[cfg(windows)]
mod usb;
#[cfg(windows)]
mod wifi;

#[cfg(windows)]
pub use connection::{DeviceRunOptions, HostSignal, HostSignals, run_device};
#[cfg(windows)]
pub use contract::{ConnState, DeviceCommand, DeviceEvent};
#[cfg(windows)]
pub use pasteboard::PasteboardError;
#[cfg(windows)]
pub use usb::{
    PairingRecordSource, UsbProbe, UsbProbeError, UsbWatchError, probe_usb, sync_usb, watch_usb,
};
#[cfg(windows)]
pub use wifi::{IdentifierMigration, WifiProbe, WifiProbeError, WifiProbeOptions, probe_wifi};
