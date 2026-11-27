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
    FileProgress(u64, FileProgress),
    FileResult(u64, Result<ReceivedFile, FileTransferError>),
    Unsupported {
        kind: Message,
    },
    /// The phone copied something CopiaGo syncs, but its data could not be read:
    /// the request timed out, or the phone declared the type without supplying bytes.
    /// The copy is not retried; the user can copy it again.
    ReadFailed {
        kind: Message,
        timed_out: bool,
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

/// File payload bytes written so far and the validated payload length.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileProgress {
    pub received_bytes: u64,
    pub total_bytes: u64,
}

/// Commands the host sends to the device worker.
pub enum DeviceCommand {
    /// The optional host-supplied id is echoed on success, rejection and disconnect.
    Push(Clip, Option<u64>),
    /// The host-supplied id is echoed by progress and the terminal result.
    FetchOffer(FileOffer, u64),
    /// Pair one USB iPhone when no record exists, or retry a paused connection.
    RequestPair,
    /// Cancel the current operation and remove CopiaGo's wireless pairing records and cached device info.
    Forget,
    Retry,
    Shutdown,
}
