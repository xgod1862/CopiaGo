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
pub mod file_bench;
#[cfg(windows)]
mod pasteboard;
#[cfg(windows)]
mod usb;
#[cfg(windows)]
mod wifi;

#[cfg(windows)]
pub use connection::{DeviceRunOptions, HostSignal, HostSignals, WAITING_FOR_IPHONE, run_device};
#[cfg(windows)]
pub use contract::{ConnState, DeviceCommand, DeviceEvent, FileProgress};
#[cfg(windows)]
pub use pasteboard::{FileTransferError, PasteboardError};
#[cfg(windows)]
pub use usb::{
    APPLE_SERVICE_UNAVAILABLE, NO_USB_IPHONE, PairingRecordSource, UsbProbe, UsbProbeError,
    UsbWatchError, probe_usb, sync_usb, watch_usb,
};
#[cfg(windows)]
pub use wifi::{
    IdentifierMigration, WifiProbe, WifiProbeError, WifiProbeOptions, has_pairing_records,
    probe_wifi, probe_wifi_pasteboard_types,
};
