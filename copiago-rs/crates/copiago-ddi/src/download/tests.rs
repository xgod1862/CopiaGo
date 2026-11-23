use super::*;

use std::io::{BufRead, BufReader, ErrorKind};
use std::net::{TcpListener, TcpStream};
use std::os::windows::fs::OpenOptionsExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Barrier};

const BODY: &[u8] = b"abcdef";
const PLIST: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?><plist version="1.0"><dict><key>ProductBuildVersion</key><string>27B52</string></dict></plist>"#;
const PAYLOAD: &str = "Image.dmg";
const TEST_PACKAGE: Package = Package {
    folder: "TestImage",
    files: &[
        PinnedFile {
            local: MANIFEST,
            remote: MANIFEST,
            bytes: PLIST.len() as u64,
            sha256: "21fecb860de6d915882011c72049452c448d3e3f250dc3395c7ddb176fe79dac",
        },
        PinnedFile {
            local: PAYLOAD,
            remote: PAYLOAD,
            bytes: BODY.len() as u64,
            sha256: "bef57ec7f53a6d40beb640a780a639c83bc29ac8a9816f1fc6c5c6dcd93c4721",
        },
    ],
};
const LOCAL_TIMEOUT: Duration = Duration::from_secs(5);
const LOCAL_IDLE: Duration = Duration::from_secs(2);

#[test]
fn incomplete_body_resumes_and_records_the_working_source() {
    let fixture = Fixture::new();
    let first = Server::new([Reply::body(b"abc")]);
    let second = Server::new([Reply::partial(b"def", "bytes 3-5/6")]);
    let sources = [first.source("first"), second.source("second")];

    let run = fixture.run(&sources);
    fixture.assert_installed(run.result);
    assert_eq!((run.stats.fetched, run.stats.resumed), (6, 3));
    assert_eq!(run.stats.failures[0].reason, SourceError::Incomplete);
    assert_eq!(run.stats.source, Some("second"));
    let first_request = first.request();
    assert!(!first_request.contains("range:"));
    assert!(first_request.contains("accept-encoding: identity"));
    assert!(second.request().contains("range: bytes=3-\r\n"));
    assert!(
        run.events
            .iter()
            .any(|event| { event.switching_source && event.progress.received_bytes == 3 })
    );
    let progress = run.events.last().unwrap().progress;
    assert_eq!(
        (progress.received_bytes, progress.total_bytes),
        (6, Some(6))
    );
    assert!(progress.bytes_per_second > 0);
    assert_eq!(
        fs::read_to_string(fixture.device().join(SOURCE_FILE)).unwrap(),
        "second"
    );

    fixture.seed_manifest();
    fs::remove_file(fixture.installed().join(PAYLOAD)).unwrap();
    let first = Server::new([]);
    let second = Server::new([Reply::body(BODY)]);
    let run = fixture.run(&[first.source("first"), second.source("second")]);
    fixture.assert_installed(run.result);
    assert!(
        second
            .request()
            .starts_with("get /personalizedimages/testimage/image.dmg ")
    );
    assert!(run.stats.failures.is_empty());
    assert!(first.requests.try_recv().is_err());
}

#[test]
fn stalled_body_falls_back_while_preserving_the_downloaded_prefix() {
    let fixture = Fixture::new();
    let first = Server::new([Reply::StallBody(None)]);
    let second = Server::new([Reply::partial(b"def", "bytes 3-5/6")]);
    let cancel = AtomicBool::new(false);
    let run = fixture.run_with(
        &[first.source("first"), second.source("second")],
        &cancel,
        LOCAL_TIMEOUT,
        Duration::from_millis(300),
    );

    fixture.assert_installed(run.result);
    assert!(second.request().contains("range: bytes=3-\r\n"));
    assert_eq!(
        run.stats.failures[0].reason,
        SourceError::Transport(HttpError::Timeout)
    );
}

#[test]
fn a_corrupt_resumed_prefix_gets_one_clean_retry_on_the_same_source() {
    let fixture = Fixture::new();
    fs::write(fixture.part(), b"xyz").unwrap();
    let source = Server::new([Reply::partial(b"def", "bytes 3-5/6"), Reply::body(BODY)]);
    let run = fixture.run(&[source.source("first")]);

    fixture.assert_installed(run.result);
    assert_eq!(
        run.stats.failures,
        [SourceFailure {
            source: "first",
            reason: SourceError::Integrity { resumed: true }
        }]
    );
    assert!(source.request().contains("range: bytes=3-\r\n"));
    assert!(!source.request().contains("range:"));
}

#[test]
fn bad_hash_and_oversized_body_are_discarded_before_switching() {
    let fixture = Fixture::new();
    let bad = Server::new([Reply::body(b"abcxyz")]);
    let oversized = Server::new([Reply::body(b"abcdefgh")]);
    let valid = Server::new([Reply::body(BODY)]);
    let run = fixture.run(&[
        bad.source("bad"),
        oversized.source("big"),
        valid.source("valid"),
    ]);

    fixture.assert_installed(run.result);
    assert!(!valid.request().contains("range:"));
    assert_eq!(run.stats.failures.len(), 2);
    assert!(
        run.stats
            .failures
            .iter()
            .all(|failure| { failure.reason == SourceError::Integrity { resumed: false } })
    );
}

#[test]
fn wrong_range_keeps_the_prefix_and_a_200_response_restarts_it() {
    let fixture = Fixture::new();
    fs::write(fixture.part(), b"abc").unwrap();
    let wrong = Server::new([Reply::partial(b"ef", "bytes 4-5/6")]);
    let full = Server::new([Reply::body(BODY)]);
    let run = fixture.run(&[wrong.source("wrong"), full.source("full")]);

    fixture.assert_installed(run.result);
    assert!(wrong.request().contains("range: bytes=3-\r\n"));
    assert!(full.request().contains("range: bytes=3-\r\n"));
    assert_eq!(run.stats.restarts, 1);
    assert_eq!(run.stats.failures[0].reason, SourceError::UnexpectedRange);
}

#[test]
fn range_not_satisfiable_discards_the_prefix() {
    let fixture = Fixture::new();
    fs::write(fixture.part(), b"abc").unwrap();
    let first = Server::new([Reply::Content {
        status: 416,
        body: b"",
        range: None,
    }]);
    let second = Server::new([Reply::body(BODY)]);
    let run = fixture.run(&[first.source("first"), second.source("second")]);

    fixture.assert_installed(run.result);
    assert!(first.request().contains("range: bytes=3-\r\n"));
    assert!(!second.request().contains("range:"));
    assert_eq!(run.stats.failures[0].reason, SourceError::HttpStatus(416));
}

#[test]
fn cancellation_interrupts_waiting_for_headers() {
    let fixture = Fixture::new();
    let (signal, waiting) = mpsc::channel();
    let source = Server::new([Reply::StallHeaders(signal)]);
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let run = thread::scope(|scope| {
        let cancel = &cancel;
        scope.spawn(move || {
            waiting.recv_timeout(LOCAL_TIMEOUT).unwrap();
            cancel.store(true, Ordering::Relaxed);
        });
        fixture.run_with(&[source.source("first")], cancel, LOCAL_TIMEOUT, LOCAL_IDLE)
    });

    assert!(matches!(run.result, Err(DownloadError::Canceled)));
    assert!(started.elapsed() < LOCAL_IDLE);
    assert!(!fixture.installed().exists());
    assert!(!fixture.part().exists());
}

#[test]
fn cancellation_interrupts_a_pending_read_and_keeps_the_prefix() {
    let fixture = Fixture::new();
    let (signal, waiting) = mpsc::channel();
    let source = Server::new([Reply::StallBody(Some(signal))]);
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let run = thread::scope(|scope| {
        let cancel = &cancel;
        let part = fixture.part();
        scope.spawn(move || {
            waiting.recv_timeout(LOCAL_TIMEOUT).unwrap();
            while file_size(&part).unwrap() != Some(3) {
                assert!(started.elapsed() < LOCAL_IDLE);
                thread::sleep(Duration::from_millis(10));
            }
            thread::sleep(Duration::from_millis(80));
            cancel.store(true, Ordering::Relaxed);
        });
        fixture.run_with(&[source.source("first")], cancel, LOCAL_TIMEOUT, LOCAL_IDLE)
    });

    assert!(matches!(run.result, Err(DownloadError::Canceled)));
    assert!(started.elapsed() < LOCAL_IDLE);
    assert!(!fixture.installed().exists());
    assert_eq!(fs::read(fixture.part()).unwrap(), b"abc");
    assert_eq!(
        fs::read_to_string(fixture.device().join(SOURCE_FILE)).unwrap(),
        "first"
    );

    let next = Server::new([Reply::partial(b"def", "bytes 3-5/6")]);
    fixture.assert_installed(fixture.run(&[next.source("first")]).result);
    assert!(next.request().contains("range: bytes=3-\r\n"));
}

#[test]
fn absolute_deadline_stops_trickling_data_without_switching_mirrors() {
    let fixture = Fixture::new();
    let first = Server::new([Reply::Trickle]);
    let second = Server::new([]);
    let run = fixture.run_with(
        &[first.source("first"), second.source("second")],
        &AtomicBool::new(false),
        Duration::from_millis(375),
        LOCAL_IDLE,
    );

    assert!(matches!(run.result, Err(DownloadError::Timeout)));
    assert!((1..BODY.len() as u64).contains(&run.stats.fetched));
    assert!(run.stats.failures.is_empty());
    assert!(second.requests.try_recv().is_err());
    assert!(!fixture.installed().join(MANIFEST).exists());
}

#[test]
fn stale_staging_is_restarted_and_a_complete_cache_is_reused() {
    let fixture = Fixture::new();
    fs::write(fixture.staging().join(REF_FILE), b"old pin").unwrap();
    fs::write(fixture.part(), b"abc").unwrap();
    let source = Server::new([Reply::body(PLIST), Reply::body(BODY)]);
    fixture.assert_installed(fixture.run(&[source.source("first")]).result);
    assert!(source.request().contains("buildmanifest.plist"));
    assert!(!source.request().contains("range:"));

    let no_network = Server::new([]);
    let run = fixture.run(&[no_network.source("none")]);
    fixture.assert_installed(run.result);
    assert!(run.events.is_empty());
    assert_eq!(run.stats.fetched, 0);
    assert_eq!(run.stats.count, 0);
    assert!(no_network.requests.try_recv().is_err());
}

#[test]
fn wrong_staged_hash_is_refetched_and_a_failed_attempt_keeps_the_old_cache() {
    let fixture = Fixture::new();
    fs::write(fixture.staging().join(PAYLOAD), b"abcxyz").unwrap();
    paths::ensure_data_dir(&fixture.installed()).unwrap();
    let old_manifest = String::from_utf8(PLIST.to_vec())
        .unwrap()
        .replace(DDI_BUILD, "27A00");
    fs::write(fixture.installed().join(MANIFEST), &old_manifest).unwrap();
    fs::write(fixture.installed().join(PAYLOAD), b"stale!").unwrap();

    let source = Server::new([Reply::body(b"def")]);
    let run = fixture.run(&[source.source("first")]);
    assert!(matches!(
        run.result,
        Err(DownloadError::SourcesFailed { .. })
    ));
    assert_eq!(
        fs::read_to_string(fixture.installed().join(MANIFEST)).unwrap(),
        old_manifest
    );
    assert_eq!(
        fs::read(fixture.installed().join(PAYLOAD)).unwrap(),
        b"stale!"
    );
    assert_eq!(fs::read(fixture.part()).unwrap(), b"def");
}

#[test]
fn simultaneous_callers_share_one_staging_writer() {
    let fixture = Fixture::new();
    let server = Server::new([Reply::body(BODY)]);
    let sources = [server.source("first")];
    let barrier = Barrier::new(2);
    let (first, second) = thread::scope(|scope| {
        let second = scope.spawn(|| {
            barrier.wait();
            fixture.run(&sources)
        });
        barrier.wait();
        let first = fixture.run(&sources);
        (first, second.join().unwrap())
    });
    fixture.assert_installed(first.result);
    fixture.assert_installed(second.result);
    let mut fetched = [first.stats.fetched, second.stats.fetched];
    fetched.sort_unstable();
    assert_eq!(fetched, [0, BODY.len() as u64]);
}

#[test]
fn interrupted_publication_leaves_no_manifest_and_reuses_verified_staging() {
    let fixture = Fixture::new();
    let folder = fixture.installed();
    paths::ensure_data_dir(&folder).unwrap();
    fs::write(folder.join(MANIFEST), PLIST).unwrap();
    fs::write(folder.join(PAYLOAD), b"old").unwrap();
    let reader = File::options()
        .read(true)
        .share_mode(0)
        .open(folder.join(PAYLOAD))
        .unwrap();

    let source = Server::new([Reply::body(BODY)]);
    let run = fixture.run(&[source.source("first")]);
    assert!(matches!(run.result, Err(DownloadError::Io(_))));
    assert_eq!(run.stats.fetched, BODY.len() as u64);
    assert!(!folder.join(MANIFEST).exists());
    assert_eq!(fs::read(fixture.staging().join(PAYLOAD)).unwrap(), BODY);
    drop(reader);

    let no_network = Server::new([]);
    let run = fixture.run(&[no_network.source("first")]);
    fixture.assert_installed(run.result);
    assert_eq!(run.stats.fetched, 0);
}

struct Run {
    result: Result<PathBuf, DownloadError>,
    stats: DownloadStats,
    events: Vec<DownloadProgress>,
}

struct Fixture {
    root: PathBuf,
    resolved: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let parent = std::env::temp_dir();
        let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!("copiago-ddi-test-{}-{sequence}", std::process::id());
        let root = parent.join(&name);
        let resolved = parent.canonicalize().unwrap().join(name);
        assert!(!root.exists());
        paths::ensure_data_dir(&root).unwrap();
        let fixture = Self { root, resolved };
        paths::ensure_data_dir(&fixture.device()).unwrap();
        fixture.seed_manifest();
        fixture
    }

    fn device(&self) -> PathBuf {
        self.root.join(DEVICE_DATA_DIR)
    }

    fn staging(&self) -> PathBuf {
        self.device().join("TestImage.download")
    }

    fn installed(&self) -> PathBuf {
        self.device().join(TEST_PACKAGE.folder)
    }

    fn part(&self) -> PathBuf {
        part_path(&self.staging().join(PAYLOAD))
    }

    fn seed_manifest(&self) {
        let staging = self.staging();
        prepare_staging(&staging, &TEST_PACKAGE).unwrap();
        fs::write(staging.join(MANIFEST), PLIST).unwrap();
    }

    fn assert_installed(&self, result: Result<PathBuf, DownloadError>) {
        let installed = result.unwrap();
        assert_eq!(installed, self.installed());
        assert_eq!(fs::read(installed.join(PAYLOAD)).unwrap(), BODY);
        assert_eq!(fs::read(installed.join(MANIFEST)).unwrap(), PLIST);
        assert!(!self.staging().exists());
    }

    fn run(&self, sources: &[Source<'_>]) -> Run {
        self.run_with(sources, &AtomicBool::new(false), LOCAL_TIMEOUT, LOCAL_IDLE)
    }

    fn run_with(
        &self,
        sources: &[Source<'_>],
        cancel: &AtomicBool,
        timeout: Duration,
        idle_timeout: Duration,
    ) -> Run {
        let started = Instant::now();
        let limits = RequestLimits {
            deadline: started + timeout,
            connect_timeout: LOCAL_IDLE,
            idle_timeout,
            cancel,
        };
        let mut stats = DownloadStats::default();
        let mut events = Vec::new();
        let result = Downloader::new(limits, started, &mut stats, |event| events.push(event)).load(
            &self.root,
            &TEST_PACKAGE,
            sources,
        );
        Run {
            result,
            stats,
            events,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if self
            .root
            .canonicalize()
            .is_ok_and(|path| path == self.resolved)
        {
            let _ = fs::remove_dir_all(&self.resolved);
        }
    }
}

enum Reply {
    Content {
        status: u16,
        body: &'static [u8],
        range: Option<&'static str>,
    },
    StallBody(Option<Sender<()>>),
    StallHeaders(Sender<()>),
    Trickle,
}

impl Reply {
    fn body(body: &'static [u8]) -> Self {
        Self::Content {
            status: 200,
            body,
            range: None,
        }
    }

    fn partial(body: &'static [u8], range: &'static str) -> Self {
        Self::Content {
            status: 206,
            body,
            range: Some(range),
        }
    }
}

struct Server {
    url: String,
    requests: Receiver<String>,
    stop: Arc<AtomicBool>,
    worker: Option<thread::JoinHandle<()>>,
}

impl Server {
    fn new<const N: usize>(replies: [Reply; N]) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/{{path}}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let (sender, requests) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_worker = stop.clone();
        let worker = thread::spawn(move || {
            for reply in replies {
                let mut stream = loop {
                    if stop_worker.load(Ordering::Relaxed) {
                        return;
                    }
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == ErrorKind::WouldBlock => {
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accept HTTP request: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(LOCAL_IDLE)).unwrap();
                stream.set_write_timeout(Some(LOCAL_IDLE)).unwrap();
                let mut reader = BufReader::new(&mut stream);
                let mut request = String::new();
                while !request.ends_with("\r\n\r\n") && reader.read_line(&mut request).unwrap() != 0
                {
                }
                sender.send(request.to_ascii_lowercase()).unwrap();

                match reply {
                    Reply::Content {
                        status,
                        body,
                        range,
                    } => {
                        let range = range
                            .map(|value| format!("Content-Range: {value}\r\n"))
                            .unwrap_or_default();
                        write!(stream, "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\n{range}Connection: close\r\n\r\n", body.len()).unwrap();
                        stream.write_all(body).unwrap();
                    }
                    Reply::StallBody(signal) => {
                        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nabc").unwrap();
                        if let Some(signal) = signal {
                            signal.send(()).unwrap();
                        }
                        wait_for_close(&mut stream, &stop_worker);
                    }
                    Reply::StallHeaders(signal) => {
                        signal.send(()).unwrap();
                        wait_for_close(&mut stream, &stop_worker);
                    }
                    Reply::Trickle => {
                        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n").unwrap();
                        for byte in BODY {
                            if stream.write_all(&[*byte]).is_err() {
                                break;
                            }
                            thread::sleep(Duration::from_millis(140));
                        }
                    }
                }
            }
        });
        Self {
            url,
            requests,
            stop,
            worker: Some(worker),
        }
    }

    fn source(&self, name: &'static str) -> Source<'_> {
        Source {
            name,
            url: &self.url,
        }
    }

    fn request(&self) -> String {
        self.requests.recv_timeout(LOCAL_IDLE).unwrap()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let result = worker.join();
            if !thread::panicking() {
                result.unwrap();
            }
        }
    }
}

fn wait_for_close(stream: &mut TcpStream, stop: &AtomicBool) {
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .unwrap();
    let mut byte = [0];
    while !stop.load(Ordering::Relaxed) {
        match stream.read(&mut byte) {
            Err(error) if matches!(error.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
            _ => return,
        }
    }
}
