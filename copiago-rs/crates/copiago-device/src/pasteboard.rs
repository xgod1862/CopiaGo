//! Clipboard synchronization on a live CoreDevice tunnel.

use std::fmt;
use std::ops::ControlFlow;
use std::time::Duration;

use copiago_core::{
    clip::{Clip, PasteboardChange},
    i18n::{Language, Message},
};
use tokio::{
    sync::Notify,
    time::{Instant, sleep_until, timeout},
};

use crate::{ConnState, DeviceCommand, DeviceEvent};

mod content;
mod file_offer;
pub(crate) mod file_transfer;
mod rsd;

#[cfg(test)]
pub(crate) use rsd::verify_push_round_trip;
pub(crate) use rsd::{PASTEBOARD_SERVICE, read_rsd_types, watch_rsd};

use content::{PNG_UTI, Snapshot, Update, read_clip};

const NOTIFICATIONS: [&str; 3] = [
    "com.apple.pasteboard.notify.changed",
    "com.apple.springboard.lockstate",
    "com.apple.mobile.keybagd.lock_status",
];
const METADATA_TIMEOUT: Duration = Duration::from_secs(8);
const SET_TIMEOUT: Duration = Duration::from_secs(20);
const ECHO_WINDOW: Duration = Duration::from_secs(120);
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const LISTING_RETRY: Duration = Duration::from_secs(5);
const MAX_POLL_FAILURES: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PasteboardError {
    ServiceMissing,
    NotificationsUnavailable,
    NotificationsClosed,
    RequestFailed,
    RequestTimeout,
    MetadataUnresponsive,
    UnsupportedPush,
    Disconnected,
    PushBusy,
}

impl fmt::Display for PasteboardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::ServiceMissing => "pasteboard service is not advertised",
            Self::NotificationsUnavailable => "could not subscribe to pasteboard notifications",
            Self::NotificationsClosed => "pasteboard notification stream closed",
            Self::RequestFailed => "pasteboard request failed",
            Self::RequestTimeout => "pasteboard request timed out",
            Self::MetadataUnresponsive => "device tunnel stopped responding to pasteboard polls",
            Self::UnsupportedPush => "pasteboard push requires text or a prepared PNG",
            Self::Disconnected => "iPhone is not connected",
            Self::PushBusy => "the waiting pasteboard pushes exceed the queue's memory budget",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for PasteboardError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PullPolicy {
    AllPromised,
    PromiseSecondary,
    AllResolved,
    Threshold(i64),
}

trait PasteboardBackend {
    async fn pull(&mut self, policy: PullPolicy) -> Result<Snapshot, PasteboardError>;
    async fn set(&mut self, item: SetItem) -> Result<Option<PasteboardChange>, PasteboardError>;
}

#[derive(PartialEq, Eq)]
enum SetItem {
    Text(String),
    Png(Vec<u8>),
}

/// State owned by the worker for one paired phone. Drop it when switching phones.
#[derive(Default)]
pub(crate) struct PasteboardState {
    last_change: Option<PasteboardChange>,
    last_push: Option<([u8; 32], Instant)>,
}

impl PasteboardState {
    pub(super) fn reconnect(&mut self, initial: Option<PasteboardChange>) -> bool {
        if self.last_change.is_none() {
            self.last_change = initial;
            return false;
        }
        initial != self.last_change
    }

    async fn push(
        &mut self,
        backend: &mut impl PasteboardBackend,
        clip: Clip,
    ) -> Result<(), PasteboardError> {
        let fingerprint = clip.fingerprint();
        let item = match clip {
            Clip::Text(text) => SetItem::Text(text),
            Clip::Image { bytes, uti } if uti == PNG_UTI && !bytes.is_empty() => {
                SetItem::Png(bytes)
            }
            _ => return Err(PasteboardError::UnsupportedPush),
        };
        // A timed-out SET can still have changed the phone's clipboard.
        self.last_push = Some((fingerprint, Instant::now()));
        let change = timeout(SET_TIMEOUT, backend.set(item))
            .await
            .map_err(|_| PasteboardError::RequestTimeout)??;
        if change.is_some() {
            self.last_change = change;
        }
        Ok(())
    }

    fn is_echo(&self, clip: &Clip) -> bool {
        self.last_push
            .is_some_and(|(sent, at)| at.elapsed() < ECHO_WINDOW && clip.fingerprint() == sent)
    }
}

async fn pull(
    backend: &mut impl PasteboardBackend,
    policy: PullPolicy,
) -> Result<Snapshot, PasteboardError> {
    let deadline = match policy {
        PullPolicy::AllPromised => METADATA_TIMEOUT,
        PullPolicy::PromiseSecondary => Duration::from_secs(15),
        PullPolicy::AllResolved | PullPolicy::Threshold(_) => Duration::from_secs(30),
    };
    timeout(deadline, backend.pull(policy))
        .await
        .map_err(|_| PasteboardError::RequestTimeout)?
}

async fn watch(
    backend: &mut impl PasteboardBackend,
    receive: impl AsyncFnMut() -> Result<String, PasteboardError>,
    wireless: bool,
    language: Language,
    state: &mut PasteboardState,
    next_command: impl AsyncFnMut() -> Option<DeviceCommand>,
    emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<Option<DeviceCommand>, PasteboardError> {
    let changed = Notify::new();
    // Keep the notification reader alive while pulls and timers run. Dropping an
    // in-progress read can leave the proxy between its length header and payload.
    tokio::select! {
        result = relay_changes(&changed, receive) => result.map(|()| None),
        result = watch_updates(backend, &changed, wireless, language, state, next_command, emit) => result,
    }
}

async fn relay_changes(
    changed: &Notify,
    mut receive: impl AsyncFnMut() -> Result<String, PasteboardError>,
) -> Result<(), PasteboardError> {
    loop {
        let name = receive().await?;
        if NOTIFICATIONS.contains(&name.as_str()) {
            changed.notify_one();
        }
    }
}

async fn watch_updates(
    backend: &mut impl PasteboardBackend,
    changed: &Notify,
    wireless: bool,
    language: Language,
    state: &mut PasteboardState,
    mut next_command: impl AsyncFnMut() -> Option<DeviceCommand>,
    mut emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<Option<DeviceCommand>, PasteboardError> {
    let initial = pull(backend, PullPolicy::AllPromised).await?.change;
    let mut stale = state.reconnect(initial);
    if stale {
        changed.notify_one();
    }
    let connected = Message::new(if wireless {
        "已连接（无线）"
    } else {
        "已连接（USB） · 可以拔掉数据线，之后将通过无线连接"
    });
    if emit(DeviceEvent::Status(
        ConnState::Connected { wireless },
        connected,
    ))
    .is_break()
    {
        return Ok(None);
    }

    let mut next_poll = Instant::now() + POLL_INTERVAL;
    let mut retry = None;
    let mut failures = 0;
    loop {
        // Metadata, content and SET share one backend. Complete each request
        // and update the change id before handling the next notification.
        tokio::select! {
            command = next_command() => {
                match command {
                    Some(DeviceCommand::Push(clip)) => {
                        let result = state.push(backend, clip).await;
                        if emit(DeviceEvent::PushResult(result)).is_break() {
                            return Ok(None);
                        }
                    }
                    // The connection manager owns lifecycle commands.
                    other => return Ok(other),
                }
            }
            _ = sleep_until(next_poll) => {
                match pull(backend, PullPolicy::AllPromised).await {
                    Ok(listing) => {
                        failures = 0;
                        if listing.change.is_some() && listing.change != state.last_change {
                            changed.notify_one();
                        }
                    }
                    Err(_) => {
                        failures += 1;
                        if failures == MAX_POLL_FAILURES {
                            return Err(PasteboardError::MetadataUnresponsive);
                        }
                    }
                }
                next_poll = Instant::now() + POLL_INTERVAL;
            }
            _ = changed.notified(), if retry.is_none() => {
                let listing = match pull(backend, PullPolicy::AllPromised).await {
                    Ok(listing) => listing,
                    Err(PasteboardError::RequestTimeout) => {
                        retry = Some(Instant::now() + LISTING_RETRY);
                        continue;
                    }
                    Err(error) => return Err(error),
                };

                if already_seen(listing.change.as_ref(), state.last_change.as_ref()) {
                    stale = false;
                    continue;
                }
                let Update { change, mut event, skipped } = read_clip(backend, listing, language).await?;
                let duplicate = already_seen(change.as_ref(), state.last_change.as_ref());
                if change.is_some() {
                    state.last_change = change;
                }
                // An abandoned pull leaves the reconnect marker available.
                if skipped {
                    continue;
                }
                let was_stale = std::mem::take(&mut stale);
                if duplicate {
                    continue;
                }
                if let Some(DeviceEvent::Clip { clip, while_disconnected }) = &mut event {
                    if state.is_echo(clip) {
                        continue;
                    }
                    *while_disconnected = was_stale;
                }
                if let Some(event) = event
                    && emit(event).is_break()
                {
                    return Ok(None);
                }
            }
            _ = sleep_until(retry.unwrap_or(next_poll)), if retry.is_some() => {
                retry = None;
                changed.notify_one();
            }
        }
    }
}

fn already_seen(current: Option<&PasteboardChange>, last: Option<&PasteboardChange>) -> bool {
    current.is_some() && current == last
}

#[cfg(test)]
mod tests;
