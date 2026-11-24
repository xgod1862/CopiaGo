use std::sync::atomic::{AtomicU32, Ordering};

use copiago_core::paths::ensure_data_dir;
use tokio::net::TcpListener;

use super::*;
use crate::{WifiProbeOptions, probe_wifi};

#[test]
fn identifier_uses_saved_value_then_python_host_id_then_dns_uuid() {
    let mut fields = Dictionary::new();
    let derived = record_identifier(&fields, || Ok("SampleHost".into())).unwrap();
    assert_eq!(
        derived,
        ("E8F86EDB-0DE0-3813-9377-A7E55F2AC177".into(), true)
    );

    fields.insert("host_identifier".into(), "OriginalHostId".into());
    let from_python = record_identifier(&fields, || panic!("host name was queried")).unwrap();
    assert_eq!(from_python, ("OriginalHostId".into(), true));

    fields.insert("identifier".into(), "StoredId".into());
    let stored = record_identifier(&fields, || panic!("host name was queried")).unwrap();
    assert_eq!(stored, ("StoredId".into(), false));
}

#[test]
fn legacy_record_is_sealed_with_all_fields_and_migrates_once() {
    let fixture = TestRecord::new();
    let mut fields = sample_record();
    let legacy_plist = Value::Dictionary(fields.clone());
    let mut initial = Vec::new();
    legacy_plist.to_writer_xml(&mut initial).unwrap();
    fs::write(&fixture.path, &initial).unwrap();

    let mut record = RemoteRecord::read(&fixture.path).unwrap();
    assert_eq!(record.peer_alt_irk(), Some(&[3; 16]));
    assert_eq!(
        record.persist_identifier(false),
        IdentifierMigration::ReadOnly
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), initial);
    assert_eq!(
        record.persist_identifier(true),
        IdentifierMigration::Written
    );

    let after = fs::read(&fixture.path).unwrap();
    assert!(after.starts_with(dpapi::MAGIC));
    let plaintext = dpapi::unprotect(&after).unwrap();
    let stored = Value::from_reader(Cursor::new(plaintext)).unwrap();
    fields.insert("identifier".into(), "OriginalHostId".into());
    assert!(stored == Value::Dictionary(fields));

    let mut reopened = RemoteRecord::read(&fixture.path).unwrap();
    assert_eq!(reopened.pairing.identifier(), "OriginalHostId");
    assert_eq!(
        reopened.persist_identifier(true),
        IdentifierMigration::AlreadyPresent
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), after);
}

#[test]
fn automatic_discovery_requires_the_python_peer_irk_field() {
    let fixture = TestRecord::new();
    let mut fields = sample_record();
    fields.remove("peer_alt_irk");
    fixture.write_sealed(fields);

    let record = RemoteRecord::read(&fixture.path).unwrap();
    assert!(record.peer_alt_irk().is_none());
}

#[test]
fn new_usb_record_is_sealed_in_the_format_used_for_automatic_wifi() {
    let fixture = TestRecord::new();
    let signing_key = SigningKey::from_bytes(&[7; 32]);
    let pairing = RpPairingFile {
        e_public_key: signing_key.verifying_key(),
        e_private_key: signing_key,
        identifier: "USBTestHost".into(),
        alt_irk: Some(vec![3; 16]),
    };
    save_remote_record(&fixture.path, pairing, "fixture-key".into()).unwrap();
    let sealed = fs::read(&fixture.path).unwrap();
    assert!(sealed.starts_with(dpapi::MAGIC));
    let mut record = RemoteRecord::read(&fixture.path).unwrap();
    assert_eq!(record.peer_alt_irk(), Some(&[3; 16]));
    assert_eq!(record.pairing.identifier(), "USBTestHost");
    assert_eq!(
        record.persist_identifier(false),
        IdentifierMigration::AlreadyPresent
    );
    let fields = record.fields.0.as_dictionary().unwrap();
    assert_eq!(
        fields["remote_unlock_host_key"].as_string(),
        Some("fixture-key")
    );
    assert!(fields.get("alt_irk").is_none());
}

#[test]
fn forgetting_removes_all_remote_records_and_cached_info_only() {
    let fixture = TestRecord::new();
    let directory = fixture.path.parent().unwrap();
    let another = directory.join("REMOTE_another.PLIST");
    let other_file = directory.join("00008030-001234567890ABCD.plist");
    let assets = directory.join("Xcode_iOS_DDI_Cryptex");
    fixture.write_sealed(sample_record());
    fs::write(&another, b"invalid remote record").unwrap();
    fs::write(&other_file, b"lockdown test fixture").unwrap();
    fs::create_dir(&assets).unwrap();
    DeviceInfo {
        model: "iPhone".into(),
        ..DeviceInfo::default()
    }
    .save(&fixture.root)
    .unwrap();
    assert!(matches!(
        find_paired_device(&fixture.root),
        Err(WifiProbeError::MultiplePairingRecords)
    ));

    forget_remote_records(&fixture.root).unwrap();
    forget_remote_records(&fixture.root).unwrap();
    assert!(matches!(
        find_paired_device(&fixture.root),
        Err(WifiProbeError::NoPairingRecord)
    ));
    assert!(!fixture.path.exists());
    assert!(!another.exists());
    assert!(!directory.join("device-info.bin").exists());
    assert_eq!(fs::read(&other_file).unwrap(), b"lockdown test fixture");
    assert!(assets.is_dir());
    fs::remove_file(other_file).unwrap();
    fs::remove_dir(assets).unwrap();
}

#[test]
fn refreshed_or_removed_record_is_preserved() {
    let fixture = TestRecord::new();
    fixture.write_sealed(sample_record());
    let mut record = RemoteRecord::read(&fixture.path).unwrap();

    let mut updated = sample_record();
    updated.insert("identifier".into(), "NewId".into());
    fixture.write_sealed(updated);
    let after = fs::read(&fixture.path).unwrap();
    assert_eq!(
        record.persist_identifier(true),
        IdentifierMigration::WriteFailed
    );
    assert_eq!(fs::read(&fixture.path).unwrap(), after);

    fs::remove_file(&fixture.path).unwrap();
    assert_eq!(
        record.persist_identifier(true),
        IdentifierMigration::WriteFailed
    );
    assert!(!fixture.path.exists());
}

#[tokio::test]
async fn failed_pair_verify_leaves_the_record_unchanged() {
    let fixture = TestRecord::new();
    fixture.write_sealed(sample_record());
    let before = fs::read(&fixture.path).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let phone = tokio::spawn(async move {
        let _ = listener.accept().await.unwrap();
    });

    let result = probe_wifi(
        &fixture.root,
        WifiProbeOptions {
            address: Some(address),
            write_identifier: true,
            ..WifiProbeOptions::default()
        },
    )
    .await;
    assert_eq!(result, Err(WifiProbeError::PairVerify));
    phone.await.unwrap();
    assert_eq!(fs::read(&fixture.path).unwrap(), before);
}

fn sample_record() -> Dictionary {
    let key = SigningKey::from_bytes(&[1; 32]);
    Dictionary::from_iter([
        (
            "public_key",
            Value::Data(key.verifying_key().to_bytes().to_vec()),
        ),
        ("private_key", Value::Data(key.to_bytes().to_vec())),
        ("peer_alt_irk", Value::Data(vec![3; 16])),
        ("alt_irk", Value::Data(vec![4; 16])),
        ("remote_unlock_host_key", Value::Data(vec![5; 16])),
        ("host_identifier", "OriginalHostId".into()),
        ("unknown", Value::Array(vec![true.into(), "extra".into()])),
    ])
}

struct TestRecord {
    root: PathBuf,
    path: PathBuf,
}

impl TestRecord {
    fn new() -> Self {
        static SEQUENCE: AtomicU32 = AtomicU32::new(0);
        let seq = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("copiago-rp-test-{}-{seq}", std::process::id()));
        let directory = root.join(DEVICE_DATA_DIR);
        ensure_data_dir(&root).unwrap();
        ensure_data_dir(&directory).unwrap();
        Self {
            root,
            path: directory.join("remote_test.plist"),
        }
    }

    fn write_sealed(&self, fields: Dictionary) {
        let mut plaintext = Zeroizing::new(Vec::new());
        Value::Dictionary(fields)
            .to_writer_binary(&mut *plaintext)
            .unwrap();
        fs::write(&self.path, dpapi::protect(&plaintext).unwrap()).unwrap();
    }
}

impl Drop for TestRecord {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(self.root.join(DEVICE_DATA_DIR));
        let _ = fs::remove_dir(&self.root);
    }
}
