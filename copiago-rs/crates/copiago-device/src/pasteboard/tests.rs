use std::collections::VecDeque;
use std::future::pending;

use copiago_core::clip::Clip;
use plist::Value;
use tokio::time::sleep;

use super::content::Snapshot;
use super::file_offer::FILE_DESCRIPTOR;
use super::*;

const TEXT: &str = "public.utf8-plain-text";
const URL: &str = "public.url";
const PNG: &str = "public.png";
const JPEG: &str = "public.jpeg";
const HEIC: &str = "public.heic";
const WEB_ARCHIVE: &str = "com.apple.webarchive";

struct FakeBackend(VecDeque<(PullPolicy, Duration, Result<Snapshot, PasteboardError>)>);

impl FakeBackend {
    fn new() -> Self {
        Self(VecDeque::new())
    }

    fn answer(mut self, policy: PullPolicy, snapshot: Snapshot) -> Self {
        self.0.push_back((policy, Duration::ZERO, Ok(snapshot)));
        self
    }

    fn delay(mut self, policy: PullPolicy, seconds: u64, snapshot: Snapshot) -> Self {
        self.0
            .push_back((policy, Duration::from_secs(seconds), Ok(snapshot)));
        self
    }

    fn error(mut self, policy: PullPolicy) -> Self {
        self.0
            .push_back((policy, Duration::ZERO, Err(PasteboardError::RequestFailed)));
        self
    }

    fn verify(&self) {
        assert!(self.0.is_empty(), "expected more requests");
    }
}

impl PasteboardBackend for FakeBackend {
    async fn pull(&mut self, policy: PullPolicy) -> Result<Snapshot, PasteboardError> {
        let (expected, delay, result) = self.0.pop_front().expect("unexpected request");
        assert_eq!(policy, expected);
        if !delay.is_zero() {
            sleep(delay).await;
        }
        result
    }
}

fn dictionary<const N: usize>(fields: [(&str, Value); N]) -> Value {
    Value::Dictionary(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

fn item(types: &[&str], data: Value) -> Value {
    dictionary([
        (
            "types",
            Value::Array(types.iter().map(|&uti| uti.into()).collect()),
        ),
        ("data", data),
    ])
}

fn board(metadata: Value, items: Vec<Value>) -> Snapshot {
    Snapshot::from_reply(dictionary([
        ("command", "PULL_REPLY".into()),
        (
            "pasteboard",
            dictionary([("metadata", metadata), ("items", Value::Array(items))]),
        ),
    ]))
    .unwrap()
}

fn metadata(nonce: Value, count: i64) -> Value {
    dictionary([("nonce", nonce), ("changeCount", count.into())])
}

fn snapshot<const N: usize>(count: i64, types: &[&str], entries: [(&str, Value); N]) -> Snapshot {
    board(
        metadata("nonce".into(), count),
        vec![item(types, dictionary(entries))],
    )
}

fn inline(bytes: &[u8]) -> Value {
    dictionary([("data", Value::Data(bytes.to_vec()))])
}

fn promised(size: i64) -> Value {
    dictionary([("isPromised", true.into()), ("size", size.into())])
}

fn text_at(count: i64, value: &str) -> Snapshot {
    snapshot(count, &[TEXT], [(TEXT, inline(value.as_bytes()))])
}

async fn next_update(mut backend: FakeBackend) -> Update {
    let listing = pull(&mut backend, PullPolicy::AllPromised).await.unwrap();
    let update = read_clip(&mut backend, listing, Language::ZhCn)
        .await
        .unwrap();
    backend.verify();
    update
}

fn assert_clip(event: Option<DeviceEvent>, expected: Clip) {
    let Some(DeviceEvent::Clip {
        clip,
        while_disconnected: false,
    }) = event
    else {
        panic!("expected a clipboard event");
    };
    assert!(clip == expected);
}

fn assert_unsupported(event: Option<DeviceEvent>, expected: &str) {
    let Some(DeviceEvent::Unsupported { kind }) = event else {
        panic!("expected an unsupported-type event");
    };
    assert_eq!(kind.render(Language::ZhCn), expected);
}

#[tokio::test]
async fn text_preserves_unicode_and_surrounding_characters() {
    let text = " \u{fffc}你好😀\r\n";
    let update = next_update(
        FakeBackend::new()
            .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
            .answer(PullPolicy::PromiseSecondary, text_at(2, text)),
    )
    .await;
    assert_clip(update.event, Clip::Text(text.to_owned()));
    assert_eq!(update.change.unwrap().count, 2);
}

#[tokio::test]
async fn text_placeholder_and_whitespace_are_unsupported() {
    let update = next_update(
        FakeBackend::new()
            .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
            .answer(PullPolicy::PromiseSecondary, text_at(1, "\u{fffc} \t\r\n")),
    )
    .await;
    assert_unsupported(update.event, "文件或其他内容");
}

#[tokio::test]
async fn url_accepts_inline_secondary_text() {
    let url = "https://example.test/";
    let update = next_update(
        FakeBackend::new()
            .answer(PullPolicy::AllPromised, snapshot(1, &[URL, TEXT], []))
            .answer(
                PullPolicy::PromiseSecondary,
                snapshot(
                    1,
                    &[URL, TEXT],
                    [
                        (URL, inline(b"opaque URL representation")),
                        (TEXT, inline(url.as_bytes())),
                    ],
                ),
            ),
    )
    .await;
    assert_clip(update.event, Clip::Text(url.to_owned()));
}

#[tokio::test]
async fn url_with_only_promised_text_is_classified_as_a_file() {
    let update = next_update(
        FakeBackend::new()
            .answer(PullPolicy::AllPromised, snapshot(1, &[URL, TEXT], []))
            .answer(
                PullPolicy::PromiseSecondary,
                snapshot(
                    1,
                    &[URL, TEXT],
                    [
                        (URL, inline(b"opaque URL representation")),
                        (TEXT, promised(20)),
                    ],
                ),
            ),
    )
    .await;
    assert_unsupported(update.event, "文件");
}

#[tokio::test]
async fn png_is_returned_from_the_lightweight_pull() {
    let update = next_update(
        FakeBackend::new()
            .answer(PullPolicy::AllPromised, snapshot(1, &[PNG, TEXT], []))
            .answer(
                PullPolicy::PromiseSecondary,
                snapshot(
                    1,
                    &[PNG, TEXT],
                    [(PNG, inline(b"png")), (TEXT, inline(b"optional text"))],
                ),
            ),
    )
    .await;
    assert_clip(
        update.event,
        Clip::Image {
            bytes: b"png".to_vec(),
            uti: PNG.to_owned(),
        },
    );
}

#[tokio::test]
async fn jpeg_wins_when_both_heic_and_jpeg_are_inline() {
    let update = next_update(
        FakeBackend::new()
            .answer(PullPolicy::AllPromised, snapshot(1, &[HEIC, JPEG], []))
            .answer(
                PullPolicy::PromiseSecondary,
                snapshot(
                    1,
                    &[HEIC, JPEG],
                    [(HEIC, inline(b"heic")), (JPEG, inline(b"jpeg"))],
                ),
            ),
    )
    .await;
    assert_clip(
        update.event,
        Clip::Image {
            bytes: b"jpeg".to_vec(),
            uti: JPEG.to_owned(),
        },
    );
}

#[tokio::test]
async fn heic_is_used_when_jpeg_is_only_promised() {
    let update = next_update(
        FakeBackend::new()
            .answer(PullPolicy::AllPromised, snapshot(1, &[HEIC, JPEG], []))
            .answer(
                PullPolicy::PromiseSecondary,
                snapshot(
                    1,
                    &[HEIC, JPEG],
                    [(HEIC, inline(b"heic")), (JPEG, promised(4))],
                ),
            ),
    )
    .await;
    assert_clip(
        update.event,
        Clip::Image {
            bytes: b"heic".to_vec(),
            uti: HEIC.to_owned(),
        },
    );
}

#[tokio::test]
async fn promised_image_gets_an_all_resolved_request() {
    let update = next_update(
        FakeBackend::new()
            .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT, PNG], []))
            .answer(
                PullPolicy::PromiseSecondary,
                snapshot(
                    1,
                    &[TEXT, PNG],
                    [(TEXT, inline(b"accompanying text")), (PNG, promised(3))],
                ),
            )
            .answer(
                PullPolicy::AllResolved,
                snapshot(1, &[TEXT, PNG], [(PNG, inline(b"png"))]),
            ),
    )
    .await;
    assert_clip(
        update.event,
        Clip::Image {
            bytes: b"png".to_vec(),
            uti: PNG.to_owned(),
        },
    );
}

#[tokio::test]
async fn web_archive_uses_first_supported_image_subresource() {
    let resource = |mime: &str, bytes: &[u8]| {
        dictionary([
            ("WebResourceMIMEType", mime.into()),
            ("WebResourceData", Value::Data(bytes.to_vec())),
        ])
    };
    let archive = dictionary([
        ("WebMainResource", resource("text/html", b"html")),
        (
            "WebSubresources",
            Value::Array(vec![
                resource("text/css", b"styles"),
                resource("image/jpeg", b"first image"),
                resource("image/png", b"second image"),
            ]),
        ),
    ]);
    let mut raw = Vec::new();
    archive.to_writer_binary(&mut raw).unwrap();

    let update = next_update(
        FakeBackend::new()
            .answer(
                PullPolicy::AllPromised,
                snapshot(1, &[WEB_ARCHIVE, TEXT], []),
            )
            .answer(
                PullPolicy::PromiseSecondary,
                snapshot(
                    1,
                    &[WEB_ARCHIVE, TEXT],
                    [
                        (WEB_ARCHIVE, inline(&raw)),
                        (TEXT, inline("\u{fffc}".as_bytes())),
                    ],
                ),
            ),
    )
    .await;
    assert_clip(
        update.event,
        Clip::Image {
            bytes: b"first image".to_vec(),
            uti: JPEG.to_owned(),
        },
    );

    let update = next_update(
        FakeBackend::new().answer(PullPolicy::AllPromised, snapshot(1, &[WEB_ARCHIVE], [])),
    )
    .await;
    assert_unsupported(update.event, "文件或其他内容");
}

#[tokio::test]
async fn unsupported_types_do_not_pull_payloads() {
    for (types, expected) in [
        (
            &["public.file-url", "com.apple.quicktime-movie"][..],
            Some("视频"),
        ),
        (&["public.mpeg-4"][..], Some("视频")),
        (&["public.zip-archive"][..], Some("文件")),
        (&["com.adobe.pdf"][..], Some("文件")),
        (&["public.rtf"][..], Some("文件或其他内容")),
        (&[][..], None),
    ] {
        let update =
            next_update(FakeBackend::new().answer(PullPolicy::AllPromised, snapshot(1, types, [])))
                .await;
        match expected {
            Some(kind) => assert_unsupported(update.event, kind),
            None => assert!(update.event.is_none()),
        }
    }
}

#[tokio::test]
async fn files_use_threshold_and_keep_offer_identity() {
    const PDF: &str = "com.adobe.pdf";
    let mut descriptor = Vec::new();
    dictionary([(
        "$objects",
        Value::Array(vec![
            "other.txt".into(),
            "/private/wrong.pdf".into(),
            "报告:草稿.PDF".into(),
        ]),
    )])
    .to_writer_binary(&mut descriptor)
    .unwrap();
    let types = &[FILE_DESCRIPTOR, "public.file-url", PDF];
    for (payload, expected_size, inline_data) in [
        (inline(b"pdf"), 3, Some(b"pdf".to_vec())),
        (promised(300_000), 300_000, None),
    ] {
        let listing = snapshot(10, types, []);
        let offer_change = listing.change.clone();
        let update = next_update(
            FakeBackend::new()
                .answer(PullPolicy::AllPromised, listing)
                .answer(
                    PullPolicy::Threshold(256 * 1024),
                    snapshot(
                        11,
                        types,
                        [(FILE_DESCRIPTOR, inline(&descriptor)), (PDF, payload)],
                    ),
                ),
        )
        .await;
        let Some(DeviceEvent::FileOffer(offer)) = update.event else {
            panic!("expected a file offer");
        };
        assert_eq!(offer.uti, PDF);
        assert_eq!(offer.name, "报告_草稿.PDF");
        assert_eq!(offer.size, expected_size);
        assert!(offer.data == inline_data);
        assert!(offer.change == offer_change && update.change == offer_change);
    }
}

#[test]
fn nonce_bytes_and_text_are_distinct_and_count_is_required() {
    let string = board(metadata("same".into(), 4), vec![]);
    let binary = board(metadata(Value::Data(b"same".to_vec()), 4), vec![]);
    let no_nonce = board(dictionary([("changeCount", 4i64.into())]), vec![]);
    let no_count = board(dictionary([("nonce", "same".into())]), vec![]);
    assert!(string.change != binary.change);
    assert!(no_nonce.change.unwrap().nonce.is_none());
    assert!(no_count.change.is_none());
}

#[tokio::test(start_paused = true)]
async fn notifications_trigger_a_pull_before_the_poll_interval() {
    for &notification in &NOTIFICATIONS {
        let mut backend = FakeBackend::new()
            .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
            .answer(PullPolicy::AllPromised, snapshot(2, &[TEXT], []))
            .answer(PullPolicy::PromiseSecondary, text_at(2, "new"));
        let started = Instant::now();
        let mut names = VecDeque::from(["unrelated notification", notification]);
        let mut ready = false;
        watch(
            &mut backend,
            async || match names.pop_front() {
                Some(name) => {
                    sleep(Duration::from_millis(50)).await;
                    Ok(name.to_owned())
                }
                None => pending().await,
            },
            false,
            Language::ZhCn,
            |event| {
                if matches!(
                    &event,
                    DeviceEvent::Status(ConnState::Connected { wireless: false }, _)
                ) {
                    ready = true;
                    return ControlFlow::Continue(());
                }
                assert!(ready);
                assert_clip(Some(event), Clip::Text("new".to_owned()));
                assert!(started.elapsed() >= Duration::from_millis(100));
                assert!(started.elapsed() < Duration::from_secs(1));
                ControlFlow::Break(())
            },
        )
        .await
        .unwrap();
        backend.verify();
    }
}

#[tokio::test(start_paused = true)]
async fn two_second_poll_detects_nonce_change_at_the_same_count() {
    let next = || {
        board(
            metadata("new daemon".into(), 1),
            vec![item(&[TEXT], dictionary([]))],
        )
    };
    let mut backend = FakeBackend::new()
        .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
        .answer(PullPolicy::AllPromised, next())
        .answer(PullPolicy::AllPromised, next())
        .answer(
            PullPolicy::PromiseSecondary,
            board(
                metadata("new daemon".into(), 1),
                vec![item(
                    &[TEXT],
                    dictionary([(TEXT, inline(b"after restart"))]),
                )],
            ),
        );
    let started = Instant::now();
    watch(&mut backend, pending, false, Language::ZhCn, |event| {
        if matches!(event, DeviceEvent::Status(..)) {
            return ControlFlow::Continue(());
        }
        assert_clip(Some(event), Clip::Text("after restart".to_owned()));
        assert!(started.elapsed() >= POLL_INTERVAL && started.elapsed() < Duration::from_secs(3));
        ControlFlow::Break(())
    })
    .await
    .unwrap();
    backend.verify();
}

#[tokio::test(start_paused = true)]
async fn notes_64_second_response_times_out_and_next_copy_arrives() {
    let note_types = &[TEXT, PNG, "public.rtf"];
    let mut backend = FakeBackend::new()
        .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
        .answer(PullPolicy::AllPromised, snapshot(2, note_types, []))
        .answer(
            PullPolicy::PromiseSecondary,
            snapshot(
                2,
                note_types,
                [
                    (
                        TEXT,
                        inline(b"Notes text does not replace the clipboard on failure"),
                    ),
                    (PNG, promised(3)),
                ],
            ),
        )
        .delay(
            PullPolicy::AllResolved,
            64,
            snapshot(2, note_types, [(PNG, inline(b"png"))]),
        )
        .answer(PullPolicy::AllPromised, snapshot(2, note_types, [])) // overdue periodic poll
        .answer(PullPolicy::AllPromised, snapshot(2, note_types, [])) // duplicate notification
        .answer(PullPolicy::AllPromised, snapshot(3, &[TEXT], []))
        .answer(PullPolicy::PromiseSecondary, text_at(3, "after Notes"));
    let started = Instant::now();
    let mut signals = VecDeque::from([50, 30_500, 31_000]);
    let mut saw_timeout = false;
    watch(
        &mut backend,
        async || {
            if let Some(millis) = signals.pop_front() {
                sleep_until(started + Duration::from_millis(millis)).await;
                Ok(NOTIFICATIONS[0].to_owned())
            } else {
                pending().await
            }
        },
        false,
        Language::ZhCn,
        |event| {
            match event {
                DeviceEvent::Status(..) => {}
                DeviceEvent::Unsupported { .. } => {
                    assert!(!saw_timeout);
                    assert!(started.elapsed() >= Duration::from_secs(30));
                    assert!(started.elapsed() < Duration::from_secs(31));
                    saw_timeout = true;
                }
                event => {
                    assert!(saw_timeout);
                    assert!(started.elapsed() < Duration::from_secs(32));
                    assert_clip(Some(event), Clip::Text("after Notes".to_owned()));
                    return ControlFlow::Break(());
                }
            }
            ControlFlow::Continue(())
        },
    )
    .await
    .unwrap();
    backend.verify();
}

#[tokio::test(start_paused = true)]
async fn lightweight_timeout_keeps_listing_id_and_skips_without_clip() {
    let listing = snapshot(1, &[TEXT], []);
    let change = listing.change.clone();
    let started = Instant::now();
    let update = next_update(
        FakeBackend::new()
            .answer(PullPolicy::AllPromised, listing)
            .delay(PullPolicy::PromiseSecondary, 64, text_at(2, "slow answer")),
    )
    .await;
    assert!(update.change == change);
    assert!(update.event.is_none());
    assert!(started.elapsed() >= Duration::from_secs(15));
    assert!(started.elapsed() < Duration::from_secs(16));
}

#[tokio::test(start_paused = true)]
async fn listing_timeout_retries_after_five_seconds() {
    let mut backend = FakeBackend::new()
        .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
        .delay(PullPolicy::AllPromised, 64, snapshot(1, &[TEXT], []))
        .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
        .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
        .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
        .answer(PullPolicy::AllPromised, snapshot(2, &[TEXT], []))
        .answer(PullPolicy::PromiseSecondary, text_at(2, "after retry"));
    let started = Instant::now();
    let mut triggered = false;
    watch(
        &mut backend,
        async || {
            if triggered {
                return pending().await;
            }
            triggered = true;
            sleep(Duration::from_millis(50)).await;
            Ok(NOTIFICATIONS[0].to_owned())
        },
        false,
        Language::ZhCn,
        |event| {
            if matches!(event, DeviceEvent::Status(..)) {
                return ControlFlow::Continue(());
            }
            assert!(started.elapsed() >= METADATA_TIMEOUT + LISTING_RETRY);
            assert!(started.elapsed() < Duration::from_secs(14));
            assert_clip(Some(event), Clip::Text("after retry".to_owned()));
            ControlFlow::Break(())
        },
    )
    .await
    .unwrap();
    backend.verify();
}

#[tokio::test(start_paused = true)]
async fn three_consecutive_failed_polls_end_the_watch() {
    let mut backend = FakeBackend::new()
        .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
        .error(PullPolicy::AllPromised)
        .error(PullPolicy::AllPromised)
        .answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []))
        .error(PullPolicy::AllPromised)
        .error(PullPolicy::AllPromised)
        .error(PullPolicy::AllPromised);
    let result = watch(&mut backend, pending, false, Language::ZhCn, |event| {
        assert!(matches!(event, DeviceEvent::Status(..)));
        ControlFlow::Continue(())
    })
    .await;
    assert_eq!(result, Err(PasteboardError::MetadataUnresponsive));
    backend.verify();
}

#[tokio::test(start_paused = true)]
async fn notification_stream_end_is_reported() {
    let mut backend = FakeBackend::new().answer(PullPolicy::AllPromised, snapshot(1, &[TEXT], []));
    let result = watch(
        &mut backend,
        async || {
            sleep(Duration::from_millis(50)).await;
            Err(PasteboardError::NotificationsClosed)
        },
        false,
        Language::ZhCn,
        |event| {
            assert!(matches!(event, DeviceEvent::Status(..)));
            ControlFlow::Continue(())
        },
    )
    .await;
    assert_eq!(result, Err(PasteboardError::NotificationsClosed));
    backend.verify();
}
