//! Windows clipboard, imaging, tray, startup, and HTTP integration.

#[cfg(windows)]
pub mod clipboard;
#[cfg(windows)]
pub mod http;
#[cfg(windows)]
pub mod image;
#[cfg(windows)]
pub mod update;
