//! iPhone connections and clipboard synchronization.

#[cfg(windows)]
mod contract;
#[cfg(windows)]
pub mod discovery;
#[cfg(windows)]
mod pasteboard;
#[cfg(windows)]
mod usb;

#[cfg(windows)]
pub use contract::{ConnState, DeviceCommand, DeviceEvent};
#[cfg(windows)]
pub use pasteboard::PasteboardError;
#[cfg(windows)]
pub use usb::{PairingRecordSource, UsbProbe, UsbProbeError, UsbWatchError, probe_usb, watch_usb};
