use std::io::Write;

use super::*;
use crate::test_support::TestDir;

#[test]
fn sanitizes_windows_names_and_uses_the_source_extension() {
    for (input, expected) in [
        ("a<b>:c?.pdf", "a_b__c_.pdf"),
        (" ..\\报告\u{0000}.PDF ", "_报告_.PDF"),
        ("无扩展名", "无扩展名.bin"),
        ("CON.txt", "_CON.txt"),
        ("nUl.tar.gz", "_nUl.tar.gz"),
        ("LPT².txt", "_LPT².txt"),
        ("AUX .txt", "_AUX .txt"),
    ] {
        assert_eq!(safe_name(input, "bin").as_deref(), Some(expected));
    }
    assert_eq!(safe_name(" . . ", "bin"), None);
    let long = "名".repeat(121);
    assert_eq!(
        safe_name(&long, "pdf").unwrap(),
        format!("{}.pdf", "名".repeat(120))
    );
    for (language, prefix) in [
        (Language::ZhCn, "iPhone 文件 "),
        (Language::En, "iPhone file "),
    ] {
        let name = fallback_name(language, "pptx");
        let stamp = name
            .strip_prefix(prefix)
            .unwrap()
            .strip_suffix(".pptx")
            .unwrap();
        let (date, time) = stamp.split_once('-').unwrap();
        assert_eq!((date.len(), time.len()), (8, 6));
        assert!(
            date.bytes()
                .chain(time.bytes())
                .all(|digit| digit.is_ascii_digit())
        );
    }
}

#[test]
fn matches_file_confirmation_and_limit_boundaries() {
    use Confirmation::*;
    use OfferDecision::*;

    let mut settings = Settings::default();
    let limit = 20 * MIB;
    assert_eq!(decide_offer(&settings, 256 * 1024), Receive);
    assert_eq!(decide_offer(&settings, limit), Receive);
    assert_eq!(decide_offer(&settings, limit + 1), Confirm(OverThreshold));
    assert_eq!(decide_offer(&settings, 0), Confirm(UnknownSize));
    assert_eq!(
        decide_offer(&settings, MAX_FILE_BYTES),
        Confirm(OverThreshold)
    );
    assert_eq!(decide_offer(&settings, MAX_FILE_BYTES + 1), TooLarge);
    settings.file_ask_mb = 0;
    assert_eq!(decide_offer(&settings, 0), Receive);
    assert_eq!(decide_offer(&settings, MAX_FILE_BYTES), Receive);
    settings.pull = false;
    assert_eq!(decide_offer(&settings, 1), Confirm(AutoSyncOff));
    assert_eq!(decide_offer(&settings, MAX_FILE_BYTES + 1), TooLarge);
    settings.pull_types = vec![SyncKind::Text];
    assert_eq!(decide_offer(&settings, MAX_FILE_BYTES + 1), Ignore);
}

#[test]
fn transfers_preserve_the_filename_without_sharing_storage() {
    let root = TestDir::new();
    let name = "报告.pdf";
    let mut paths = Vec::new();
    for content in [b"one", b"two"] {
        let mut file = StagedFile::new(root.path(), name.to_owned()).unwrap();
        file.writer().write_all(content).unwrap();
        let synced = file.sync(content.len() as u64).unwrap();
        assert!(
            root.path()
                .join(RECEIVED)
                .read_dir()
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| paths.contains(&entry.path().join(name))
                    || entry
                        .file_name()
                        .as_encoded_bytes()
                        .starts_with(STAGING_PREFIX.as_bytes()))
        );
        let offered = synced.publish().unwrap();
        assert!(offered.path().is_absolute());
        assert_eq!(offered.path().file_name().unwrap(), name);
        assert_eq!(fs::read(offered.path()).unwrap(), content);
        paths.push(offered.path().to_path_buf());
        offered.keep();
    }
    assert_ne!(paths[0], paths[1]);
    assert_eq!(fs::read(&paths[0]).unwrap(), b"one");
}

#[test]
fn discards_partial_invalid_and_unclaimed_transfers() {
    let root = TestDir::new();
    for invalid in [
        "../file.pdf",
        "C:file.pdf",
        "NUL.tar.gz",
        "file.pdf. ",
        "..",
        "file",
    ] {
        assert!(StagedFile::new(root.path(), invalid.to_owned()).is_err());
    }
    let new_file = || StagedFile::new(root.path(), "example.bin".to_owned()).unwrap();
    let mut partial = new_file();
    partial.writer().write_all(b"partial").unwrap();
    drop(partial);
    assert!(new_file().sync(1).is_err());
    let unclaimed = new_file().sync(0).unwrap().publish().unwrap();
    let path = unclaimed.path().to_path_buf();
    drop(unclaimed);
    assert!(!path.exists());
    assert_eq!(fs::read_dir(root.path().join(RECEIVED)).unwrap().count(), 0);
}

#[test]
fn cleans_up_at_start_after_24h_then_evicts_the_oldest_over_budget() {
    let root = TestDir::new();
    let received = root.path().join(RECEIVED);
    cleanup(root.path()).unwrap();
    assert!(!received.exists());
    fs::create_dir(&received).unwrap();
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000);
    let create = |name, age| {
        let dir = received.join(name);
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join("sample.bin"), [0, 1, 2]).unwrap();
        set_modified(&dir, now - age).unwrap();
        dir
    };
    let old = create("old-python-transfer", KEEP_FOR + Duration::from_secs(1));
    let expired_stage = create(".incoming-abandoned", KEEP_FOR + Duration::from_secs(1));
    let at_cutoff = create("at-24h", KEEP_FOR);
    let newest = create("new", Duration::from_secs(1));
    let staging = create(".incoming-active", Duration::from_secs(1));
    let root_file = received.join("notes.txt");
    fs::write(&root_file, [0; 10]).unwrap();

    cleanup_at(&received, now, 6).unwrap();
    assert!(!old.exists());
    assert!(!expired_stage.exists());
    assert!(at_cutoff.exists());
    cleanup_at(&received, now, 3).unwrap();
    assert!(!at_cutoff.exists());
    assert!(newest.exists());
    assert!(staging.exists());
    assert!(root_file.exists());
}
