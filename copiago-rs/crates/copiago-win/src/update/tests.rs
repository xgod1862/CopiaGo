use super::*;
use serde_json::json;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::JoinHandle;

const TEST_WAIT: Duration = Duration::from_secs(4);
const CURRENT: &str = "1.1.0";
const PAGE: &str = "https://github.com/xgod1862/CopiaGo/releases/tag/v1.2.0";

fn payload(tag: &str, page: &str) -> Value {
    json!({"tag_name": tag, "html_url": page, "draft": false, "prerelease": false})
}

fn found() -> Option<Release> {
    newer_release(CURRENT, &payload("v1.2.0", PAGE))
}

#[test]
fn compares_only_plain_versions_numerically() {
    for (current, latest, expected) in [
        ("1.9.3", " \u{1c}v001.010.003\t", "1.10.3"),
        ("1.2.99", "v١.𝟛.００", "1.3.0"),
        (
            "1.18446744073709551615.0",
            "1.18446744073709551616.0",
            "1.18446744073709551616.0",
        ),
        ("0.0.0", "0.0.001", "0.0.1"),
    ] {
        let release = newer_release(current, &payload(latest, PAGE)).unwrap();
        assert_eq!(release.version(), expected);
        assert_eq!(release.page(), PAGE);
    }
    for (current, latest) in [
        ("1.10.0", "v1.10.0"),
        ("1.10.0", "1.9.0"),
        ("2.0.0-beta", "v9.0.0"),
        ("1.0.0", "v1.2.0-beta"),
        ("1.0.0", "v1.2.0+build"),
        ("1.0.0", "V1.2.0"),
        ("1.0.0", "1.2"),
        ("1.0.0", "1.2.3.4"),
        ("1.0.0", "1..3"),
        ("1.0.0", "1.+2.0"),
        ("1.0.0", "1.².0"),
    ] {
        assert_eq!(
            newer_release(current, &payload(latest, PAGE)),
            None,
            "{current}/{latest}"
        );
    }
}

#[test]
fn rejects_drafts_prereleases_and_malformed_payloads() {
    for release in [
        json!(null),
        json!([{"tag_name": "v1.2.0", "html_url": PAGE}]),
        json!({"tag_name": 120, "html_url": PAGE}),
        json!({"tag_name": "v1.2.0", "html_url": null}),
        json!({"tag_name": "v1.2.0", "html_url": PAGE, "draft": true}),
        json!({"tag_name": "v1.2.0", "html_url": PAGE, "prerelease": true}),
        json!({"tag_name": "v1.2.0", "html_url": PAGE, "prerelease": "false"}),
    ] {
        assert_eq!(newer_release(CURRENT, &release), None, "{release}");
    }
    let flags_missing = json!({"tag_name": "v1.2.0", "html_url": PAGE});
    let flags_falsey =
        json!({"tag_name": "v1.2.0", "html_url": PAGE, "draft": 0, "prerelease": []});
    assert_eq!(newer_release(CURRENT, &flags_missing), found());
    assert_eq!(newer_release(CURRENT, &flags_falsey), found());
}

#[test]
fn validates_release_destination_before_constructing_notice() {
    for page in [
        "http://github.com/xgod1862/CopiaGo/releases/tag/v1.2.0",
        "https://github.com.example.com/xgod1862/CopiaGo/releases/tag/v1.2.0",
        "https://github.com@elsewhere.example/xgod1862/CopiaGo/releases/tag/v1.2.0",
        "https://github.com/other/CopiaGo/releases/tag/v1.2.0",
        "https://github.com/xgod1862/CopiaGo/releases-else/tag/v1.2.0",
        "https://github.com/xgod1862/CopiaGo/releases/download/v1.2.0/CopiaGo.exe",
        "https://github.com/xgod1862/CopiaGo/releases/%64ownload/v1.2.0/CopiaGo.exe",
        "https://github.com/xgod1862/CopiaGo/releases/tag/../..",
        "https://github.com/xgod1862/CopiaGo/releases/tag/.%2e/%2E./other",
        "https://github.com/xgod1862/CopiaGo/releases/tag/..%2Fother",
        "https://github.com/xgod1862/CopiaGo/releases/tag/../v1.2.0",
        "https://github.com/xgod1862/CopiaGo/releases/tag/v1.2.0\0else",
        "https://github.com/xgod1862/CopiaGo/releases/tag/..\\..\\v1.2.0",
        "https://github.com/xgod1862/CopiaGo/releases/tag/v1.2.0\"",
    ] {
        assert_eq!(
            newer_release(CURRENT, &payload("1.2.0", page)),
            None,
            "{page:?}"
        );
    }
    for page in [
        PAGE,
        "https://github.com/xgod1862/CopiaGo/releases/latest",
        "https://github.com/xgod1862/CopiaGo/releases/tag/v%EF%BC%91.2.0#assets",
    ] {
        assert!(
            newer_release(CURRENT, &payload("1.2.0", page)).is_some(),
            "{page}"
        );
    }
}

#[test]
fn startup_schedules_one_check_and_never_while_disabled() {
    let (send, wake) = mpsc::channel();
    let mut disabled = UpdateCheck::start_after(
        CURRENT,
        false,
        move || {
            let _ = send.send(());
        },
        Duration::ZERO,
        |_, _| panic!("disabled check must not run"),
    );
    assert_eq!(disabled.release(), None);
    assert!(disabled.pending.is_none());
    assert!(wake.try_recv().is_err());

    assert_eq!(STARTUP_DELAY, Duration::from_secs(15));
    let delay = Duration::from_millis(80);
    let started = Instant::now();
    let (send, wake) = mpsc::channel();
    let mut check = UpdateCheck::start_after(
        CURRENT,
        true,
        move || send.send(()).unwrap(),
        delay,
        move |current, _| {
            assert_eq!(current, CURRENT);
            assert!(started.elapsed() >= delay);
            found()
        },
    );
    wake.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!(check.release(), found().as_ref());
    check.change_enabled(true, |_, _| {
        panic!("unchanged setting must not start a check")
    });
    assert!(check.pending.is_none());
    assert!(wake.try_recv().is_err());
    check.set_enabled(false);
    assert_eq!(check.release(), None);
}

#[test]
fn disabling_or_dropping_cancels_startup_delay() {
    for disable_first in [false, true] {
        let (send, held_by_worker) = mpsc::channel();
        let mut check = UpdateCheck::start_after(
            CURRENT,
            true,
            || panic!("canceled check must not wake the host"),
            STARTUP_DELAY,
            move |_, _| {
                send.send(()).unwrap();
                None
            },
        );
        if disable_first {
            check.set_enabled(false);
            assert_eq!(check.release(), None);
        } else {
            drop(check);
        }
        assert_eq!(
            held_by_worker.recv_timeout(TEST_WAIT),
            Err(mpsc::RecvTimeoutError::Disconnected)
        );
    }
}

#[test]
fn reenable_checks_immediately_and_discards_canceled_results() {
    let (send, wake) = mpsc::channel();
    let mut check = UpdateCheck::start(CURRENT, false, move || send.send(()).unwrap());
    let (entered, worker_started) = mpsc::channel();
    let (finish, worker_finish) = mpsc::channel();
    let (canceled, cancellation) = mpsc::channel();
    check.change_enabled(true, move |_, cancel| {
        entered.send(()).unwrap();
        worker_finish.recv_timeout(TEST_WAIT).unwrap();
        canceled.send(cancel.load(Ordering::Relaxed)).unwrap();
        found()
    });
    worker_started.recv_timeout(TEST_WAIT).unwrap();
    check.set_enabled(false);
    assert_eq!(check.release(), None);
    check.change_enabled(true, |current, _| {
        newer_release(current, &payload("v1.3.0", PAGE))
    });
    finish.send(()).unwrap();
    assert!(cancellation.recv_timeout(TEST_WAIT).unwrap());
    wake.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!(check.release().map(Release::version), Some("1.3.0"));
    check.set_enabled(false);
    assert_eq!(check.release(), None);
}

fn serve_once(response: impl FnOnce(TcpStream) + Send + 'static) -> (String, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!(
        "http://{}/repos/xgod1862/CopiaGo/releases/latest",
        listener.local_addr().unwrap()
    );
    let worker = thread::spawn(move || {
        let start = Instant::now();
        let stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(start.elapsed() < TEST_WAIT, "no connection received");
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("{error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(TEST_WAIT)).unwrap();
        stream.set_write_timeout(Some(TEST_WAIT)).unwrap();
        response(stream);
    });
    (url, worker)
}

fn request_headers(stream: &mut TcpStream) -> String {
    let mut req = Vec::new();
    let mut chunk = [0; 512];
    while !req.ends_with(b"\r\n\r\n") {
        let read = stream.read(&mut chunk).unwrap();
        assert!(read > 0 && req.len() < 16 * 1024);
        req.extend_from_slice(&chunk[..read]);
    }
    String::from_utf8(req).unwrap().to_ascii_lowercase()
}

fn send_response(stream: &mut TcpStream, status: &str, headers: &str, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
}

#[test]
fn winhttp_reads_latest_json_and_swallows_http_and_body_failures() {
    let valid = payload("v1.2.0", PAGE).to_string().into_bytes();
    for (status, body, expected) in [
        ("200 OK", valid.clone(), found()),
        ("403 Forbidden", valid, None),
        ("200 OK", b"<html>not JSON</html>".to_vec(), None),
        ("200 OK", vec![b' '; MAX_RESPONSE_BYTES + 1], None),
    ] {
        let (url, server) = serve_once(move |mut stream| {
            let headers = request_headers(&mut stream);
            assert!(headers.starts_with("get /repos/xgod1862/copiago/releases/latest http/"));
            assert!(headers.contains("\r\nuser-agent: copiago/1.1.0\r\n"));
            assert!(headers.contains("\r\naccept: application/vnd.github+json\r\n"));
            send_response(&mut stream, status, "", &body);
        });
        assert_eq!(
            fetch_release(CURRENT, &url, TEST_WAIT, &AtomicBool::new(false)),
            expected
        );
        server.join().unwrap();
    }
}

#[test]
fn winhttp_does_not_follow_redirect() {
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    other.set_nonblocking(true).unwrap();
    let target = format!("http://{}/other", other.local_addr().unwrap());
    let (url, server) = serve_once(move |mut stream| {
        request_headers(&mut stream);
        send_response(
            &mut stream,
            "302 Found",
            &format!("Location: {target}\r\n"),
            b"",
        );
    });
    assert_eq!(
        fetch_release(CURRENT, &url, TEST_WAIT, &AtomicBool::new(false)),
        None
    );
    server.join().unwrap();
    assert_eq!(
        other.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn winhttp_stalled_body_is_a_silent_completion_after_deadline() {
    let (finish, stall) = mpsc::channel();
    let (url, server) = serve_once(move |mut stream| {
        request_headers(&mut stream);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nConnection: close\r\n\r\n{")
            .unwrap();
        stall.recv_timeout(TEST_WAIT).unwrap();
    });
    let (send, wake) = mpsc::channel();
    let timeout = Duration::from_millis(300);
    let start = Instant::now();
    let mut check = UpdateCheck::start_after(
        CURRENT,
        true,
        move || send.send(()).unwrap(),
        Duration::ZERO,
        move |current, cancel| fetch_release(current, &url, timeout, cancel),
    );
    wake.recv_timeout(TEST_WAIT).unwrap();
    assert_eq!(check.release(), None);
    assert!(start.elapsed() >= timeout);
    assert!(check.pending.is_none());
    finish.send(()).unwrap();
    server.join().unwrap();
}

#[test]
fn disabling_aborts_in_flight_winhttp_without_waking_the_host() {
    let (received, ready) = mpsc::channel();
    let (finish, stall) = mpsc::channel();
    let (url, server) = serve_once(move |mut stream| {
        request_headers(&mut stream);
        received.send(()).unwrap();
        stall.recv_timeout(TEST_WAIT).unwrap();
    });
    let (send, wake) = mpsc::channel();
    let (request_done, done) = mpsc::channel();
    let mut check = UpdateCheck::start_after(
        CURRENT,
        true,
        move || send.send(()).unwrap(),
        Duration::ZERO,
        move |current, cancel| {
            let release = fetch_release(current, &url, TIMEOUT, cancel);
            request_done.send(release.is_none()).unwrap();
            release
        },
    );
    ready.recv_timeout(TEST_WAIT).unwrap();
    check.set_enabled(false);
    assert_eq!(check.release(), None);
    assert!(done.recv_timeout(TEST_WAIT).unwrap());
    assert_eq!(wake.try_recv(), Err(TryRecvError::Empty));
    finish.send(()).unwrap();
    server.join().unwrap();
}
