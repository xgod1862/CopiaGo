use std::{
    fs,
    future::pending,
    io::Read,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};

use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, sync::Notify};

use crate::pasteboard::file_transfer::{Method, simulation};

use super::*;

struct TestRoot(PathBuf);

impl TestRoot {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("copiago-file-test-{}-{id}", std::process::id()));
        copiago_core::paths::ensure_data_dir(&path).unwrap();
        Self(path)
    }

    fn is_empty(&self) -> bool {
        self.0.join("received").read_dir().map_or_else(
            |error| error.kind() == io::ErrorKind::NotFound,
            |mut dirs| dirs.next().is_none(),
        )
    }
}

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn nameless_file_uses_the_current_saved_language_without_a_new_session() {
    use copiago_core::{
        i18n::{Language, LanguagePreference},
        settings::{Settings, SettingsChange},
    };

    use crate::pasteboard::{WatchOptions, content::Snapshot, file_offer};

    let root = TestRoot::new();
    let options = WatchOptions {
        data_dir: &root.0,
        wireless: true,
        language: Language::En,
        follow_saved_language: true,
    };
    let describe = |options: &WatchOptions| {
        file_offer::describe(Snapshot::default(), "com.adobe.pdf".into(), None, || {
            options.filename_language()
        })
    };
    Settings::update(&root.0, SettingsChange::Language(LanguagePreference::En)).unwrap();
    let english = describe(&options);
    assert!(english.name.starts_with("iPhone file ") && english.name.ends_with(".pdf"));

    Settings::update(&root.0, SettingsChange::Language(LanguagePreference::ZhCn)).unwrap();
    let chinese = describe(&options);
    assert!(chinese.name.starts_with("iPhone 文件 ") && chinese.name.ends_with(".pdf"));
    assert!(english.name.starts_with("iPhone file "));

    let fixed = WatchOptions {
        follow_saved_language: false,
        ..options
    };
    assert!(describe(&fixed).name.starts_with("iPhone file "));
}

fn offer(size: u64) -> FileOffer {
    FileOffer {
        name: "test.zip".to_owned(),
        uti: "public.zip-archive".to_owned(),
        size,
        change: None,
        data: None,
    }
}

#[tokio::test]
async fn publishes_the_complete_streamed_file_after_validating_and_syncing() {
    check_streamed_file(300_001).await;
}

#[tokio::test]
#[ignore = "writes and verifies 512 MiB of synthetic data in the temporary directory"]
async fn streamed_file_accepts_the_exact_file_size_limit() {
    check_streamed_file(MAX_FILE_BYTES).await;
}

async fn check_streamed_file(size: u64) {
    let root = TestRoot::new();
    let mut digest = None;
    let started = Instant::now();
    let received = receive(&root.0, offer(size), async |offer, dest| {
        let (bytes, sha) =
            simulation::run(Method::Threshold, offer.size, dest, &mut |_| {}).await?;
        digest = Some(sha);
        Ok(bytes)
    })
    .await
    .unwrap();
    let receive_ms = started.elapsed().as_millis();
    let mut file = File::open(received.path()).unwrap();
    assert_eq!(received.bytes(), size);
    assert_eq!(file.metadata().unwrap().len(), size);
    let mut sha = Sha256::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        let read = file.read(&mut chunk).unwrap();
        if read == 0 {
            break;
        }
        sha.update(&chunk[..read]);
    }
    assert_eq!(<[u8; 32]>::from(sha.finalize()), digest.unwrap());
    println!(
        "bytes={size} receive_ms={receive_ms} total_ms={}",
        started.elapsed().as_millis()
    );
}

#[tokio::test]
async fn rejects_an_oversized_offer_before_creating_a_destination() {
    let root = TestRoot::new();
    let result = receive(&root.0, offer(MAX_FILE_BYTES + 1), async |_, _| {
        panic!("oversized offer must not start a transfer");
    })
    .await;
    assert!(matches!(result, Err(FileTransferError::TooLarge)));
    assert!(!root.0.join("received").exists());
}

#[tokio::test]
async fn failed_or_cancelled_transfer_never_leaves_a_partial_file() {
    let root = TestRoot::new();
    let result = receive(&root.0, offer(10), async |_, dest| {
        dest.write_all(b"partial").await.unwrap();
        Err(FileTransferError::OfferExpired)
    })
    .await;
    assert!(matches!(result, Err(FileTransferError::OfferExpired)));
    assert!(root.is_empty());

    let started = Notify::new();
    tokio::select! {
        result = receive(&root.0, offer(10), async |_, dest| {
            dest.write_all(b"partial").await.unwrap();
            started.notify_one();
            pending().await
        }) => panic!("transfer unexpectedly completed with {}", if result.is_ok() { "success" } else { "an error" }),
        () = started.notified() => {}
    };
    assert!(root.is_empty());
}
