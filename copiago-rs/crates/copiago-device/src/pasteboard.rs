//! Read-only clipboard watching on a live CoreDevice tunnel.

use std::fmt;
use std::ops::ControlFlow;
use std::time::Duration;

use copiago_core::{
    clip::PasteboardChange,
    i18n::{Language, Message},
};
use tokio::{
    sync::Notify,
    time::{Instant, sleep_until, timeout},
};

use crate::{ConnState, DeviceEvent};

mod content;
mod file_offer;
mod rsd;

pub(crate) use rsd::{PASTEBOARD_SERVICE, watch_rsd};

use content::{Snapshot, Update, read_clip};

const NOTIFICATIONS: [&str; 3] = [
    "com.apple.pasteboard.notify.changed",
    "com.apple.springboard.lockstate",
    "com.apple.mobile.keybagd.lock_status",
];
const METADATA_TIMEOUT: Duration = Duration::from_secs(8);
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
    emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<(), PasteboardError> {
    let changed = Notify::new();
    // Keep the notification reader alive while pulls and timers run. Dropping an
    // in-progress read can leave the proxy between its length header and payload.
    tokio::select! {
        result = relay_changes(&changed, receive) => result,
        result = watch_updates(backend, &changed, wireless, language, emit) => result,
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
    mut emit: impl FnMut(DeviceEvent) -> ControlFlow<()>,
) -> Result<(), PasteboardError> {
    // Establish a baseline after subscribing. A first-time connection does not
    // deliver whatever happened to be on the phone before it connected.
    let mut last_change = pull(backend, PullPolicy::AllPromised).await?.change;
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
        return Ok(());
    }

    let mut next_poll = Instant::now() + POLL_INTERVAL;
    let mut retry = None;
    let mut failures = 0;
    loop {
        // A single worker owns the backend for both metadata and content requests.
        tokio::select! {
            _ = sleep_until(next_poll) => {
                match pull(backend, PullPolicy::AllPromised).await {
                    Ok(listing) => {
                        failures = 0;
                        if listing.change.is_some() && listing.change != last_change {
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

                if already_seen(listing.change.as_ref(), last_change.as_ref()) {
                    continue;
                }
                let Update { change, event } = read_clip(backend, listing, language).await?;
                if already_seen(change.as_ref(), last_change.as_ref()) {
                    continue;
                }
                if change.is_some() {
                    last_change = change;
                }
                if let Some(event) = event
                    && emit(event).is_break()
                {
                    return Ok(());
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
