//! Progress shared by the developer image installer and its consumers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DdiStage {
    Download,
    Sign,
    Install,
}

/// Why a setup stage failed, as far as the user can act on it. Never carries device data.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DdiCause {
    #[default]
    Unknown,
    /// The phone locked while its developer services were being prepared.
    DeviceLocked,
    /// The USB or network link to the phone dropped mid-operation.
    Disconnected,
    /// The phone's image mounter service did not answer.
    ServiceUnavailable,
    /// The phone answered but refused the image or ticket.
    DeviceRejected,
    /// The downloaded image does not fit this phone's model or iOS build.
    Unsupported,
    /// Cached image files on this PC could not be read or parsed.
    LocalFiles,
    /// Apple's signing server answered but declined to sign.
    SigningRejected,
    /// Apple's signing server returned an HTTP error.
    SigningServer,
    /// TLS to Apple failed, typically because of an intercepting proxy.
    SecureConnection,
    /// The PC could not reach Apple's signing server.
    Network,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    pub received_bytes: u64,
    /// A missing total indicates indeterminate progress.
    pub total_bytes: Option<u64>,
    pub bytes_per_second: u64,
}
