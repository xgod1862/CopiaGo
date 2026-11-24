use copiago_core::DeviceInfo;
use copiago_core::clip::{Clip, FileOffer};
use copiago_core::ddi::{DdiStage, Progress};
use copiago_core::i18n::Message;

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
    Unsupported {
        kind: Message,
    },
    SetupProgress(DdiStage, Progress),
    PairingPrompt(Message),
    Error(Message),
}

/// Commands the host sends to the device worker.
pub enum DeviceCommand {
    Push(Clip),
    FetchOffer(FileOffer),
    RequestPair,
    Forget,
    Retry,
    Shutdown,
}
