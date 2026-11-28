use std::{
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Write as _},
    mem::MaybeUninit,
    path::Path,
    sync::Mutex,
    time::Instant,
};

use copiago_core::{
    ddi::{DdiCause, DdiStage},
    diagnostic, paths,
};
use copiago_win::http::Error as HttpError;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

use super::{SetupError, StepError};
use crate::download::{DownloadError, DownloadStats, SourceError};

const LOG_NAME: &str = "developer-setup.log";
const ROLLED_LOG: &str = "developer-setup.log.1";
const MAX_LOG_BYTES: u64 = 256 * 1024;
static LOCK: Mutex<()> = Mutex::new(());

/// Per-attempt state so a deadline can report the operation it interrupted.
pub(super) struct SetupLog<'a> {
    pub data_root: &'a Path,
    pub step: &'static str,
}

impl<'a> SetupLog<'a> {
    pub(super) fn new(data_root: &'a Path) -> Self {
        Self {
            data_root,
            step: "ConnectImageMounter",
        }
    }

    pub(super) fn finish<T>(
        &self,
        stage: DdiStage,
        started: Instant,
        result: Result<T, StepError>,
        stats: Option<&DownloadStats>,
        log_success: bool,
    ) -> Result<T, SetupError> {
        let status = match result {
            Ok(value) => {
                if log_success {
                    let _ = record(
                        self.data_root,
                        stage,
                        started.elapsed().as_secs_f64(),
                        "ok",
                        None,
                        stats,
                    );
                }
                return Ok(value);
            }
            Err(StepError::Canceled) => return Err(SetupError::Canceled),
            Err(error) => error,
        };
        let cause = cause(stage, self.step, &status);
        let detail = match &status {
            StepError::Device { detail, .. } => format!(" detail=\"{detail}\""),
            _ => String::new(),
        };
        diagnostic::event(format_args!(
            "event=setup_step_failed stage={stage:?} step={} error={} cause={cause:?} ms={}{detail}",
            self.step,
            error_label(&status),
            started.elapsed().as_millis(),
        ));
        if status == StepError::DeveloperModeRequired {
            return Err(SetupError::DeveloperModeRequired);
        }
        let timed_out = status == StepError::Timeout
            || status == StepError::Kind("ConnectionError<-ReadTimeoutError");
        let outcome = if timed_out { "timeout" } else { "failed" };
        let _ = record(
            self.data_root,
            stage,
            started.elapsed().as_secs_f64(),
            outcome,
            Some((self.step, status)),
            stats,
        );
        Err(SetupError::Failed {
            stage,
            timed_out,
            cause,
        })
    }
}

/// Records the model and exact iOS build, so a report can be matched to a test device.
/// Model codes such as `iPhone17,2` and build numbers are shared by every phone of a kind.
pub(super) fn context(rsd: &idevice::rsd::RsdHandshake, kind: crate::download::ImageKind) {
    let field = |key: &str| {
        rsd.properties
            .get(key)
            .and_then(plist::Value::as_string)
            .filter(|value| {
                value.len() <= 32
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b".,_-".contains(&b))
            })
            .unwrap_or("-")
    };
    diagnostic::event(format_args!(
        "event=setup_context kind={kind:?} product={} ios={} build={}",
        field("ProductType"),
        field("OSVersion"),
        field("BuildVersion"),
    ));
}

/// Steps that only touch image files cached on this PC.
const LOCAL_STEPS: &[&str] = &[
    "ReadDeveloperImage",
    "ReadTrustCache",
    "ReadBuildManifest",
    "ParseBuildManifest",
    "LoadCryptexAssets",
];

/// Maps a failed step to what the user can do about it. Download failures keep their
/// stage-level hints, so only storage problems are singled out there.
pub(super) fn cause(stage: DdiStage, step: &str, error: &StepError) -> DdiCause {
    use StepError::{Device, HttpStatus, Kind};
    let storage = matches!(
        error,
        Kind("FileNotFoundError" | "PermissionError" | "OSError" | "InvalidFileException")
    );
    match stage {
        DdiStage::Download if storage => DdiCause::LocalFiles,
        DdiStage::Download => DdiCause::Unknown,
        DdiStage::Sign => match error {
            Kind("TSSError") => DdiCause::SigningRejected,
            HttpStatus(_) => DdiCause::SigningServer,
            Kind("SSLError") => DdiCause::SecureConnection,
            Kind("ConnectionError" | "ConnectionRefusedError" | "OSError" | "HTTPError") => {
                DdiCause::Network
            }
            _ => DdiCause::Unknown,
        },
        DdiStage::Install if LOCAL_STEPS.contains(&step) => DdiCause::LocalFiles,
        DdiStage::Install => match error {
            Kind("PasswordRequiredError") => DdiCause::DeviceLocked,
            Kind("InvalidServiceError") => DdiCause::ServiceUnavailable,
            Kind(
                "NoSuchBuildIdentityError"
                | "DeviceFeatureNotSupportedError"
                | "MissingPersonalizationIdentifier",
            ) => DdiCause::Unsupported,
            Kind("ConnectionRefusedError" | "OSError" | "ConnectionError") => {
                DdiCause::Disconnected
            }
            Device { .. } | Kind("CryptexdError") => DdiCause::DeviceRejected,
            _ => DdiCause::Unknown,
        },
    }
}

const MAX_DETAIL: usize = 200;

/// Keeps the wording of a device error while dropping anything that could identify the
/// phone or its owner: identifier-like words, quoted text, paths, addresses and non-ASCII.
/// New iOS releases tend to change this wording, so it is the most useful clue in a report.
pub(super) fn scrub(detail: &str) -> String {
    let mut out = String::new();
    for token in detail.split_whitespace() {
        if out.len() >= MAX_DETAIL {
            out.push_str(" ...");
            break;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        if token.contains(['/', '\\']) {
            out.push_str("<path>");
        } else if token.contains('@') {
            out.push_str("<addr>");
        } else if !token.is_ascii() || token.contains(['"', '\'', '`', '=']) {
            out.push_str("<text>");
        } else {
            scrub_words(token, &mut out);
        }
    }
    out
}

/// Replaces words that look like UDIDs, ECIDs, serials or hashes with `#`.
fn scrub_words(token: &str, out: &mut String) {
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        let digits = word.bytes().filter(u8::is_ascii_digit).count();
        if digits >= 4 || (digits > 0 && word.len() >= 8) {
            out.push('#');
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in token.chars() {
        if c.is_ascii_alphanumeric() {
            word.push(c);
        } else {
            flush(&mut word, out);
            out.push(c);
        }
    }
    flush(&mut word, out);
}

/// The enum variant of an error, without its payload.
pub(super) fn variant_name(error: &impl std::fmt::Debug) -> String {
    format!("{error:?}")
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(48)
        .collect()
}

pub(super) fn record(
    data_root: &Path,
    stage: DdiStage,
    elapsed: f64,
    outcome: &str,
    error: Option<(&'static str, StepError)>,
    stats: Option<&DownloadStats>,
) -> io::Result<()> {
    let _guard = LOCK
        .lock()
        .map_err(|_| io::Error::other("setup log lock poisoned"))?;
    paths::ensure_data_dir(data_root)?;
    let path = data_root.join(LOG_NAME);
    if path
        .metadata()
        .is_ok_and(|metadata| metadata.len() >= MAX_LOG_BYTES)
    {
        fs::rename(&path, data_root.join(ROLLED_LOG))?;
    }
    let stage = match stage {
        DdiStage::Download => "download",
        DdiStage::Sign => "sign",
        DdiStage::Install => "install",
    };
    let mut line = format!(
        "{} {stage} {outcome} elapsed={elapsed:.1}s",
        local_timestamp()
    );
    if let Some((_, cause)) = &error {
        let _ = write!(line, " error={}", error_label(cause));
    }
    if let Some(stats) = stats.filter(|stats| stats.count != 0 || stats.fetched != 0) {
        let speed = if elapsed > 0.0 {
            stats.fetched as f64 / elapsed / 1024.0
        } else {
            0.0
        };
        let http = stats
            .http_status
            .map_or_else(|| "None".to_owned(), |code| code.to_string());
        let _ = write!(
            line,
            " file={} part={}/{} received={} total={} resumed={} fetched={} restarts={} http={} speed={speed:.0}KB/s source={}",
            stats.file,
            stats.index,
            stats.count,
            stats.received,
            stats.total,
            stats.resumed,
            stats.fetched,
            stats.restarts,
            http,
            stats.source.unwrap_or("-"),
        );
        let recent = &stats.failures[stats.failures.len().saturating_sub(10)..];
        for (i, failure) in recent.iter().enumerate() {
            let prefix = if i == 0 { " failures=" } else { "," };
            let label = match failure.reason {
                SourceError::Integrity { resumed: true } => "IntegrityError(resumed)",
                SourceError::HttpStatus(_) => "HTTPError",
                SourceError::Transport(_)
                | SourceError::Incomplete
                | SourceError::UnexpectedRange => "ConnectionError",
                SourceError::Integrity { resumed: false } => "IntegrityError",
            };
            let _ = write!(line, "{prefix}{}:{label}", failure.source);
        }
    }
    if let Some((step, _)) = error {
        let _ = write!(line, " step={step}");
    }
    // Append one complete Python-compatible UTF-8 record.
    line.push_str("\r\n");
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(line.as_bytes())
}

pub(super) fn download_error(error: DownloadError) -> StepError {
    match error {
        DownloadError::Canceled => StepError::Canceled,
        DownloadError::Io(error) => error.into(),
        DownloadError::Timeout | DownloadError::Transport(HttpError::Deadline) => {
            StepError::Timeout
        }
        DownloadError::Transport(HttpError::Timeout) => {
            StepError::Kind("ConnectionError<-ReadTimeoutError")
        }
        DownloadError::Transport(_) => StepError::Kind("ConnectionError"),
        DownloadError::SourcesFailed { last, .. } => match last.reason {
            SourceError::HttpStatus(status) => StepError::HttpStatus(status),
            SourceError::Transport(HttpError::Timeout) => {
                StepError::Kind("ConnectionError<-ReadTimeoutError")
            }
            SourceError::Transport(HttpError::Deadline) => StepError::Timeout,
            SourceError::Integrity { .. } => StepError::Kind("IntegrityError"),
            _ => StepError::Kind("ConnectionError"),
        },
    }
}

fn error_label(error: &StepError) -> String {
    match error {
        StepError::Canceled => "DownloadCanceled".into(),
        StepError::DeveloperModeRequired => "DeveloperModeIsNotEnabledError".into(),
        StepError::Timeout => "TimeoutError".into(),
        StepError::Kind(kind) | StepError::Device { kind, .. } => (*kind).into(),
        StepError::HttpStatus(status) => format!("HTTPError[{status}]"),
    }
}

fn local_timestamp() -> String {
    let mut local = MaybeUninit::uninit();
    // SAFETY: GetLocalTime initializes the supplied, writable SYSTEMTIME.
    let local = unsafe {
        GetLocalTime(local.as_mut_ptr());
        local.assume_init()
    };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute, local.wSecond,
    )
}
