//! Clipboard synchronization on a live CoreDevice tunnel.

use std::fmt;
use std::ops::ControlFlow;
use std::pin::pin;
use std::time::Duration;

use copiago_core::{
    clip::{Clip, FileOffer, PasteboardChange},
    files::ReceivedFile,
    i18n::{Language, Message},
};
use futures_util::future::OptionFuture;
use tokio::{
    sync::{Notify, mpsc},
    time::{Instant, sleep_until, timeout},
};

use crate::{ConnState, DeviceCommand, DeviceEvent, FileProgress};

mod content;
mod file_offer;
mod file_receive;
pub(crate) mod file_transfer;
mod rsd;

#[cfg(test)]
pub(crate) use rsd::verify_push_round_trip;
pub(crate) use rsd::{PASTEBOARD_SERVICE, WatchOptions, read_rsd_types, watch_rsd};

use content::{PNG_UTI, Snapshot, Update, read_clip};
pub use file_transfer::FileTransferError;

const NOTIFICATIONS: [&str; 3] = [
    "com.apple.pasteboard.notify.changed",
    "com.apple.springboard.lockstate",
    "com.apple.mobile.keybagd.lock_status",
];
const METADATA_TIMEOUT: Duration = Duration::from_secs(8);
// Matches copiago_win::image::MAX_IMAGE_BYTES for the optional RESOLVE path.
const MAX_ALTERNATE_IMAGE_BYTES: usize = 64 * 1024 * 1024;
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
    /// Another handle to the same pasteboard whose requests may run concurrently
    /// with this one's. Reading, writing and file downloads each use their own.
    fn fork(&self) -> Self
    where
        Self: Sized;
    async fn pull(&mut self, policy: PullPolicy) -> Result<Snapshot, PasteboardError>;
    async fn alternate_listing(&mut self) -> Result<Snapshot, PasteboardError>;
    async fn resolve_alternate(
        &mut self,
        item_index: usize,
        uti: &str,
    ) -> Result<Vec<u8>, PasteboardError>;
    async fn set(&mut self, item: SetItem) -> Result<Option<PasteboardChange>, PasteboardError>;
    async fn fetch_file(
        &mut self,
        offer: FileOffer,
        progress: &mut dyn FnMut(FileProgress),
    ) -> Result<ReceivedFile, FileTransferError>;
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

    /// A whole push on one backend. The watch loop runs the same steps on its
    /// writing lane; tests use this for push behavior alone.
    #[cfg(test)]
    async fn push(
        &mut self,
        backend: &mut impl PasteboardBackend,
        clip: Clip,
    ) -> Result<(), PasteboardError> {
        let item = self.begin_push(clip)?;
        let change = set_with_timeout(backend, item).await?;
        self.finish_push(change);
        Ok(())
    }

    /// Validates a push and records its fingerprint before the SET starts, so a pull
    /// that reads the item back while the SET is still in flight counts as an echo.
    fn begin_push(&mut self, clip: Clip) -> Result<SetItem, PasteboardError> {
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
        Ok(item)
    }

    fn finish_push(&mut self, change: Option<PasteboardChange>) {
        if change.is_some() {
            self.last_change = change;
        }
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
    language: impl Fn() -> Language,
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
    language: impl Fn() -> Language,
    state: &mut PasteboardState,
    mut next_command: impl AsyncFnMut() -> Option<DeviceCommand>,
    mut emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<Option<DeviceCommand>, PasteboardError> {
    let initial = pull(backend, PullPolicy::AllPromised).await?.change;
    copiago_core::diagnostic::event(format_args!("monitor_ready"));
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

    // Three lanes that never wait for one another: reading the phone (polls and
    // change notifications, in order), writing to it (pushes, in order), and file
    // downloads. Before, one slow image read or a large file held up every push.
    // A lane's backend is `None` while its request runs and comes back with the result.
    let mut reader = Some(backend.fork());
    let mut writer = Some(backend.fork());
    let mut downloader = Some(backend.fork());
    let mut reading = pin!(OptionFuture::default());
    let mut writing = pin!(OptionFuture::default());
    let mut fetching = pin!(OptionFuture::default());
    let (progress_sender, mut progress) = mpsc::unbounded_channel();
    // A command taken from the queue whose lane is busy. No more are taken until it
    // starts, so at most the running SET and this one sit outside the queue's
    // memory budget, and a FetchOffer waits behind at most one push.
    let mut deferred: Option<DeviceCommand> = None;

    // Counts pushes that may have replaced the phone's clipboard. A read that
    // started before one of them finished returns content the user has already
    // overwritten from the PC; writing it back would undo that copy.
    let mut writes = 0_u64;
    let mut read_started_at_write = writes;

    let mut next_poll = Instant::now() + POLL_INTERVAL;
    let mut retry = None;
    let mut failures = 0;
    loop {
        tokio::select! {
            command = next_command(), if deferred.is_none() => match command {
                Some(command @ (DeviceCommand::Push(..) | DeviceCommand::FetchOffer(..))) => {
                    deferred = Some(command);
                }
                // The connection manager owns lifecycle commands.
                other => return Ok(other),
            },
            Some((lane, id, result)) = writing.as_mut(), if writer.is_none() => {
                writing.set(None.into());
                writer = Some(lane);
                let result: Result<Option<PasteboardChange>, PasteboardError> = result;
                // A timed-out SET may still have reached the phone.
                if matches!(result, Ok(_) | Err(PasteboardError::RequestTimeout)) {
                    writes += 1;
                }
                let result = result.map(|change| state.finish_push(change));
                if emit(DeviceEvent::PushResult(id, result)).is_break() {
                    return Ok(None);
                }
            }
            Some((lane, id, result)) = fetching.as_mut(), if downloader.is_none() => {
                fetching.set(None.into());
                downloader = Some(lane);
                // Deliver queued progress before the terminal result.
                while let Ok((progress_id, report)) = progress.try_recv() {
                    if emit(DeviceEvent::FileProgress(progress_id, report)).is_break() {
                        return Ok(None);
                    }
                }
                if emit(DeviceEvent::FileResult(id, result)).is_break() {
                    return Ok(None);
                }
            }
            Some((id, report)) = progress.recv() => {
                if emit(DeviceEvent::FileProgress(id, report)).is_break() {
                    return Ok(None);
                }
            }
            Some((lane, outcome)) = reading.as_mut(), if reader.is_none() => {
                reading.set(None.into());
                reader = Some(lane);
                match outcome {
                    ReadOutcome::Polled(Ok(listing)) => {
                        failures = 0;
                        if listing.change.is_some() && listing.change != state.last_change {
                            changed.notify_one();
                        }
                        next_poll = Instant::now() + POLL_INTERVAL;
                    }
                    ReadOutcome::Polled(Err(_)) => {
                        failures += 1;
                        if failures == MAX_POLL_FAILURES {
                            return Err(PasteboardError::MetadataUnresponsive);
                        }
                        next_poll = Instant::now() + POLL_INTERVAL;
                    }
                    ReadOutcome::ListingTimedOut => {
                        copiago_core::diagnostic::event(format_args!("pull timeout types=unknown"));
                        retry = Some(Instant::now() + LISTING_RETRY);
                    }
                    ReadOutcome::Seen => stale = false,
                    ReadOutcome::Read(update) => {
                        let Update { change, mut event, skipped } = update?;
                        if read_started_at_write != writes {
                            // Superseded by a push: keep the push's change as the
                            // latest, and let the next poll pick up anything newer.
                            copiago_core::diagnostic::event(format_args!("pull_superseded"));
                            continue;
                        }
                        // Compared with the state as it is now: a push may have
                        // finished while this read ran.
                        let duplicate = already_seen(change.as_ref(), state.last_change.as_ref());
                        if change.is_some() {
                            state.last_change = change;
                        }
                        // An abandoned pull leaves the reconnect marker available. Its
                        // failure notice, if any, still reaches the user.
                        if skipped {
                            if let Some(event) = event
                                && emit(event).is_break()
                            {
                                return Ok(None);
                            }
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
                }
            }
            _ = sleep_until(next_poll), if reader.is_some() => {
                let lane = reader.take().unwrap();
                reading.set(Some(read_lane(lane, ReadJob::Poll, &language)).into());
            }
            _ = changed.notified(), if retry.is_none() && reader.is_some() => {
                let lane = reader.take().unwrap();
                let job = ReadJob::Changed(state.last_change.clone());
                read_started_at_write = writes;
                reading.set(Some(read_lane(lane, job, &language)).into());
            }
            _ = sleep_until(retry.unwrap_or(next_poll)), if retry.is_some() => {
                retry = None;
                changed.notify_one();
            }
        }

        match deferred.take() {
            Some(DeviceCommand::Push(clip, id)) if writer.is_some() => match state.begin_push(clip)
            {
                Ok(item) => {
                    let lane = writer.take().unwrap();
                    writing.set(Some(write_lane(lane, item, id)).into());
                }
                Err(error) => {
                    if emit(DeviceEvent::PushResult(id, Err(error))).is_break() {
                        return Ok(None);
                    }
                }
            },
            Some(DeviceCommand::FetchOffer(offer, id)) if downloader.is_some() => {
                let lane = downloader.take().unwrap();
                fetching.set(Some(fetch_lane(lane, offer, id, progress_sender.clone())).into());
            }
            other => deferred = other,
        }
    }
}

enum ReadJob {
    Poll,
    /// Handle a change notification. Carries the last change seen when it started,
    /// so an already-seen listing skips the content request.
    Changed(Option<PasteboardChange>),
}

enum ReadOutcome {
    Polled(Result<Snapshot, PasteboardError>),
    ListingTimedOut,
    Seen,
    Read(Result<Update, PasteboardError>),
}

async fn read_lane<B: PasteboardBackend>(
    mut lane: B,
    job: ReadJob,
    language: &impl Fn() -> Language,
) -> (B, ReadOutcome) {
    let outcome = match job {
        ReadJob::Poll => ReadOutcome::Polled(pull(&mut lane, PullPolicy::AllPromised).await),
        ReadJob::Changed(last) => match pull(&mut lane, PullPolicy::AllPromised).await {
            Err(PasteboardError::RequestTimeout) => ReadOutcome::ListingTimedOut,
            Err(error) => ReadOutcome::Read(Err(error)),
            Ok(listing) if already_seen(listing.change.as_ref(), last.as_ref()) => {
                ReadOutcome::Seen
            }
            Ok(listing) => ReadOutcome::Read(read_clip(&mut lane, listing, language).await),
        },
    };
    (lane, outcome)
}

async fn set_with_timeout(
    backend: &mut impl PasteboardBackend,
    item: SetItem,
) -> Result<Option<PasteboardChange>, PasteboardError> {
    timeout(SET_TIMEOUT, backend.set(item))
        .await
        .map_err(|_| PasteboardError::RequestTimeout)?
}

async fn write_lane<B: PasteboardBackend>(
    mut lane: B,
    item: SetItem,
    id: Option<u64>,
) -> (
    B,
    Option<u64>,
    Result<Option<PasteboardChange>, PasteboardError>,
) {
    let result = set_with_timeout(&mut lane, item).await;
    (lane, id, result)
}

async fn fetch_lane<B: PasteboardBackend>(
    mut lane: B,
    offer: FileOffer,
    id: u64,
    progress: mpsc::UnboundedSender<(u64, FileProgress)>,
) -> (B, u64, Result<ReceivedFile, FileTransferError>) {
    let result = lane
        .fetch_file(offer, &mut |report| {
            let _ = progress.send((id, report));
        })
        .await;
    (lane, id, result)
}

fn already_seen(current: Option<&PasteboardChange>, last: Option<&PasteboardChange>) -> bool {
    current.is_some() && current == last
}

#[cfg(test)]
mod tests;
