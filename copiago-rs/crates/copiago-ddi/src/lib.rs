//! Developer disk image download, validation, signing, and mounting.

#[cfg(windows)]
pub mod download;

#[cfg(windows)]
pub mod setup;
