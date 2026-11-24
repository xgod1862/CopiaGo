use std::{
    fmt::Write as _,
    fs::{self, OpenOptions},
    io::{self, Write as _},
    mem::MaybeUninit,
    path::Path,
    sync::Mutex,
    time::Instant,
};

use copiago_core::{ddi::DdiStage, paths};
use copiago_win::http::Error as HttpError;
use windows_sys::Win32::System::SystemInformation::GetLocalTime;

use super::{SetupError, StepError};
use crate::download::{DownloadError, DownloadStats, SourceError};

const LOG_NAME: &str = "developer-setup.log";
const ROLLED_LOG: &str = "developer-setup.log.1";
const MAX_LOG_BYTES: u64 = 256 * 1024;
static LOCK: Mutex<()> = Mutex::new(());

pub(super) fn finish<T>(
    data_root: &Path,
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
                    data_root,
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
        Err(StepError::DeveloperModeRequired) => return Err(SetupError::DeveloperModeRequired),
        Err(error) => error,
    };
    let timed_out = status == StepError::Timeout
        || status == StepError::Kind("ConnectionError<-ReadTimeoutError");
    let outcome = if timed_out { "timeout" } else { "failed" };
    let _ = record(
        data_root,
        stage,
        started.elapsed().as_secs_f64(),
        outcome,
        Some(status),
        stats,
    );
    Err(SetupError::Failed { stage, timed_out })
}

pub(super) fn record(
    data_root: &Path,
    stage: DdiStage,
    elapsed: f64,
    outcome: &str,
    error: Option<StepError>,
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
    if let Some(error) = error {
        let _ = write!(line, " error={}", error_label(error));
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

fn error_label(error: StepError) -> String {
    match error {
        StepError::Canceled => "DownloadCanceled".into(),
        StepError::DeveloperModeRequired => "DeveloperModeIsNotEnabledError".into(),
        StepError::Timeout => "TimeoutError".into(),
        StepError::Kind(kind) => kind.into(),
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
