//! Local diagnostics written to `diagnostic.log` in the data folder.
//!
//! Callers supply fixed event names, categories and measurements only. Never pass
//! clipboard content, file names, device names, serial numbers or paths: users copy
//! this log into public GitHub issues.

use std::{
    env, fmt,
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use windows_sys::Win32::Storage::FileSystem::FILE_APPEND_DATA;

/// Current log file name inside the data folder.
pub const LOG_FILE: &str = "diagnostic.log";
/// The previous log after rotation. Kept so a report still covers the last session.
pub const OLD_LOG_FILE: &str = "diagnostic.old.log";
/// Rotate once the current log exceeds this size, bounding disk use to about twice it.
const MAX_LOG_BYTES: u64 = 512 * 1024;

struct Log {
    path: PathBuf,
    start: Instant,
    /// Serializes writes and tracks the current file's size, so a long-running process
    /// rotates as it goes instead of only at startup.
    write: Mutex<u64>,
}

static DIAGNOSTIC: OnceLock<Log> = OnceLock::new();

/// Starts the log for this process. Events before this call are dropped.
///
/// A nonempty `ICB_DIAGNOSTIC_FILE` keeps its earlier meaning and redirects the log,
/// which development scripts rely on. Calling this more than once keeps the first log.
pub fn init(data_dir: &Path, version: &str) {
    let path = env::var_os("ICB_DIAGNOSTIC_FILE")
        .filter(|path| !path.is_empty())
        .map_or_else(|| data_dir.join(LOG_FILE), PathBuf::from);
    let log = DIAGNOSTIC.get_or_init(|| {
        let size = fs::metadata(&path).map_or(0, |metadata| metadata.len());
        Log {
            path,
            start: Instant::now(),
            write: Mutex::new(size),
        }
    });
    if let Ok(mut size) = log.write.lock() {
        let line = format!("{} event=log_opened version={version}", utc_now());
        write_line(&log.path, &mut size, MAX_LOG_BYTES, &line);
    }
}

/// Appends one line when the log is open. Never fails visibly: diagnostics must not
/// change application behavior.
pub fn event(details: fmt::Arguments<'_>) {
    let Some(log) = DIAGNOSTIC.get() else {
        return;
    };
    if let Ok(mut size) = log.write.lock() {
        let line = format!(
            "{} elapsed_ms={} {details}",
            utc_now(),
            log.start.elapsed().as_millis()
        );
        write_line(&log.path, &mut size, MAX_LOG_BYTES, &line);
    }
}

/// Appends a line, first rotating the file once it has reached `limit` bytes. Keeps disk
/// use at about twice the limit. `size` tracks the current file and must be guarded by
/// the caller's lock.
fn write_line(path: &Path, size: &mut u64, limit: u64, line: &str) {
    if *size >= limit {
        // Rename replaces an earlier rotated log on Windows.
        if fs::rename(path, old_path(path)).is_ok() {
            *size = 0;
        }
    }
    if append(path, format_args!("{line}")) {
        *size += line.len() as u64 + 1;
    }
}

/// The open log file, if [`init`] ran.
pub fn log_path() -> Option<&'static Path> {
    DIAGNOSTIC.get().map(|log| log.path.as_path())
}

/// The last `max_lines` lines across the rotated and current log, oldest first.
///
/// Reads at most `max_bytes` from the end of each file so a report stays small enough
/// to paste into an issue.
pub fn recent_lines(max_lines: usize, max_bytes: u64) -> Vec<String> {
    let Some(log) = DIAGNOSTIC.get() else {
        return Vec::new();
    };
    let _write = log.write.lock();
    let mut lines = Vec::new();
    for path in [old_path(&log.path), log.path.clone()] {
        lines.extend(read_tail(&path, max_bytes));
    }
    let skip = lines.len().saturating_sub(max_lines);
    lines.split_off(skip)
}

pub fn install_crash_hook(data_dir: &Path, version: &'static str) {
    let path = data_dir.join("crash.log");
    std::panic::set_hook(Box::new(move |info| {
        // A panic message may include data. Record only the source-file name and position.
        let (file, line, column) = info.location().map_or(("unknown", 0, 0), |location| {
            let file = Path::new(location.file())
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown");
            (file, location.line(), location.column())
        });
        let _ = append(
            &path,
            format_args!("version={version} file={file} line={line} column={column}"),
        );
        // Bypass the write lock: the panicking thread may already hold it.
        if let Some(log) = DIAGNOSTIC.get() {
            let _ = append(
                &log.path,
                format_args!(
                    "{} event=panic file={file} line={line} column={column}",
                    utc_now()
                ),
            );
        }
    }));
}

fn old_path(path: &Path) -> PathBuf {
    path.with_file_name(OLD_LOG_FILE)
}

fn read_tail(path: &Path, max_bytes: u64) -> Vec<String> {
    let Ok(mut file) = fs::File::open(path) else {
        return Vec::new();
    };
    let length = file.metadata().map_or(0, |metadata| metadata.len());
    let start = length.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    // A tail that starts mid-file usually begins with a partial line.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    lines
}

fn append(path: &Path, details: fmt::Arguments<'_>) -> bool {
    OpenOptions::new()
        .create(true)
        .append(true)
        .access_mode(FILE_APPEND_DATA)
        .open(path)
        .and_then(|mut file| file.write_all(format!("{details}\n").as_bytes()))
        .is_ok()
}

/// UTC wall-clock time, so lines from different users line up with their reports.
fn utc_now() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    format_utc(seconds)
}

/// Formats Unix seconds as `YYYY-MM-DDTHH:MM:SSZ` without a date library.
fn format_utc(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Howard Hinnant's days-to-civil conversion, valid for the proleptic Gregorian calendar.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_utc_timestamps() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_795_574_461), "2026-11-25T02:41:01Z");
    }

    #[test]
    fn rotates_while_writing_and_keeps_one_old_log() {
        let dir = std::env::temp_dir().join(format!("copiago-rotate-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE);
        let mut size = 0;
        for index in 0..12 {
            write_line(&path, &mut size, 20, &format!("line {index:02}"));
        }
        // Each line is 7 bytes plus a newline, so every file holds three lines.
        assert!(fs::metadata(&path).unwrap().len() <= 27);
        assert_eq!(
            fs::read_to_string(old_path(&path)).unwrap(),
            "line 06\nline 07\nline 08\n"
        );
        assert_eq!(size, fs::metadata(&path).unwrap().len());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tail_drops_a_partial_first_line() {
        let dir = std::env::temp_dir().join(format!("copiago-diag-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE);
        fs::write(&path, "first line\nsecond\nthird\n").unwrap();
        assert_eq!(read_tail(&path, 1_000), ["first line", "second", "third"]);
        assert_eq!(read_tail(&path, 15), ["second", "third"]);
        fs::remove_dir_all(&dir).unwrap();
    }
}
