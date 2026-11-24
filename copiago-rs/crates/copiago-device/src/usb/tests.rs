use super::*;
use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicU32, Ordering};

use idevice::usbmuxd::{
    RawPacket,
    server::{UsbmuxdServerRequest, UsbmuxdServerResponse},
};
use plist::{Dictionary, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    runtime::Builder,
    task::JoinHandle,
};

const UDID: &str = "00008030-001234567890ABCD";
const PAIRING_FIXTURE: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>DeviceCertificate</key><data>AA==</data>
    <key>HostCertificate</key><data>AA==</data>
    <key>RootCertificate</key><data>AA==</data>
    <key>HostPrivateKey</key><data>AA==</data>
    <key>RootPrivateKey</key><data>AA==</data>
    <key>SystemBUID</key><string>test-buid</string>
    <key>HostID</key><string>test-host</string>
    <key>WiFiMACAddress</key><string>00:00:00:00:00:00</string>
</dict>
</plist>"#;

fn run(test: impl Future<Output = ()>) {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            timeout(Duration::from_secs(10), test).await.unwrap();
        });
}

async fn mock_mux(
    respond: impl FnOnce(UsbmuxdServerRequest) -> Vec<UsbmuxdServerResponse> + Send + 'static,
) -> (UsbmuxdAddr, JoinHandle<()>) {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
    let addr = UsbmuxdAddr::TcpSocket(listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let size = socket.read_u32_le().await.unwrap();
        assert!((16..4096).contains(&size));
        let mut packet = vec![0; size as usize];
        packet[..4].copy_from_slice(&size.to_le_bytes());
        socket.read_exact(&mut packet[4..]).await.unwrap();
        let request = RawPacket::try_from(packet.as_slice()).unwrap();
        for response in respond(UsbmuxdServerRequest::decode(&request).unwrap()) {
            socket
                .write_all(&Vec::from(response.into_packet(request.tag)))
                .await
                .unwrap();
        }
    });
    (addr, task)
}

fn mux_device(device_id: u32, transport: &str) -> Value {
    mux_device_for(device_id, transport, UDID)
}

fn mux_device_for(device_id: u32, transport: &str, serial: &str) -> Value {
    let properties = Dictionary::from_iter([
        ("ConnectionType", transport.into()),
        ("SerialNumber", serial.into()),
        ("NetworkAddress", Value::Data(vec![2; 16])),
    ]);
    Value::Dictionary(Dictionary::from_iter([
        ("DeviceID", device_id.into()),
        ("Properties", Value::Dictionary(properties)),
    ]))
}

async fn mock_device_list(devices: Vec<Value>) -> (UsbmuxdAddr, JoinHandle<()>) {
    mock_mux(|request| {
        assert!(matches!(request, UsbmuxdServerRequest::ListDevices));
        vec![UsbmuxdServerResponse::DeviceList(devices)]
    })
    .await
}

#[test]
fn usb_list_filters_other_transports() {
    run(async {
        let (addr, task) = mock_device_list(vec![
            mux_device(4, "Network"),
            mux_device(5, "USB"),
            mux_device(6, "Unknown"),
        ])
        .await;
        let devices = list_usb_devices(&addr).await.unwrap();
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].device_id, 5);
        task.await.unwrap();
    });
}

#[test]
fn paired_usb_selection_requires_both_serial_and_usb() {
    run(async {
        const OTHER: &str = "00008030-001234567890ABCE";
        let device = PairedDevice {
            serial: UDID.to_owned(),
            path: PathBuf::new(),
        };
        for (devices, expected) in [
            (
                vec![mux_device(1, "Network"), mux_device_for(2, "USB", OTHER)],
                None,
            ),
            (
                vec![
                    mux_device(1, "Network"),
                    mux_device_for(2, "USB", OTHER),
                    mux_device(3, "USB"),
                ],
                Some(3),
            ),
            (vec![], None),
        ] {
            let (addr, task) = mock_device_list(devices).await;
            assert_eq!(paired_usb_id(&addr, &device).await, Ok(expected));
            task.await.unwrap();
        }
    });
}

#[test]
fn usbmux_listen_relays_removal_and_leaves_attaches_to_the_fallback_lookup() {
    run(async {
        let (addr, server) = mock_mux(|request| {
            assert!(matches!(request, UsbmuxdServerRequest::Listen));
            let mut attach = mux_device(5, "USB").into_dictionary().unwrap();
            attach.insert("MessageType".to_owned(), "Attached".into());
            vec![
                UsbmuxdServerResponse::Result(0),
                UsbmuxdServerResponse::Attached(attach),
                UsbmuxdServerResponse::Detached(5),
            ]
        })
        .await;
        let (detached, mut changes) = mpsc::channel(4);
        assert!(listen_once(&addr, &detached).await.is_err()); // test server closed after the events
        assert_eq!(changes.try_recv(), Ok(5));
        assert_eq!(changes.try_recv(), Err(mpsc::error::TryRecvError::Empty));
        server.await.unwrap();
    });
}

#[test]
fn probe_requires_one_usb_device() {
    run(async {
        let cases = [
            (vec![mux_device(4, "Network")], UsbProbeError::NoUsbDevice),
            (
                vec![mux_device(4, "USB"), mux_device(5, "USB")],
                UsbProbeError::MultipleUsbDevices(2),
            ),
        ];
        for (devices, expected) in cases {
            let (addr, task) = mock_device_list(devices).await;
            assert_eq!(probe_on_addr(&addr, Path::new(".")).await, Err(expected));
            task.await.unwrap();
        }
    });
}

#[test]
fn pairing_stops_before_prompt_or_writes_when_usb_selection_is_ambiguous() {
    run(async {
        let cases = [
            (vec![mux_device(4, "Network")], UsbProbeError::NoUsbDevice),
            (
                vec![mux_device(4, "USB"), mux_device(5, "USB")],
                UsbProbeError::MultipleUsbDevices(2),
            ),
        ];
        let local = LocalFixture::new();
        for (devices, expected) in cases {
            let (addr, task) = mock_device_list(devices).await;
            let result = pair_over_usb(&addr, &local.root, |_| panic!("prompted for trust")).await;
            assert_eq!(result, Err(PairingError::Usb(expected)));
            assert!(
                fs::read_dir(local.root.join(DEVICE_DATA_DIR))
                    .unwrap()
                    .next()
                    .is_none()
            );
            task.await.unwrap();
        }
    });
}

async fn mock_pair_record(response: UsbmuxdServerResponse) -> (UsbmuxdProvider, JoinHandle<()>) {
    let (addr, task) = mock_mux(|request| {
        assert!(matches!(
            request,
            UsbmuxdServerRequest::ReadPairRecord { pair_record_id } if pair_record_id == UDID
        ));
        vec![response]
    })
    .await;
    let device = UsbmuxdDevice {
        connection_type: Connection::Usb,
        udid: UDID.into(),
        device_id: 5,
    };
    (device.to_provider(addr, "CopiaGo test"), task)
}

#[test]
fn pairing_prefers_usbmuxd_when_both_records_are_available() {
    run(async {
        let local = LocalFixture::new();
        fs::write(&local.file, PAIRING_FIXTURE).unwrap();
        let (provider, task) =
            mock_pair_record(UsbmuxdServerResponse::PairRecord(PAIRING_FIXTURE.to_vec())).await;
        let (_, source) = pairing_record(&provider, &local.root).await.unwrap();
        assert_eq!(source, PairingRecordSource::Usbmuxd);
        task.await.unwrap();
    });
}

#[test]
fn pairing_falls_back_to_plaintext_and_dpapi_records_without_rewriting() {
    run(async {
        let local = LocalFixture::new();
        for bytes in [
            PAIRING_FIXTURE.to_vec(),
            dpapi::protect(PAIRING_FIXTURE).unwrap(),
        ] {
            fs::write(&local.file, &bytes).unwrap();
            let (provider, task) = mock_pair_record(UsbmuxdServerResponse::Result(2)).await;
            let (_, source) = pairing_record(&provider, &local.root).await.unwrap();
            assert_eq!(source, PairingRecordSource::LocalFile);
            assert_eq!(fs::read(&local.file).unwrap(), bytes);
            task.await.unwrap();
        }
    });
}

#[test]
fn pairing_reports_missing_records() {
    run(async {
        let local = LocalFixture::new();
        let (provider, task) = mock_pair_record(UsbmuxdServerResponse::Result(2)).await;
        assert!(matches!(
            pairing_record(&provider, &local.root).await,
            Err(UsbProbeError::PairingRecordUnavailable)
        ));
        task.await.unwrap();
    });
}

#[test]
fn local_record_path_cannot_use_an_unsafe_udid() {
    let root = Path::new("app-data");
    let file = root.join(DEVICE_DATA_DIR).join(format!("{UDID}.plist"));
    assert_eq!(local_record_path(root, UDID), Some(file));
    for invalid in ["", ".", "..", "../ABCD", r"C:\ABCD", "ABCD:EF", "CON"] {
        assert!(local_record_path(root, invalid).is_none());
    }
}

#[tokio::test]
#[ignore = "requires a dedicated trusted USB iPhone with an empty pasteboard"]
async fn usb_push_round_trip() {
    let root =
        std::env::var_os("ICB_DATA_DIR").expect("set ICB_DATA_DIR to an isolated test directory");
    let addr = UsbmuxdAddr::default();
    let (mut tunnel, _) = timeout(
        PROBE_TIMEOUT,
        open_single_usb_tunnel(&addr, Path::new(&root)),
    )
    .with_subscriber(Dispatch::none())
    .await
    .expect("USB tunnel timed out")
    .expect("USB tunnel unavailable");
    let checked = crate::pasteboard::verify_push_round_trip(
        &mut tunnel.adapter,
        &tunnel.rsd,
        Path::new(&root),
    )
    .await;
    let _ = tunnel.adapter.close().await;
    checked.expect("USB clipboard round trip");
}

struct LocalFixture {
    root: PathBuf,
    file: PathBuf,
}

impl LocalFixture {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let root = loop {
            let number = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir().join(format!(
                "copiago-device-test-{}-{number}",
                std::process::id()
            ));
            match fs::create_dir(&root) {
                Ok(()) => break root,
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create test directory: {error}"),
            }
        };
        let devices = root.join(DEVICE_DATA_DIR);
        fs::create_dir(&devices).unwrap();
        Self {
            root,
            file: devices.join(format!("{UDID}.plist")),
        }
    }
}

impl Drop for LocalFixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.file);
        let _ = fs::remove_dir(self.root.join(DEVICE_DATA_DIR));
        let _ = fs::remove_dir(&self.root);
    }
}
