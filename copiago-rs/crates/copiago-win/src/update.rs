//! One-shot GitHub release checks for the Windows host.

mod version;

use std::io;
use std::ptr::{null, null_mut};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, TryRecvError},
};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

use serde_json::Value;
use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
use windows_sys::w;

use crate::http::{Client, RequestLimits};

pub const STARTUP_DELAY: Duration = Duration::from_secs(15);
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const LATEST_URL: &str = "https://api.github.com/repos/xgod1862/CopiaGo/releases/latest";
const RELEASE_PAGE_PREFIX: &str = "https://github.com/xgod1862/CopiaGo/releases/";
const HEADERS: &[(&str, &str)] = &[("Accept", "application/vnd.github+json")];

/// A verified release for this repository.
#[derive(Debug, PartialEq, Eq)]
pub struct Release {
    version: String,
    page: String,
}

impl Release {
    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn page(&self) -> &str {
        &self.page
    }

    /// Opens the verified page in the default browser. Call on the COM-initialized UI thread.
    pub fn open(&self) -> io::Result<()> {
        let page: Vec<_> = self.page.encode_utf16().chain([0]).collect();
        // SAFETY: The URL is validated before storing it and both string arguments are
        // NUL-terminated and live through this call. Other arguments are optional.
        let status = unsafe {
            ShellExecuteW(
                null_mut(),
                w!("open"),
                page.as_ptr(),
                null(),
                null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if status > 32 {
            Ok(())
        } else {
            Err(io::Error::other(format!("ShellExecuteW returned {status}")))
        }
    }
}

/// Own on the UI thread. Dropping the check cancels scheduled or in-flight work.
#[must_use]
pub struct UpdateCheck {
    current: String,
    enabled: bool,
    wake: Arc<dyn Fn() + Send + Sync>,
    pending: Option<Pending>,
    release: Option<Release>,
}

impl UpdateCheck {
    /// Schedules a single check after 15 seconds when enabled. The worker invokes the callback
    /// on completion so the host can read [Self::release] on the UI thread.
    /// The callback should post a message or send an event and return promptly.
    pub fn start(current: &str, enabled: bool, wake: impl Fn() + Send + Sync + 'static) -> Self {
        Self::start_after(current, enabled, wake, STARTUP_DELAY, fetch_latest)
    }

    /// After saving the setting, call this and read [Self::release] to refresh the UI.
    /// Disabling cancels pending work and clears the notice. Enabling starts a check immediately.
    pub fn set_enabled(&mut self, enabled: bool) {
        self.change_enabled(enabled, fetch_latest);
    }

    /// Reads the current notice without blocking. Read again on click before calling
    /// [Release::open], so an old UI event cannot open a page after the setting was disabled.
    pub fn release(&mut self) -> Option<&Release> {
        if let Some(pending) = &self.pending {
            match pending.result.try_recv() {
                Ok(release) => {
                    self.release = release;
                    self.pending = None;
                }
                Err(TryRecvError::Disconnected) => self.pending = None,
                Err(TryRecvError::Empty) => {}
            }
        }
        self.release.as_ref()
    }

    fn start_after(
        current: &str,
        enabled: bool,
        wake: impl Fn() + Send + Sync + 'static,
        delay: Duration,
        fetch: impl FnOnce(&str, &AtomicBool) -> Option<Release> + Send + 'static,
    ) -> Self {
        let mut check = Self {
            current: current.to_owned(),
            enabled,
            wake: Arc::new(wake),
            pending: None,
            release: None,
        };
        if enabled {
            check.schedule(delay, fetch);
        }
        check
    }

    fn change_enabled(
        &mut self,
        enabled: bool,
        fetch: impl FnOnce(&str, &AtomicBool) -> Option<Release> + Send + 'static,
    ) {
        if self.enabled == enabled {
            return;
        }
        self.enabled = enabled;
        if enabled {
            self.schedule(Duration::ZERO, fetch);
        } else {
            self.pending = None;
            self.release = None;
        }
    }

    fn schedule(
        &mut self,
        delay: Duration,
        fetch: impl FnOnce(&str, &AtomicBool) -> Option<Release> + Send + 'static,
    ) {
        let current = self.current.clone();
        let start_at = Instant::now() + delay;
        let wake = Arc::clone(&self.wake);
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&cancel);
        let (send, result) = mpsc::channel();

        if let Ok(worker) = thread::Builder::new()
            .name("copiago-update".into())
            .spawn(move || {
                if wait_for_start(start_at, &stop) {
                    let release = fetch(&current, &stop);
                    if !stop.load(Ordering::Relaxed) && send.send(release).is_ok() {
                        wake();
                    }
                }
            })
        {
            self.pending = Some(Pending {
                cancel,
                worker: worker.thread().clone(),
                result,
            });
        }
    }
}

struct Pending {
    cancel: Arc<AtomicBool>,
    worker: Thread,
    result: Receiver<Option<Release>>,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
        self.worker.unpark();
    }
}

fn wait_for_start(start_at: Instant, cancel: &AtomicBool) -> bool {
    while !cancel.load(Ordering::Relaxed) {
        let remaining = start_at.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return true;
        }
        thread::park_timeout(remaining);
    }
    false
}

fn fetch_latest(current: &str, cancel: &AtomicBool) -> Option<Release> {
    fetch_release(current, LATEST_URL, TIMEOUT, cancel)
}

fn fetch_release(
    current: &str,
    url: &str,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Option<Release> {
    if current.contains(['\r', '\n', '\0']) {
        return None;
    }
    let limits = RequestLimits {
        deadline: Instant::now() + timeout,
        connect_timeout: timeout,
        idle_timeout: timeout,
        cancel,
    };
    limits.check().ok()?;
    let client = Client::new(&format!("CopiaGo/{current}")).ok()?;
    let mut response = client.get_without_redirects(url, HEADERS, limits).ok()?;
    if response.status() != 200 {
        return None;
    }
    let mut body = Vec::new();
    loop {
        let chunk = response.read_chunk().ok()?;
        if chunk.is_empty() {
            break;
        }
        if chunk.len() > MAX_RESPONSE_BYTES - body.len() {
            return None;
        }
        body.extend_from_slice(chunk);
    }
    let release = newer_release(current, &serde_json::from_slice(&body).ok()?);
    limits.check().ok()?;
    release
}

fn newer_release(current: &str, payload: &Value) -> Option<Release> {
    let release = payload.as_object()?;
    if ["draft", "prerelease"]
        .into_iter()
        .any(|flag| release.get(flag).is_some_and(truthy))
    {
        return None;
    }
    let latest = version::parse(release.get("tag_name")?.as_str()?)?;
    if latest <= version::parse(current)? {
        return None;
    }
    let page = release.get("html_url")?.as_str()?;
    if !allowed_release_page(page) {
        return None;
    }
    let [(_, major), (_, minor), (_, patch)] = latest;
    Some(Release {
        version: format!("{major}.{minor}.{patch}"),
        page: page.to_owned(),
    })
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64() != Some(0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn allowed_release_page(page: &str) -> bool {
    let Some(suffix) = page.strip_prefix(RELEASE_PAGE_PREFIX) else {
        return false;
    };
    if page.contains(['\\', '"']) || page.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }

    // Reject path separators and dot segments that a browser or server could normalize.
    let path = suffix
        .split(['?', '#'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let first = path.split('/').next().unwrap_or_default();
    if first == "download" || first.contains('%') || path.starts_with('/') {
        return false;
    }
    !path.split('/').any(|segment| {
        matches!(segment.replace("%2e", ".").as_str(), "." | "..")
            || ["%2f", "%5c", "%25", "%00"]
                .iter()
                .any(|escape| segment.contains(escape))
    })
}

#[cfg(test)]
mod tests;
