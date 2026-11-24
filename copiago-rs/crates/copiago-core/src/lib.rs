//! Shared models, settings, and persistence for CopiaGo.

#[cfg(windows)]
pub mod atomic;
#[cfg(windows)]
pub mod device_info;
#[cfg(windows)]
pub mod dpapi;
pub mod i18n;
#[cfg(windows)]
pub mod paths;
#[cfg(windows)]
mod win32;

#[cfg(windows)]
pub use device_info::DeviceInfo;

#[cfg(all(test, windows))]
mod storage_tests;
#[cfg(all(test, windows))]
mod test_support;
