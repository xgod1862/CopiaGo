//! Optional local diagnostics. Callers supply fixed event names, categories and measurements.

use std::{
    env, fmt,
    fs::OpenOptions,
    io::Write,
    os::windows::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::Instant,
};

use windows_sys::Win32::Storage::FileSystem::FILE_APPEND_DATA;

struct Log {
    path: PathBuf,
    start: Instant,
    write: Mutex<()>,
}

static DIAGNOSTIC: OnceLock<Option<Log>> = OnceLock::new();

pub fn event(details: fmt::Arguments<'_>) {
    let Some(log) = DIAGNOSTIC.get_or_init(|| {
        env::var_os("ICB_DIAGNOSTIC_FILE")
            .filter(|path| !path.is_empty())
            .map(|path| Log {
                path: path.into(),
                start: Instant::now(),
                write: Mutex::new(()),
            })
    }) else {
        return;
    };
    if let Ok(_write) = log.write.lock() {
        append(
            &log.path,
            format_args!("elapsed_ms={} {details}", log.start.elapsed().as_millis()),
        );
    }
}

pub fn install_crash_hook(data_dir: &Path, version: &'static str) {
    let path = data_dir.join("crash.log");
    std::panic::set_hook(Box::new(move |info| {
        // A panic message may include data. Record only the source-file name and position.
        if let Some(location) = info.location() {
            let file = Path::new(location.file())
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("unknown");
            append(
                &path,
                format_args!(
                    "version={version} file={file} line={} column={}",
                    location.line(),
                    location.column()
                ),
            );
        } else {
            append(&path, format_args!("version={version} file=unknown"));
        }
    }));
}

fn append(path: &Path, details: fmt::Arguments<'_>) {
    if let Ok(mut file) = OpenOptions::new()
        .create(true)
        .append(true)
        .access_mode(FILE_APPEND_DATA)
        .open(path)
    {
        let _ = file.write_all(format!("{details}\n").as_bytes());
    }
}
