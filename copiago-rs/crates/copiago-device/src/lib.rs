//! iPhone connections and clipboard synchronization.

#[cfg(windows)]
mod contract;

#[cfg(windows)]
pub mod discovery;
#[cfg(windows)]
mod usb;

#[cfg(windows)]
pub use contract::{ConnState, DeviceCommand, DeviceEvent};
#[cfg(windows)]
pub use usb::{PairingRecordSource, UsbProbe, UsbProbeError, probe_usb};
