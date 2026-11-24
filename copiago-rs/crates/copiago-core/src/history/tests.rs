use std::env;
use std::process::Command;
use std::sync::Barrier;

use crate::clip::FileOffer;
use crate::settings::{Settings, SettingsChange};
use crate::test_support::TestDir;

use super::*;

fn history(dir: &TestDir) -> History {
    History::new(&dir.path().join("历史"))
}

fn add(history: &History, source: Source, clip: &Clip) -> bool {
    history
        .add(source, clip, |_, _, _| panic!("unexpected image thumbnail"))
        .unwrap()
}

fn add_text(history: &History, source: Source, text: &str) -> bool {
    add(history, source, &Clip::Text(text.to_owned()))
}

fn file(name: &str, data: Option<Vec<u8>>) -> Clip {
    Clip::File(FileOffer {
        name: name.into(),
        uti: "public.data".into(),
        size: 0,
        change: None,
        data,
    })
}

#[test]
fn list_lookup_and_deletion_leave_a_missing_database_absent() {
    let dir = TestDir::new();
    let history = history(&dir);
    assert!(history.entries(Source::Phone).unwrap().is_empty());
    assert!(history.get(1).unwrap().is_none());
    history.remove(1).unwrap();
    history.clear(Some(Source::Pc)).unwrap();
    history.clear(None).unwrap();
    assert!(!parent_dir(&history.path).exists());
}

#[test]
fn duplicates_match_only_the_latest_kind_and_bytes_within_each_source() {
    let dir = TestDir::new();
    let history = history(&dir);
    assert!(add_text(&history, Source::Phone, "相同😀"));
    assert!(add_text(&history, Source::Pc, "相同😀"));
    assert!(add_text(&history, Source::Phone, "相同😀"));
    assert_eq!(history.entries(Source::Phone).unwrap().len(), 1);
    assert_eq!(history.entries(Source::Pc).unwrap().len(), 1);

    let raw = "相同😀".as_bytes().to_vec();
    assert!(add(
        &history,
        Source::Phone,
        &file("one.txt", Some(raw.clone()))
    ));
    assert!(add(&history, Source::Phone, &file("two.txt", Some(raw))));
    assert!(add_text(&history, Source::Phone, "相同😀"));
    let entries = history.entries(Source::Phone).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].kind, "text");
    assert_eq!(entries[1].kind, "file");
    assert_eq!(entries[1].preview, "one.txt\n10".as_bytes());
    assert!(entries[0].id > entries[1].id && entries[1].id > entries[2].id);
}

#[test]
fn previews_match_python_whitespace_and_character_truncation() {
    let text = format!("\t{}\u{1c}\u{3000}最后", "🦀".repeat(95));
    assert_eq!(
        text_preview(&text),
        format!("{} ", "🦀".repeat(95)).as_bytes()
    );

    let dir = TestDir::new();
    let history = history(&dir);
    assert!(add_text(&history, Source::Phone, " \t\u{1c}\n"));
    let entries = history.entries(Source::Phone).unwrap();
    assert_eq!(entries.len(), 1);
    assert!(entries[0].preview.is_empty());
}

#[test]
fn pruning_enforces_fifty_items_per_source() {
    let dir = TestDir::new();
    let history = history(&dir);
    for index in 0..=MAX_ITEMS {
        assert!(add_text(&history, Source::Phone, &format!("phone {index}")));
        assert!(add_text(&history, Source::Pc, &format!("pc {index}")));
    }
    for (source, name) in [(Source::Phone, "phone"), (Source::Pc, "pc")] {
        let entries = history.entries(source).unwrap();
        assert_eq!(entries.len(), MAX_ITEMS);
        assert_eq!(entries[0].preview, format!("{name} 50").as_bytes());
        assert_eq!(
            entries[MAX_ITEMS - 1].preview,
            format!("{name} 1").as_bytes()
        );
    }
}

#[test]
fn pruning_counts_both_blob_columns_against_each_sources_byte_budget() {
    let dir = TestDir::new();
    let history = history(&dir);
    let db = history.connect(true).unwrap();
    db.transaction(|db| {
        db.execute(
            c"INSERT INTO items (created, kind, preview, payload, source)
              VALUES ('', 'image', x'00', x'00', 'pc')",
            &[],
        )?;
        // Create four rows whose combined BLOB sizes are exactly 128 MiB.
        for _ in 0..4 {
            db.execute(
                c"INSERT INTO items (created, kind, preview, payload, source)
                  VALUES ('', 'image', x'00', zeroblob(?), 'phone')",
                &[Param::Integer((MAX_BYTES / 4) - 1)],
            )?;
        }
        prune(db, Source::Phone)
    })
    .unwrap();
    let count = |source: Source| {
        db.prepare(
            c"SELECT count(*) FROM items WHERE source = ?",
            &[Param::Text(source.as_str())],
        )
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .integer(0)
        .unwrap()
    };
    assert_eq!(count(Source::Phone), 4);
    assert!(add_text(&history, Source::Phone, "over the limit"));
    assert_eq!(count(Source::Phone), 4);
    assert_eq!(count(Source::Pc), 1);
    assert!(history.get(2).unwrap().is_none());
}

#[test]
fn checks_raw_item_size_before_preparing_a_preview() {
    let dir = TestDir::new();
    let history = history(&dir);
    assert!(!add_text(&history, Source::Phone, ""));
    assert!(!add(&history, Source::Pc, &file("pending.txt", None)));
    assert!(!add(
        &history,
        Source::Phone,
        &Clip::Image {
            bytes: vec![0; MAX_ITEM_BYTES + 1],
            uti: "public.png".into()
        },
    ));
    assert!(!history.path.exists());
    assert!(add(
        &history,
        Source::Phone,
        &file("large.bin", Some(vec![0; MAX_ITEM_BYTES]))
    ));
    assert_eq!(
        history.entries(Source::Phone).unwrap()[0].preview,
        b"large.bin\n33554432"
    );
    assert!(add(
        &history,
        Source::Phone,
        &file("empty.bin", Some(Vec::new()))
    ));
    let id = history.entries(Source::Phone).unwrap()[0].id;
    assert!(
        history.get(id).unwrap()
            == Some(StoredClip::File {
                name: "empty.bin".into(),
                bytes: Vec::new()
            })
    );
}

#[test]
fn disabling_recording_preserves_history_and_explicit_deletes_are_scoped() {
    let dir = TestDir::new();
    let history = history(&dir);
    let data_dir = parent_dir(&history.path);
    Settings::update(data_dir, SettingsChange::History(true)).unwrap();
    Settings::update(data_dir, SettingsChange::PcHistory(true)).unwrap();
    add_text(&history, Source::Phone, "one");
    add_text(&history, Source::Phone, "two");
    add_text(&history, Source::Pc, "three");

    let settings = Settings::update(data_dir, SettingsChange::History(false)).unwrap();
    assert!(!settings.history && settings.pc_history);
    assert_eq!(history.entries(Source::Phone).unwrap().len(), 2);
    let settings = Settings::update(data_dir, SettingsChange::PcHistory(false)).unwrap();
    assert!(!settings.pc_history);
    assert_eq!(history.entries(Source::Pc).unwrap().len(), 1);

    let id = history.entries(Source::Phone).unwrap()[0].id;
    history.remove(id).unwrap();
    assert!(history.get(id).unwrap().is_none());
    assert_eq!(history.entries(Source::Phone).unwrap().len(), 1);
    history.clear(Some(Source::Phone)).unwrap();
    assert!(history.entries(Source::Phone).unwrap().is_empty());
    assert_eq!(history.entries(Source::Pc).unwrap().len(), 1);
    history.clear(None).unwrap();
    assert!(history.entries(Source::Pc).unwrap().is_empty());
}

#[test]
fn concurrent_writes_migrate_the_legacy_schema_once_and_deduplicate() {
    let dir = TestDir::new();
    let history = history(&dir);
    crate::paths::ensure_data_dir(parent_dir(&history.path)).unwrap();
    let legacy = Connection::open(&history.path, true).unwrap();
    legacy.execute(
        c"CREATE TABLE items (id INTEGER PRIMARY KEY, created TEXT NOT NULL, kind TEXT NOT NULL,
                              preview BLOB NOT NULL, payload BLOB NOT NULL)",
        &[],
    ).unwrap();
    let preview = protect_history(b"legacy preview").unwrap();
    let payload = protect_history(b"legacy payload").unwrap();
    legacy
        .execute(
            c"INSERT INTO items VALUES (10, '2025-01-02T03:04', 'text', ?, ?)",
            &[Param::Blob(&preview), Param::Blob(&payload)],
        )
        .unwrap();
    drop(legacy);

    let barrier = Barrier::new(4);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                barrier.wait();
                assert!(add_text(&history, Source::Pc, "new item"));
            });
        }
    });
    let entries = history.entries(Source::Phone).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, 10);
    assert_eq!(entries[0].created, "2025-01-02T03:04");
    assert_eq!(entries[0].preview, b"legacy preview");
    assert!(history.get(10).unwrap() == Some(StoredClip::Text("legacy payload".into())));
    assert_eq!(history.entries(Source::Pc).unwrap().len(), 1);
}

#[test]
fn secure_delete_is_set_and_a_failed_prune_rolls_back_the_insert() {
    let dir = TestDir::new();
    let history = history(&dir);
    let db = history.connect(true).unwrap();
    let mut pragma = db.prepare(c"PRAGMA secure_delete", &[]).unwrap();
    assert_eq!(pragma.next().unwrap().unwrap().integer(0).unwrap(), 1);
    drop(pragma);
    db.execute(
        c"WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n < ?)
          INSERT INTO items (id, created, kind, preview, payload)
          SELECT n, '', 'image', x'00', x'00' FROM numbers",
        &[Param::Integer(MAX_ITEMS as i64)],
    )
    .unwrap();
    db.execute(c"CREATE TRIGGER reject_delete BEFORE DELETE ON items BEGIN SELECT RAISE(ABORT, 'blocked'); END", &[]).unwrap();
    assert!(
        history
            .add(
                Source::Phone,
                &Clip::Text("new item".into()),
                |_, _, _| unreachable!()
            )
            .is_err()
    );
    assert!(history.get(MAX_ITEMS as i64 + 1).unwrap().is_none());
    assert!(history.entries(Source::Phone).unwrap().is_empty());
}

#[test]
#[ignore = "requires the Python CopiaGo environment and PYTHONPATH"]
fn python_history_interoperability() {
    let dir = TestDir::new();
    let history = history(&dir);
    let data_dir = parent_dir(&history.path);
    crate::paths::ensure_data_dir(data_dir).unwrap();
    run_python(data_dir, "seed");

    let entries = history.entries(Source::Phone).unwrap();
    assert_eq!(entries.len(), 2);
    let image = &entries[0];
    assert_eq!(image.kind, "image");
    let Some(StoredClip::Image(bytes)) = history.get(image.id).unwrap() else {
        panic!("missing Python image");
    };
    assert_eq!(png_size(&bytes), (240, 120));
    assert_eq!(png_size(&image.preview), (88, 44));
    let text = &entries[1];
    assert_eq!(text.kind, "text");
    assert_eq!(text.preview, "来自 Python😀 保留原文".as_bytes());
    assert!(
        history.get(text.id).unwrap()
            == Some(StoredClip::Text(
                " \u{3000}来自 Python😀\u{1c} 保留原文 \n".into()
            ))
    );
    let entries = history.entries(Source::Pc).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].preview, "样例😀.bin\n16".as_bytes());
    assert!(
        history.get(entries[0].id).unwrap()
            == Some(StoredClip::File {
                name: "样例😀.bin".into(),
                bytes: b"\0payload-python\xff".to_vec(),
            })
    );

    assert!(add_text(
        &history,
        Source::Phone,
        "\t来自 Rust 🦀 \n 第二行  "
    ));
    let clip = Clip::Image {
        bytes,
        uti: "public.png".into(),
    };
    assert!(
        history
            .add(Source::Phone, &clip, |raw, width, height| {
                assert_eq!(png_size(raw), (240, 120));
                assert_eq!((width, height), (88, 56));
                Ok(image.preview.clone())
            })
            .unwrap()
    );
    assert!(add(
        &history,
        Source::Pc,
        &file("资料🦀.bin", Some(b"\x00Rust file\xff".to_vec()))
    ));
    run_python(data_dir, "verify");
}

fn png_size(bytes: &[u8]) -> (u32, u32) {
    assert!(bytes.starts_with(b"\x89PNG\r\n\x1a\n"));
    let width = u32::from_be_bytes(bytes[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(bytes[20..24].try_into().unwrap());
    (width, height)
}

fn run_python(data_dir: &Path, action: &str) {
    let interpreter = env::var_os("COPIAGO_HISTORY_PYTHON").expect("set COPIAGO_HISTORY_PYTHON");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/history_interop.py");
    let output = Command::new(interpreter)
        .arg("-B")
        .arg(fixture)
        .arg(action)
        .env("ICB_DATA_DIR", data_dir)
        .output()
        .expect("run Python history fixture");
    assert!(
        output.status.success(),
        "Python history {action} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
