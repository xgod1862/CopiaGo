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
    PushResult(Option<u64>, Result<(), PasteboardError>),
    ForgetResult(bool),
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
    /// The optional host-supplied id is echoed on success, rejection and disconnect.
    Push(Clip, Option<u64>),
    FetchOffer(FileOffer),
    /// Pair one USB iPhone when no record exists, or retry a paused connection.
    RequestPair,
    /// Cancel the current operation and remove CopiaGo's wireless pairing records and cached device info.
    Forget,
    Retry,
    Shutdown,
}
