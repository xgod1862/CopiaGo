use std::mem::{MaybeUninit, size_of};

use idevice::xpc::{XPCMessage, XPCObject};
use windows_sys::Win32::System::{
    ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS_EX2},
    Threading::GetCurrentProcess,
};

use super::*;

const CRYPTEX_PORT: u16 = 124;
const IMAGE_BYTES: u64 = 17_239_040;
const DDI_NAME: &str = "com.apple.MobileAsset.DDI";
const STALE_VERSION: &str = "stale-simulation";

#[tokio::test]
async fn replaces_only_unmounted_ddi_and_uploads_its_five_cryptex_payloads() {
    let root = TempRoot::new();
    write_cryptex(&root.0);
    let mut rsd = handshake("27.2.1");
    rsd.services.insert(
        mount::CRYPTEX_SERVICE.to_owned(),
        super::service(CRYPTEX_PORT),
    );

    let (mut provider, check) = mounter(mounter_steps(true));
    let mut scripts = vec![check];
    add_xpc(
        &mut provider,
        &mut scripts,
        "copy-installed",
        installed(STALE_VERSION),
    );
    let (mut recheck, script) = mounter(mounter_steps(false));
    provider.streams.append(&mut recheck.streams);
    scripts.push(script);
    add_xpc(
        &mut provider,
        &mut scripts,
        "uninstall",
        idevice::xpc!({ "error": 0i64 }),
    );
    add_xpc(
        &mut provider,
        &mut scripts,
        "read-personalization-id",
        idevice::xpc!({
            "argv": { "img4_chip_chip": 1u64, "img4_chip_ecid": 2u64, "img4_chip_cpro": true }
        }),
    );
    let mut nonce = vec![0, 0, 0x12, 0x34];
    nonce.extend(2u32.to_le_bytes());
    add_xpc(
        &mut provider,
        &mut scripts,
        "get-nonce",
        idevice::xpc!({ "argv": { "nonce": XPCObject::Data(nonce) } }),
    );
    add_xpc(
        &mut provider,
        &mut scripts,
        "install",
        idevice::xpc!({ "error": 0u64 }),
    );
    add_xpc(
        &mut provider,
        &mut scripts,
        "copy-installed",
        installed("new-simulation"),
    );

    let started = Instant::now();
    within(Duration::from_secs(5), async {
        let mut log = SetupLog::new(&root.0);
        let mut image = mount::prepare_image(
            &mut provider,
            &mut rsd,
            ImageKind::Cryptex,
            &root.0,
            &mut log,
        )
        .await
        .expect("cryptex preparation and replacement")
        .expect("a stale cryptex is not mounted");
        let request = image
            .signature_request(&mut provider, &mut rsd, &mut log)
            .await
            .expect("read chip and nonce");
        assert_eq!(request["@Cryptex1,Ticket"].as_boolean(), Some(true));
        assert_eq!(
            request["Cryptex1,Nonce"].as_data(),
            Some([0x12, 0x34].as_slice())
        );
        assert_eq!(request["Cryptex1,UDID"].as_data().unwrap().len(), 16);
        assert!(
            request["Cryptex1,GenericDmg"]
                .as_dictionary()
                .unwrap()
                .contains_key("Digest")
        );
        image
            .install(&mut provider, &mut rsd, b"fake ticket".to_vec(), &mut log)
            .await
            .expect("cryptex upload and install verification");
        Ok(())
    })
    .await
    .unwrap();
    assert!(provider.streams.is_empty());
    for script in scripts {
        script.await.unwrap();
    }
    println!(
        "cryptex_image_bytes={IMAGE_BYTES} simulation_ms={:.2}",
        started.elapsed().as_secs_f64() * 1000.0
    );
    if let Some(counters) = process_memory() {
        println!(
            "peak_working_set={} peak_private_commit={}",
            counters.PeakWorkingSetSize, counters.PeakPagefileUsage,
        );
    }
}

fn write_cryptex(folder: &Path) {
    fs::File::create(folder.join("Image.dmg"))
        .unwrap()
        .set_len(IMAGE_BYTES)
        .unwrap();
    fs::write(folder.join("Image.dmg.trustcache"), b"cache").unwrap();
    fs::write(folder.join("Image.dmg.cryptex_info"), b"info").unwrap();
    fs::write(folder.join("Image.dmg.root_hash"), b"hash").unwrap();
    let component = fields([
        (
            "Info",
            Value::Dictionary(fields([("Personalize", true.into())])),
        ),
        ("Digest", Value::Data(vec![1, 2])),
    ]);
    let bundle = fields([("Cryptex1,GenericDmg", Value::Dictionary(component))]);
    let variant = fields([("Variant", "Developer Disk Image Cryptex".into())]);
    let identity = fields([
        ("Info", Value::Dictionary(variant)),
        ("Cryptex1,ChipID", "0x01".into()),
        ("Cryptex1,ProductClass", "0x02".into()),
        ("Cryptex1,Type", "DDI".into()),
        ("Cryptex1,SubType", 0u64.into()),
        ("Cryptex1,UseProductClass", true.into()),
        ("Cryptex1,NonceDomain", 4u64.into()),
        ("Cryptex1,Version", "test".into()),
        ("Cryptex1,PreauthorizationVersion", 1u64.into()),
        ("Manifest", Value::Dictionary(bundle)),
    ]);
    Value::Dictionary(fields([(
        "BuildIdentities",
        Value::Array(vec![Value::Dictionary(identity)]),
    )]))
    .to_file_xml(folder.join("BuildManifest.plist"))
    .unwrap();
}

fn installed(version: &str) -> XPCObject {
    idevice::xpc!({
        "argv": {
            "remote-cryptex-array": [
                { "remote-cryptex-identifier": "synthetic.unrelated", "remote-cryptex-version": "other" },
                { "remote-cryptex-identifier": DDI_NAME, "remote-cryptex-version": version }
            ]
        }
    })
}

fn add_xpc(
    provider: &mut ScriptedRsd,
    scripts: &mut Vec<task::JoinHandle<()>>,
    routine: &'static str,
    response: XPCObject,
) {
    let (client, device) = duplex(64 * 1024);
    provider.streams.push_back((CRYPTEX_PORT, client));
    scripts.push(task::spawn(run_xpc(device, routine, response)));
}

async fn run_xpc(mut device: DuplexStream, routine: &str, response: XPCObject) {
    let mut magic = [0; 24];
    device.read_exact(&mut magic).await.unwrap();
    assert_eq!(&magic, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");
    let mut payload = vec![0u8; 64 * 1024];
    let message_id = loop {
        let (kind, _, stream, size) = read_frame(&mut device, &mut payload).await;
        if kind == 0 && stream == 1 {
            let wrapper = XPCMessage::decode(&payload[..size]).unwrap();
            if let Some(request) = wrapper.message.as_ref().and_then(XPCObject::as_dictionary)
                && let Some(name) = request.get("routine")
            {
                assert_eq!(name, &XPCObject::String(routine.to_owned()));
                let args = request["argv"].as_dictionary().unwrap();
                match routine {
                    "uninstall" => {
                        assert_eq!(
                            args["remote-cryptex-identifier"],
                            XPCObject::String(DDI_NAME.to_owned())
                        );
                        assert_eq!(
                            args["remote-cryptex-version"],
                            XPCObject::String(STALE_VERSION.to_owned())
                        );
                    }
                    "get-nonce" => {
                        assert_eq!(args["nonce-domain-handle"], XPCObject::UInt64(4));
                    }
                    "install" => {
                        assert_eq!(args["persistence"], XPCObject::UInt64(2));
                        assert_eq!(args["nonce-persistence"], XPCObject::UInt64(1));
                        receive_upload(&mut device, &mut payload).await;
                    }
                    _ => {}
                }
                break wrapper.message_id.unwrap();
            }
        }
    };
    let response = XPCMessage::new(None, Some(response), Some(message_id))
        .encode(message_id)
        .unwrap();
    send_frame(&mut device, 0, 3, &response).await;
    tokio::io::copy(&mut device, &mut tokio::io::sink())
        .await
        .unwrap();
}

async fn receive_upload(device: &mut DuplexStream, buffer: &mut [u8]) {
    let window = (32u32 * 1024 * 1024).to_be_bytes();
    let settings = [0, 4, window[0], window[1], window[2], window[3]];
    send_frame(device, 4, 0, &settings).await;
    send_frame(device, 8, 0, &window).await;
    let mut collected = [0usize; 5];
    let mut finished = 0;
    while finished != collected.len() {
        let (kind, flags, stream, size) = read_frame(device, buffer).await;
        if kind == 0 && stream >= 5 {
            if stream == 5
                && collected[0] == 24
                && let Some(memory) = process_memory()
            {
                println!(
                    "private_working_set_in_transfer={}",
                    memory.PrivateWorkingSetSize
                );
            }
            collected[((stream - 5) / 2) as usize] += size;
            if flags & 1 != 0 {
                finished += 1;
            }
        }
    }
    // Each transfer opens with a 24-byte XPC wrapper before the raw payload.
    assert_eq!(collected, [IMAGE_BYTES as usize + 24, 29, 35, 28, 28]);
}

async fn read_frame(device: &mut DuplexStream, buf: &mut [u8]) -> (u8, u8, u32, usize) {
    let mut header = [0; 9];
    device.read_exact(&mut header).await.unwrap();
    let size = u32::from_be_bytes([0, header[0], header[1], header[2]]) as usize;
    device.read_exact(&mut buf[..size]).await.unwrap();
    let stream = u32::from_be_bytes(header[5..9].try_into().unwrap());
    (header[3], header[4], stream, size)
}

async fn send_frame(device: &mut DuplexStream, kind: u8, stream: u32, payload: &[u8]) {
    let length = (payload.len() as u32).to_be_bytes();
    device.write_all(&length[1..]).await.unwrap();
    device.write_all(&[kind, 0]).await.unwrap();
    device.write_all(&stream.to_be_bytes()).await.unwrap();
    device.write_all(payload).await.unwrap();
}

fn process_memory() -> Option<PROCESS_MEMORY_COUNTERS_EX2> {
    let mut counters = MaybeUninit::<PROCESS_MEMORY_COUNTERS_EX2>::uninit();
    // SAFETY: The process pseudo-handle is valid and the memory information output is writable.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            counters.as_mut_ptr().cast(),
            size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32,
        )
    };
    // SAFETY: A successful call initialized the entire structure.
    (ok != 0).then(|| unsafe { counters.assume_init() })
}
