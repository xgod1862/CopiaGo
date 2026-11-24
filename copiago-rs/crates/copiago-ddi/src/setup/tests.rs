use std::{collections::VecDeque, env, fs, future::pending};

use copiago_core::paths;
use idevice::{ReadWrite, RsdService, mobile_image_mounter::ImageMounter};
use plist::{Dictionary, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream, duplex};

use super::*;
use crate::download::{SourceError, SourceFailure};

mod cryptex;
mod personalized;

fn fields<const N: usize>(items: [(&str, Value); N]) -> Dictionary {
    items
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn service(port: u16) -> idevice::rsd::RsdService {
    idevice::rsd::RsdService {
        entitlement: String::new(),
        port,
        uses_remote_xpc: false,
        features: None,
        service_version: None,
    }
}

fn handshake(version: &str) -> RsdHandshake {
    RsdHandshake {
        protocol_version: 7,
        uuid: String::new(),
        properties: [("OSVersion".to_owned(), Value::from(version))].into(),
        services: [(ImageMounter::rsd_service_name().into_owned(), service(123))].into(),
    }
}

#[test]
fn selects_cryptex_only_when_os_and_service_support_install() {
    for (os, features, expected) in [
        (
            "17.0",
            Some(vec!["CryptexInstall"]),
            ImageKind::Personalized,
        ),
        (
            "26.4",
            Some(vec!["CryptexInstall"]),
            ImageKind::Personalized,
        ),
        ("27.0", Some(vec!["CryptexInstall"]), ImageKind::Cryptex),
        ("27.2.1", None, ImageKind::Cryptex),
        ("27.2.1", Some(vec![]), ImageKind::Cryptex),
        (
            "27.2.1",
            Some(vec!["ReadIdentifiers"]),
            ImageKind::Personalized,
        ),
    ] {
        let mut rsd = handshake(os);
        assert_eq!(mount::image_kind(&rsd), ImageKind::Personalized);
        let mut advertised = service(124);
        advertised.features = features.map(|names| names.into_iter().map(str::to_owned).collect());
        rsd.services
            .insert(mount::CRYPTEX_SERVICE.to_owned(), advertised);
        assert_eq!(mount::image_kind(&rsd), expected);
    }
}

#[derive(Debug)]
struct ScriptedRsd {
    streams: VecDeque<(u16, DuplexStream)>,
}

impl RsdProvider for ScriptedRsd {
    async fn connect_to_service_port(
        &mut self,
        port: u16,
    ) -> Result<Box<dyn ReadWrite>, IdeviceError> {
        let (expected, stream) = self.streams.pop_front().expect("unexpected service open");
        assert_eq!(port, expected);
        Ok(Box::new(stream))
    }
}

async fn read_plist(device: &mut DuplexStream) -> Dictionary {
    let length = device.read_u32().await.unwrap();
    let mut bytes = vec![0; length as usize];
    device.read_exact(&mut bytes).await.unwrap();
    Value::from_reader_xml(bytes.as_slice())
        .unwrap()
        .into_dictionary()
        .unwrap()
}

async fn send_plist(device: &mut DuplexStream, message: Dictionary) {
    let mut body = Vec::new();
    Value::Dictionary(message).to_writer_xml(&mut body).unwrap();
    device.write_u32(body.len() as u32).await.unwrap();
    device.write_all(&body).await.unwrap();
}

fn mounter(steps: Vec<(&'static str, Dictionary)>) -> (ScriptedRsd, task::JoinHandle<()>) {
    let (client, mut device) = duplex(4096);
    let script = task::spawn(async move {
        let request = read_plist(&mut device).await;
        assert_eq!(request["Request"].as_string(), Some("RSDCheckin"));
        send_plist(&mut device, fields([("Request", "RSDCheckin".into())])).await;
        send_plist(&mut device, fields([("Request", "StartService".into())])).await;
        for (command, response) in steps {
            let request = read_plist(&mut device).await;
            assert_eq!(request["Command"].as_string(), Some(command));
            send_plist(&mut device, response).await;
            if command == "ReceiveBytes" {
                let mut bytes =
                    vec![0; request["ImageSize"].as_unsigned_integer().unwrap() as usize];
                device.read_exact(&mut bytes).await.unwrap();
                assert_eq!(bytes, personalized::IMAGE);
                send_plist(&mut device, fields([("Status", "Complete".into())])).await;
            }
        }
    });
    (
        ScriptedRsd {
            streams: [(123, client)].into(),
        },
        script,
    )
}

fn mounter_steps(enabled: bool) -> Vec<(&'static str, Dictionary)> {
    let mut steps = vec![
        ("LookupImage", fields([("ImagePresent", false.into())])),
        (
            "CopyDevices",
            fields([("EntryList", Value::Array(Vec::new()))]),
        ),
    ];
    if enabled {
        steps.push((
            "QueryDeveloperModeStatus",
            fields([("DeveloperModeStatus", true.into())]),
        ));
    }
    steps
}

#[tokio::test]
async fn empty_signature_requires_mounted_entry_or_developer_mode() {
    let root = TempRoot::new();
    for mounted in [true, false] {
        let signature = Value::Array(vec![Value::Data(Vec::new())]);
        let entries = if mounted {
            vec![Value::Dictionary(fields([(
                "DiskImageType",
                "Personalized".into(),
            )]))]
        } else {
            Vec::new()
        };
        let mut steps = vec![
            ("LookupImage", fields([("ImageSignature", signature)])),
            (
                "CopyDevices",
                fields([("EntryList", Value::Array(entries))]),
            ),
        ];
        if !mounted {
            steps.push((
                "QueryDeveloperModeStatus",
                fields([("DeveloperModeStatus", false.into())]),
            ));
        }
        let (mut provider, script) = mounter(steps);
        let mut rsd = handshake("27.0");
        let mut updates = Vec::new();
        let result = prepare_developer_services(&mut provider, &mut rsd, &root.0, |event| {
            updates.push(event);
            ControlFlow::Continue(())
        })
        .await;
        assert_eq!(
            result,
            if mounted {
                Ok(())
            } else {
                Err(SetupError::DeveloperModeRequired)
            }
        );
        assert_eq!(updates, [SetupUpdate::Checking]);
        assert_eq!(fs::read_dir(&root.0).unwrap().count(), 0);
        script.await.unwrap();
    }
}

#[test]
fn ticket_parser_checks_success_accepts_fallback_and_rejects_empty_ticket() {
    fn response(tickets: Dictionary) -> Vec<u8> {
        let mut response = b"STATUS=0&MESSAGE=SUCCESS&REQUEST_STRING=".to_vec();
        Value::Dictionary(tickets)
            .to_writer_xml(&mut response)
            .unwrap();
        response
    }
    let both = response(fields([
        ("Cryptex1,Ticket", Value::Data(vec![1, 2])),
        ("ApImg4Ticket", Value::Data(vec![3, 4])),
    ]));
    assert_eq!(
        signing::decode_ticket(&both, ImageKind::Cryptex),
        Ok(vec![1, 2])
    );
    assert_eq!(
        signing::decode_ticket(&both, ImageKind::Personalized),
        Ok(vec![3, 4])
    );
    let fallback = response(fields([
        ("Cryptex1,Ticket", Value::Data(Vec::new())),
        ("ApImg4Ticket", Value::Data(vec![3, 4])),
    ]));
    assert_eq!(
        signing::decode_ticket(&fallback, ImageKind::Cryptex),
        Ok(vec![3, 4])
    );
    assert_eq!(
        signing::decode_ticket(&response(Dictionary::new()), ImageKind::Personalized),
        Err(StepError::Kind("TSSError")),
    );
    assert_eq!(
        signing::decode_ticket(b"STATUS=94&MESSAGE=denied", ImageKind::Cryptex),
        Err(StepError::Kind("TSSError")),
    );
}

#[tokio::test(start_paused = true)]
async fn stage_deadlines_and_worker_cancellation() {
    let started = tokio::time::Instant::now();
    assert_eq!(
        within(SIGN_TIMEOUT, pending::<Result<(), StepError>>()).await,
        Err(StepError::Timeout)
    );
    assert_eq!(started.elapsed(), Duration::from_secs(45));
    let started = tokio::time::Instant::now();
    assert_eq!(
        within(INSTALL_TIMEOUT, pending::<Result<(), StepError>>()).await,
        Err(StepError::Timeout)
    );
    assert_eq!(started.elapsed(), Duration::from_secs(240));
    let flag = Arc::new(AtomicBool::new(false));
    let guard = CancelDownload(Arc::clone(&flag));
    assert!(!flag.load(Ordering::Relaxed));
    drop(guard);
    assert!(flag.load(Ordering::Relaxed));
}

#[test]
fn python_log_keeps_field_order_and_rotates_without_raw_errors() {
    let root = TempRoot::new();
    let log_file = root.0.join("developer-setup.log");
    let rolled = root.0.join("developer-setup.log.1");
    fs::write(&log_file, vec![b'x'; 256 * 1024]).unwrap();
    fs::write(&rolled, b"older").unwrap();
    let stats = DownloadStats {
        file: "Image.dmg",
        index: 2,
        count: 3,
        received: 4096,
        total: 7168,
        resumed: 2048,
        fetched: 2048,
        restarts: 1,
        http_status: Some(206),
        source: Some("raw.githubusercontent.com"),
        failures: vec![SourceFailure {
            source: "ghfast.top",
            reason: SourceError::Transport(copiago_win::http::Error::Timeout),
        }],
    };
    let detail = IdeviceError::InternalError("test/private/device-identifier".into());
    log::record(
        &root.0,
        DdiStage::Download,
        2.0,
        "failed",
        Some(StepError::HttpStatus(503)),
        Some(&stats),
    )
    .unwrap();
    log::record(
        &root.0,
        DdiStage::Install,
        12.3,
        "failed",
        Some(detail.into()),
        None,
    )
    .unwrap();
    let input = fs::read_to_string(&log_file).unwrap();
    let (download, install) = input.split_once("\r\n").unwrap();
    assert_eq!(
        &download[19..],
        " download failed elapsed=2.0s error=HTTPError[503] file=Image.dmg part=2/3 received=4096 total=7168 resumed=2048 fetched=2048 restarts=1 http=206 speed=1KB/s source=raw.githubusercontent.com failures=ghfast.top:ConnectionError",
    );
    assert_eq!(
        &install[19..],
        " install failed elapsed=12.3s error=IdeviceError\r\n"
    );
    assert_eq!(fs::metadata(rolled).unwrap().len(), 256 * 1024);
}

#[tokio::test]
#[ignore = "contacts Apple over authenticated TLS without any personalization fields"]
async fn apple_tss_returns_a_business_response_without_device_identifiers() {
    let result = within(
        SIGN_TIMEOUT,
        signing::request_ticket(Dictionary::new(), ImageKind::Cryptex),
    )
    .await;
    assert_eq!(result, Err(StepError::Kind("TSSError")));
}

struct TempRoot(PathBuf);

impl TempRoot {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let temp = env::temp_dir().canonicalize().unwrap();
        let base = env::var_os("ICB_DATA_DIR")
            .and_then(|path| fs::canonicalize(path).ok())
            .filter(|path| path.starts_with(&temp))
            .unwrap_or(temp);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!("copiago-setup-test-{}-{id}", std::process::id()));
        paths::ensure_data_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
