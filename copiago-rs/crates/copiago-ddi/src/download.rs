//! Download and cache the Python release's pinned developer images.

mod pinned;
#[cfg(test)]
mod tests;

pub use pinned::{DDI_BUILD, DDI_REF, DDI_REPO};

use std::error;
use std::fmt;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::{Duration, Instant};

use copiago_core::{
    atomic,
    ddi::Progress,
    paths::{self, DEVICE_DATA_DIR},
};
use copiago_win::http::{Client, Error as HttpError, RequestLimits};
use sha2::{Digest, Sha256};

const MANIFEST: &str = "BuildManifest.plist";
const SOURCE_FILE: &str = "developer-image-source";
const REF_FILE: &str = ".ref";
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const STALL_TIMEOUT: Duration = Duration::from_secs(30);
const LOCK_POLL: Duration = Duration::from_millis(50);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageKind {
    Cryptex,
    Personalized,
}

impl ImageKind {
    fn package(self) -> &'static Package {
        match self {
            Self::Cryptex => &pinned::CRYPTEX,
            Self::Personalized => &pinned::PERSONALIZED,
        }
    }
}

/// One-based file index and that file's progress. A source switch keeps the last byte count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DownloadProgress {
    pub file: &'static str,
    pub index: usize,
    pub count: usize,
    pub progress: Progress,
    pub switching_source: bool,
}

/// Download counters for developer setup diagnostics.
#[derive(Debug, Default)]
pub struct DownloadStats {
    pub file: &'static str,
    pub index: usize,
    pub count: usize,
    pub received: u64,
    pub total: u64,
    pub resumed: u64,
    pub fetched: u64,
    pub restarts: u32,
    pub http_status: Option<u16>,
    /// The most recent source that delivered bytes, including during an interrupted attempt.
    pub source: Option<&'static str>,
    pub failures: Vec<SourceFailure>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceFailure {
    pub source: &'static str,
    pub reason: SourceError,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceError {
    Transport(HttpError),
    HttpStatus(u16),
    Incomplete,
    UnexpectedRange,
    Integrity { resumed: bool },
}

#[derive(Debug)]
pub enum DownloadError {
    Canceled,
    Timeout,
    Io(io::Error),
    Transport(HttpError),
    SourcesFailed {
        file: &'static str,
        last: SourceFailure,
    },
}

impl DownloadError {
    pub fn is_timeout(&self) -> bool {
        matches!(
            self,
            Self::Timeout
                | Self::Transport(HttpError::Timeout)
                | Self::SourcesFailed {
                    last: SourceFailure {
                        reason: SourceError::Transport(HttpError::Timeout),
                        ..
                    },
                    ..
                }
        )
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Canceled => f.write_str("developer image download canceled"),
            Self::Timeout => f.write_str("developer image download deadline expired"),
            Self::Io(error) => write!(f, "developer image storage: {error}"),
            Self::Transport(error) => write!(f, "developer image HTTP session: {error}"),
            Self::SourcesFailed { file, last } => {
                write!(f, "all sources failed for {file} ({last:?})")
            }
        }
    }
}

impl error::Error for DownloadError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Transport(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for DownloadError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<HttpError> for DownloadError {
    fn from(error: HttpError) -> Self {
        match error {
            HttpError::Canceled => Self::Canceled,
            HttpError::Deadline => Self::Timeout,
            error => Self::Transport(error),
        }
    }
}

/// Runs on a worker thread. Cancellation keeps the verified files and resumable parts in staging.
/// The returned path is the complete image directory under the user's device-data folder.
pub fn load_image(
    data_root: &Path,
    kind: ImageKind,
    cancel: &AtomicBool,
    stats: &mut DownloadStats,
    report: impl FnMut(DownloadProgress),
) -> Result<PathBuf, DownloadError> {
    let started = Instant::now();
    let limits = RequestLimits {
        deadline: started + DOWNLOAD_TIMEOUT,
        connect_timeout: CONNECT_TIMEOUT,
        idle_timeout: STALL_TIMEOUT,
        cancel,
    };
    Downloader::new(limits, started, stats, report).load(
        data_root,
        kind.package(),
        &pinned::SOURCES,
    )
}

struct Package {
    folder: &'static str,
    files: &'static [PinnedFile],
}

struct PinnedFile {
    local: &'static str,
    remote: &'static str,
    bytes: u64,
    sha256: &'static str,
}

#[derive(Clone, Copy)]
struct Source<'a> {
    name: &'static str,
    url: &'a str,
}

impl Source<'_> {
    fn file_url(self, folder: &str, remote: &str) -> String {
        self.url
            .replace("{repo}", DDI_REPO)
            .replace("{ref}", DDI_REF)
            .replace("{path}", &format!("PersonalizedImages/{folder}/{remote}"))
    }
}

struct Downloader<'a, F> {
    limits: RequestLimits<'a>,
    started: Instant,
    stats: &'a mut DownloadStats,
    report: F,
}

impl<'a, F: FnMut(DownloadProgress)> Downloader<'a, F> {
    fn new(
        limits: RequestLimits<'a>,
        started: Instant,
        stats: &'a mut DownloadStats,
        report: F,
    ) -> Self {
        *stats = DownloadStats::default();
        Self {
            limits,
            started,
            stats,
            report,
        }
    }

    fn load(
        &mut self,
        data_root: &Path,
        package: &Package,
        sources: &[Source<'_>],
    ) -> Result<PathBuf, DownloadError> {
        self.check_active()?;
        paths::ensure_data_dir(data_root)?;
        let device_dir = data_root.join(DEVICE_DATA_DIR);
        paths::ensure_data_dir(&device_dir)?;
        let _lock = self.lock(&device_dir)?;
        let folder = device_dir.join(package.folder);
        if cache_valid(&folder, package) {
            self.check_active()?;
            return Ok(folder);
        }

        let source_file = device_dir.join(SOURCE_FILE);
        let preference = read_marker(&source_file);
        let preferred = preference.as_deref().map(str::trim);

        let staging = device_dir.join(format!("{}.download", package.folder));
        prepare_staging(&staging, package)?;
        let downloaded = self.fill(&staging, package, sources, preferred);
        if let Some(source) = self.stats.source
            && Some(source) != preferred
        {
            let _ = atomic::write(&source_file, source.as_bytes());
        }
        downloaded?;
        self.check_active()?;
        publish(&staging, &folder, package)?;
        Ok(folder)
    }

    fn lock(&self, device_dir: &Path) -> Result<File, DownloadError> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(device_dir.join(".developer-image.lock"))?;
        loop {
            self.check_active()?;
            match lock.try_lock() {
                Ok(()) => return Ok(lock),
                Err(TryLockError::WouldBlock) => {
                    let remaining = self
                        .limits
                        .deadline
                        .saturating_duration_since(Instant::now());
                    thread::sleep(remaining.min(LOCK_POLL));
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }

    fn fill(
        &mut self,
        staging: &Path,
        package: &Package,
        sources: &[Source<'_>],
        mut preferred: Option<&str>,
    ) -> Result<(), DownloadError> {
        self.stats.count = package.files.len();
        let client = Client::new(concat!("CopiaGo/", env!("CARGO_PKG_VERSION")))?;
        for (index, pin) in package.files.iter().enumerate() {
            self.check_active()?;
            self.stats.file = pin.local;
            self.stats.index = index + 1;
            self.stats.received = 0;
            self.stats.total = pin.bytes;
            self.stats.http_status = None;

            let ready = staging.join(pin.local);
            if file_size(&ready)? == Some(pin.bytes) {
                let (bytes, digest) = self.hash_file(&ready, pin.bytes)?;
                if bytes == pin.bytes && digest_matches(digest, pin) {
                    self.stats.received = pin.bytes;
                    continue;
                }
            }
            remove_file_if_present(&ready)?;

            let mut last = None;
            let (first, rest) = sources
                .iter()
                .partition::<Vec<&Source<'_>>, _>(|source| Some(source.name) == preferred);
            for source in first.into_iter().chain(rest) {
                if last.is_some() {
                    self.report(true);
                }
                let url = source.file_url(package.folder, pin.remote);
                let before = self.stats.fetched;
                let mut result = self.fetch(&client, &url, &ready, pin);
                if let Err(AttemptError::Source(
                    reason @ SourceError::Integrity { resumed: true },
                )) = result
                {
                    self.stats.failures.push(SourceFailure {
                        source: source.name,
                        reason,
                    });
                    // A bad prefix can come from a previous source. Give this source one clean try.
                    result = self.fetch(&client, &url, &ready, pin);
                }
                if self.stats.fetched > before {
                    self.stats.source = Some(source.name);
                }
                match result {
                    Ok(()) => {
                        preferred = Some(source.name);
                        last = None;
                        break;
                    }
                    Err(AttemptError::Stop(error)) => return Err(error),
                    Err(AttemptError::Source(reason)) => {
                        let failure = SourceFailure {
                            source: source.name,
                            reason,
                        };
                        self.stats.failures.push(failure);
                        last = Some(failure);
                    }
                }
            }
            if let Some(last) = last {
                return Err(DownloadError::SourcesFailed {
                    file: pin.local,
                    last,
                });
            }
        }
        Ok(())
    }

    fn fetch(
        &mut self,
        client: &Client,
        url: &str,
        ready: &Path,
        pin: &PinnedFile,
    ) -> Result<(), AttemptError> {
        self.check_active()?;
        let part = part_path(ready);
        let mut offset = file_size(&part)?.unwrap_or(0);
        if offset > pin.bytes {
            remove_file_if_present(&part)?;
            offset = 0;
        }

        let mut digest = if offset == 0 {
            Sha256::new()
        } else {
            let (read, digest) = self.hash_file(&part, offset)?;
            if read != offset {
                return Err(SourceError::Incomplete.into());
            }
            digest
        };
        self.stats.received = offset;
        self.stats.http_status = None;

        let file = if offset < pin.bytes {
            let mut headers = vec![("Accept-Encoding", "identity")];
            let range;
            if offset != 0 {
                range = format!("bytes={offset}-");
                headers.push(("Range", &range));
            }
            let mut response = client.get(url, &headers, self.limits)?;
            let status = response.status();
            self.stats.http_status = Some(status);
            match status {
                200 => {
                    if offset != 0 {
                        self.stats.restarts += 1;
                        offset = 0;
                        self.stats.received = 0;
                        digest = Sha256::new();
                    }
                }
                206 => {
                    let start = response
                        .header("Content-Range")?
                        .as_deref()
                        .and_then(range_start);
                    if offset == 0 || start != Some(offset) {
                        return Err(SourceError::UnexpectedRange.into());
                    }
                    self.stats.resumed += offset;
                }
                416 => {
                    remove_file_if_present(&part)?;
                    return Err(SourceError::HttpStatus(status).into());
                }
                _ => return Err(SourceError::HttpStatus(status).into()),
            }

            self.report(false);
            self.check_active()?;
            let mut file = OpenOptions::new()
                .create(true)
                .write(true)
                .append(offset != 0)
                .truncate(offset == 0)
                .open(&part)?;

            let mut last_report = Instant::now();
            let mut last_received = self.stats.received;
            loop {
                let chunk = response.read_chunk()?;
                if chunk.is_empty() {
                    break;
                }
                self.check_active()?;
                let chunk_len = chunk.len() as u64;
                if self.stats.received + chunk_len > pin.bytes {
                    drop(file);
                    remove_file_if_present(&part)?;
                    return Err(SourceError::Integrity {
                        resumed: offset != 0,
                    }
                    .into());
                }
                file.write_all(chunk)?;
                digest.update(chunk);
                self.stats.received += chunk_len;
                self.stats.fetched += chunk_len;
                if last_report.elapsed() >= PROGRESS_INTERVAL {
                    self.report(false);
                    last_report = Instant::now();
                    last_received = self.stats.received;
                }
            }
            if self.stats.received != pin.bytes {
                return Err(SourceError::Incomplete.into());
            }
            if last_received != self.stats.received {
                self.report(false);
            }
            file
        } else {
            OpenOptions::new().write(true).open(&part)?
        };

        self.check_active()?;
        if !digest_matches(digest, pin) {
            drop(file);
            remove_file_if_present(&part)?;
            return Err(SourceError::Integrity {
                resumed: offset != 0,
            }
            .into());
        }
        file.sync_all()?;
        drop(file);
        self.check_active()?;
        atomic::move_file(&part, ready)?;
        Ok(())
    }

    fn check_active(&self) -> Result<(), DownloadError> {
        self.limits.check().map_err(DownloadError::from)
    }

    fn hash_file(&self, path: &Path, limit: u64) -> Result<(u64, Sha256), DownloadError> {
        let mut file = File::open(path)?.take(limit);
        let mut buffer = vec![0; 64 * 1024];
        let mut digest = Sha256::new();
        let mut total = 0;
        loop {
            self.check_active()?;
            let bytes = file.read(&mut buffer)?;
            if bytes == 0 {
                return Ok((total, digest));
            }
            digest.update(&buffer[..bytes]);
            total += bytes as u64;
        }
    }

    fn report(&mut self, switching_source: bool) {
        let elapsed = self.started.elapsed().as_secs_f64();
        let stats = &self.stats;
        (self.report)(DownloadProgress {
            file: stats.file,
            index: stats.index,
            count: stats.count,
            progress: Progress {
                received_bytes: stats.received,
                total_bytes: Some(stats.total),
                bytes_per_second: (stats.fetched as f64 / elapsed) as u64,
            },
            switching_source,
        });
    }
}

enum AttemptError {
    Stop(DownloadError),
    Source(SourceError),
}

impl From<DownloadError> for AttemptError {
    fn from(error: DownloadError) -> Self {
        Self::Stop(error)
    }
}

impl From<io::Error> for AttemptError {
    fn from(error: io::Error) -> Self {
        Self::Stop(error.into())
    }
}

impl From<HttpError> for AttemptError {
    fn from(error: HttpError) -> Self {
        match error {
            HttpError::Canceled => Self::Stop(DownloadError::Canceled),
            HttpError::Deadline => Self::Stop(DownloadError::Timeout),
            error => Self::Source(SourceError::Transport(error)),
        }
    }
}

impl From<SourceError> for AttemptError {
    fn from(error: SourceError) -> Self {
        Self::Source(error)
    }
}

fn digest_matches(hash: Sha256, pin: &PinnedFile) -> bool {
    format!("{:x}", hash.finalize()) == pin.sha256
}

fn range_start(header: &str) -> Option<u64> {
    let content = header.strip_prefix("bytes ")?;
    let start = content.split_once('-').map_or(content, |(start, _)| start);
    if !start.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    start.parse().ok()
}

fn read_marker(path: &Path) -> Option<String> {
    let mut contents = String::new();
    // Both marker formats are short ASCII strings. Ignore malformed or oversized input.
    File::open(path)
        .ok()?
        .take(129)
        .read_to_string(&mut contents)
        .ok()?;
    (contents.len() <= 128 && contents.is_ascii()).then_some(contents)
}

fn part_path(ready: &Path) -> PathBuf {
    let mut part = ready.as_os_str().to_os_string();
    part.push(".part");
    PathBuf::from(part)
}

fn cache_valid(folder: &Path, package: &Package) -> bool {
    package
        .files
        .iter()
        .all(|pin| file_size(&folder.join(pin.local)).is_ok_and(|size| size == Some(pin.bytes)))
        && plist::Value::from_file(folder.join(MANIFEST))
            .ok()
            .and_then(|value| value.into_dictionary())
            .is_some_and(|manifest| {
                manifest
                    .get("ProductBuildVersion")
                    .and_then(plist::Value::as_string)
                    == Some(DDI_BUILD)
            })
}

fn file_size(path: &Path) -> io::Result<Option<u64>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(metadata.len())),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "expected a developer image file",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn prepare_staging(staging: &Path, package: &Package) -> io::Result<()> {
    paths::ensure_data_dir(staging)?;
    let marker = staging.join(REF_FILE);
    if !read_marker(&marker).is_some_and(|stored| stored.trim() == DDI_REF) {
        for file in package.files {
            let ready = staging.join(file.local);
            remove_file_if_present(&ready)?;
            remove_file_if_present(&part_path(&ready))?;
        }
        atomic::write(&marker, DDI_REF.as_bytes())?;
    }
    Ok(())
}

fn publish(staging: &Path, folder: &Path, package: &Package) -> io::Result<()> {
    paths::ensure_data_dir(folder)?;
    let manifest = folder.join(MANIFEST);
    // The manifest is the completion marker for both implementations. A failed publication
    // cannot leave a mixture of old and new payloads that appears complete.
    remove_file_if_present(&manifest)?;
    for file in package.files.iter().filter(|file| file.local != MANIFEST) {
        atomic::move_file(&staging.join(file.local), &folder.join(file.local))?;
    }
    atomic::move_file(&staging.join(MANIFEST), &manifest)?;
    let _ = fs::remove_file(staging.join(REF_FILE));
    let _ = fs::remove_dir(staging);
    Ok(())
}
