use copiago_core::DeviceInfo;
use copiago_core::clip::{Clip, FileOffer};
use copiago_core::ddi::{DdiStage, Progress};
use copiago_core::files::ReceivedFile;
use copiago_core::i18n::Message;

use crate::{FileTransferError, PasteboardError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnState {
    Idle,
    Setup,
    Pairing,
    Waiting,
    Connecting,
    Developer,
    Connected { wireless: bool },
    Unsupported,
    Hold,
    Error,
}

/// Events the device worker sends to the host.
pub enum DeviceEvent {
    Status(ConnState, Message),
    DeviceInfo(DeviceInfo),
    Clip {
        clip: Clip,
        while_disconnected: bool,
    },
    FileOffer(FileOffer),
    FileResult(Result<ReceivedFile, FileTransferError>),
    Unsupported {
        kind: Message,
    },
    PushResult(Result<(), PasteboardError>),
    /// The tunnel opened but RSD did not advertise the pasteboard service.
    DeveloperServicesMissing {
        wireless: bool,
    },
    SetupProgress(DdiStage, Progress),
    PairingPrompt(Message),
    Error(Message),
}

/// Commands the host sends to the device worker.
pub enum DeviceCommand {
    Push(Clip),
    FetchOffer(FileOffer),
    /// Rescan pairing records after the host completes pairing (T-DEV-09).
    RequestPair,
    /// Cancel the current session and reset clipboard state after the host removes pairing data.
    Forget,
    Retry,
    Shutdown,
}
