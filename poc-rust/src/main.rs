//! CopiaGo Rust feasibility prototype.
//!
//! Commands:
//!   info                       list usbmux devices and read lockdown values
//!   run [--wifi] [--set TEXT] [--idle SECS]
//!                              open a CoreDevice tunnel (USB via CoreDeviceProxy, or Wi-Fi via
//!                              RemotePairing + TLS-PSK), RSD handshake, read/write the pasteboard,
//!                              then watch pasteboard notifications while sampling memory.
//!
//! Pairing records are the ones the Python build already wrote (DPAPI sealed), found under
//! `--data DIR` (default: the installed app's data folder for this account).

mod dpapi;
mod mem;
#[cfg(feature = "wifi")]
mod mdns;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use idevice::{
    IdeviceService, RsdService,
    core_device::{DataInclusionPolicy, GENERAL_PASTEBOARD, PasteboardPayload, PasteboardServiceClient},
    core_device_proxy::CoreDeviceProxy,
    lockdown::LockdownClient,
    notification_proxy::NotificationProxyClient,
    pairing_file::PairingFile,
    provider::{IdeviceProvider, UsbmuxdProvider},
    rsd::RsdHandshake,
    tcp::handle::AdapterHandle,
    usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdConnection},
};

type Res<T> = Result<T, Box<dyn std::error::Error>>;

const PASTEBOARD_SERVICE: &str = "com.apple.coredevice.pasteboardservice";
const PASTEBOARD_CHANGED: &str = "com.apple.pasteboard.notify.changed";

fn main() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    if let Err(e) = rt.block_on(real_main()) {
        eprintln!("FAILED: {e}");
        std::process::exit(1);
    }
}

struct Args {
    cmd: String,
    wifi: bool,
    set: Option<String>,
    idle: u64,
    data: PathBuf,
}

fn parse_args() -> Args {
    let mut it = std::env::args().skip(1);
    let mut a = Args {
        cmd: it.next().unwrap_or_else(|| "info".into()),
        wifi: false,
        set: None,
        idle: 0,
        data: PathBuf::from(std::env::var("COPIAGO_DATA").unwrap_or_else(|_| {
            format!(r"E:\iPhoneClipboard\data\{}\device-data", std::env::var("USERNAME").unwrap_or_default())
        })),
    };
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--wifi" => a.wifi = true,
            "--set" => a.set = it.next(),
            "--idle" => a.idle = it.next().and_then(|s| s.parse().ok()).unwrap_or(0),
            "--data" => a.data = it.next().map(PathBuf::from).unwrap_or(a.data),
            other => eprintln!("ignored argument {other}"),
        }
    }
    a
}

async fn real_main() -> Res<()> {
    let args = parse_args();
    let t0 = Instant::now();
    match args.cmd.as_str() {
        "info" => info(&args).await,
        "run" => {
            let r = run(&args).await;
            eprintln!("total {:.2}s", t0.elapsed().as_secs_f64());
            r
        }
        other => Err(format!("unknown command {other}").into()),
    }
}

// ---------------------------------------------------------------- step 1: usbmux + lockdown

/// A usbmux provider that falls back to the lockdown pairing record the Python build saved
/// itself (Apple Mobile Device Service on Windows usually has none for pymobiledevice3 pairs).
#[derive(Debug)]
struct Provider {
    inner: UsbmuxdProvider,
    record: PathBuf,
}

impl IdeviceProvider for Provider {
    fn connect(
        &self,
        port: u16,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<idevice::Idevice, idevice::IdeviceError>> + Send>> {
        self.inner.connect(port)
    }
    fn label(&self) -> &str {
        self.inner.label()
    }
    fn get_pairing_file(
        &self,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<PairingFile, idevice::IdeviceError>> + Send>> {
        let fut = self.inner.get_pairing_file();
        let record = self.record.clone();
        Box::pin(async move {
            match fut.await {
                Ok(p) => {
                    eprintln!("  pairing record: from usbmuxd");
                    Ok(p)
                }
                Err(e) => {
                    eprintln!("  usbmuxd has no pairing record ({e}); using {}", record.display());
                    let bytes = dpapi::unseal(&std::fs::read(&record)?)
                        .map_err(|e| idevice::IdeviceError::InternalError(e.to_string()))?;
                    PairingFile::from_bytes(&bytes)
                }
            }
        })
    }
}

async fn usb_providers(data: &Path) -> Res<Vec<(String, Provider)>> {
    let mut mux = UsbmuxdConnection::default().await?;
    let devices = mux.get_devices().await?;
    eprintln!("usbmux: {} device(s)", devices.len());
    let addr = UsbmuxdAddr::default();
    let mut out = Vec::new();
    for d in devices {
        eprintln!("  {} id={} via {:?}", d.udid, d.device_id, d.connection_type);
        if matches!(d.connection_type, Connection::Usb) {
            let record = data.join(format!("{}.plist", d.udid));
            out.push((d.udid.clone(), Provider { inner: d.to_provider(addr.clone(), "copiago-poc"), record }));
        }
    }
    Ok(out)
}

async fn info(args: &Args) -> Res<()> {
    for (udid, provider) in usb_providers(&args.data).await? {
        let mut lockdown = LockdownClient::connect(&provider).await?;
        // Unpaired values first, then a TLS session for the full set.
        let pt = lockdown.get_value(Some("ProductType"), None).await?;
        eprintln!("[{udid}] ProductType (no session) = {pt:?}");
        let pairing = provider.get_pairing_file().await?;
        lockdown.start_session(&pairing).await?;
        for key in ["DeviceName", "ProductVersion", "BuildVersion", "HardwareModel"] {
            let v = lockdown.get_value(Some(key), None).await?;
            eprintln!("[{udid}] {key} = {}", v.as_string().unwrap_or("?"));
        }
        let wifi = lockdown.get_value(Some("EnableWifiConnections"), Some("com.apple.mobile.wireless_lockdown")).await;
        eprintln!("[{udid}] EnableWifiConnections = {wifi:?}");
    }
    Ok(())
}

// ---------------------------------------------------------------- step 3: tunnel + RSD

async fn open_usb_tunnel(args: &Args) -> Res<(AdapterHandle, u16)> {
    let (udid, provider) = usb_providers(&args.data).await?.into_iter().next().ok_or("no USB iPhone")?;
    let t = Instant::now();
    let proxy = CoreDeviceProxy::connect(&provider).await?;
    let info = proxy.tunnel_info().clone();
    eprintln!(
        "[{udid}] CoreDeviceProxy tunnel {} -> {} mtu={} rsd={} ({:.2}s)",
        info.client_address, info.server_address, info.mtu, info.server_rsd_port, t.elapsed().as_secs_f64()
    );
    let adapter = proxy.create_software_tunnel()?;
    Ok((adapter.to_async_handle(), info.server_rsd_port))
}

#[cfg(feature = "wifi")]
async fn open_wifi_tunnel(args: &Args) -> Res<(AdapterHandle, u16)> {
    use idevice::remote_pairing::{RemotePairingClient, RpPairingSocket, connect_tls_psk_tunnel_native};
    use tokio::net::TcpStream;

    // The Python build names records remote_<udid>.plist; use the first one.
    let record = std::fs::read_dir(&args.data)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("remote_")))
        .ok_or("no remote_*.plist pairing record")?;
    let udid = record.file_stem().unwrap().to_string_lossy().trim_start_matches("remote_").to_string();
    let mut rpf = load_rp_record(&record)?;
    eprintln!("RP record {} (host identifier {})", record.display(), rpf.identifier);

    let t = Instant::now();
    let candidates = mdns::browse_remotepairing(Duration::from_secs(2)).await?;
    eprintln!("mDNS _remotepairing._tcp: {} answer(s) in {:.2}s", candidates.len(), t.elapsed().as_secs_f64());
    for c in &candidates {
        eprintln!("  {} {:?}:{} txt={:?}", c.name, c.addrs, c.port, c.txt);
    }
    // TXT "identifier" is the device's RemotePairing identifier, not the UDID, so try every answer.
    let mut last_err: Box<dyn std::error::Error> = "no candidates".into();
    for c in candidates {
        for addr in &c.addrs {
            let sa = std::net::SocketAddr::new(*addr, c.port);
            let t = Instant::now();
            let attempt = async {
                let tcp = tokio::time::timeout(Duration::from_secs(4), TcpStream::connect(sa)).await??;
                let mut client = RemotePairingClient::new(RpPairingSocket::new(tcp), &rpf.identifier.clone());
                client.attempt_pair_verify().await?;
                client.validate_pairing(&mut rpf).await?;
                eprintln!("  pair-verify OK at {sa} ({:.2}s)", t.elapsed().as_secs_f64());
                let port = client.create_tcp_listener().await?;
                let key = client.encryption_key().to_vec();
                let tcp2 = TcpStream::connect(std::net::SocketAddr::new(*addr, port)).await?;
                let tunnel = connect_tls_psk_tunnel_native(tcp2, &key).await?;
                // Keep the control connection alive for the life of the tunnel.
                std::mem::forget(client);
                Ok::<_, Box<dyn std::error::Error>>(tunnel)
            };
            match attempt.await {
                Ok(tunnel) => {
                    let info = tunnel.info.clone();
                    eprintln!(
                        "[{udid}] Wi-Fi tunnel {} -> {} mtu={} rsd={} ({:.2}s)",
                        info.client_address, info.server_address, info.mtu, info.server_rsd_port, t.elapsed().as_secs_f64()
                    );
                    let our: std::net::IpAddr = info.client_address.parse()?;
                    let their: std::net::IpAddr = info.server_address.parse()?;
                    let stream: Box<dyn idevice::ReadWrite> = Box::new(tunnel.into_inner());
                    let mut adapter = idevice::tcp::adapter::Adapter::new(Box::new(stream), our, their);
                    adapter.set_mss((info.mtu as usize).saturating_sub(60));
                    return Ok((adapter.to_async_handle(), info.server_rsd_port));
                }
                Err(e) => {
                    eprintln!("  {sa}: {e}");
                    last_err = e;
                }
            }
        }
    }
    Err(last_err)
}

/// The Python record has public_key/private_key (and maybe host_identifier) but no `identifier`;
/// pymobiledevice3 derived it as uppercase uuid3(DNS, hostname).
#[cfg(feature = "wifi")]
fn load_rp_record(path: &Path) -> Res<idevice::remote_pairing::RpPairingFile> {
    let raw = dpapi::unseal(&std::fs::read(path)?)?;
    let mut dict: plist::Dictionary = plist::from_bytes(&raw)?;
    eprintln!("  record keys: {:?}", dict.keys().collect::<Vec<_>>());
    if !dict.contains_key("identifier") {
        let id = match dict.get("host_identifier").and_then(|v| v.as_string()) {
            Some(h) if !h.is_empty() => h.to_string(),
            _ => {
                let host = std::env::var("COMPUTERNAME").unwrap_or_default();
                // platform.node() on Windows returns the NetBIOS name as typed by the user.
                let host = hostname().unwrap_or(host);
                uuid::Uuid::new_v3(&uuid::Uuid::NAMESPACE_DNS, host.as_bytes()).to_string().to_uppercase()
            }
        };
        dict.insert("identifier".into(), plist::Value::String(id));
    }
    if let Some(irk) = dict.remove("peer_alt_irk").or_else(|| dict.remove("alt_irk")) {
        dict.insert("alt_irk".into(), irk);
    }
    let mut buf = Vec::new();
    plist::to_writer_xml(&mut buf, &plist::Value::Dictionary(dict))?;
    Ok(idevice::remote_pairing::RpPairingFile::from_bytes(&buf)?)
}

#[cfg(feature = "wifi")]
fn hostname() -> Option<String> {
    let out = std::process::Command::new("hostname").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string()).filter(|s| !s.is_empty())
}

// ---------------------------------------------------------------- step 4: pasteboard

async fn run(args: &Args) -> Res<()> {
    mem::report("start");
    let (mut adapter, rsd_port) = if args.wifi {
        #[cfg(feature = "wifi")]
        {
            open_wifi_tunnel(args).await?
        }
        #[cfg(not(feature = "wifi"))]
        return Err("built without wifi feature".into());
    } else {
        open_usb_tunnel(args).await?
    };
    let t = Instant::now();
    let stream = adapter.connect(rsd_port).await?;
    let mut handshake = RsdHandshake::new(stream).await?;
    eprintln!(
        "RSD handshake: {} services, device uuid {} ({:.2}s)",
        handshake.services.len(),
        handshake.uuid,
        t.elapsed().as_secs_f64()
    );
    let has_pb = handshake.services.contains_key(PASTEBOARD_SERVICE);
    eprintln!("  {PASTEBOARD_SERVICE}: {}", if has_pb { "PRESENT" } else { "MISSING (DDI not mounted?)" });
    for probe in ["com.apple.mobile.mobile_image_mounter.shim.remote", "com.apple.mobile.notification_proxy.shim.remote", "com.apple.mobile.lockdown.remote.trusted"] {
        eprintln!("  {probe}: {}", handshake.services.contains_key(probe));
    }
    if !has_pb {
        return Err("pasteboard service not advertised".into());
    }
    mem::report("tunnel+rsd");

    // Read
    let t = Instant::now();
    let mut pb = PasteboardServiceClient::connect_rsd(&mut adapter, &mut handshake).await?;
    let listing = pb.get_with_policy(GENERAL_PASTEBOARD, DataInclusionPolicy::AllPromised).await?;
    let types: Vec<_> = listing.items.iter().flat_map(|i| i.types.iter().cloned()).collect();
    eprintln!("PULL allPromised: changeCount={:?} types={types:?} ({:.2}s)", listing.change_count, t.elapsed().as_secs_f64());
    let t = Instant::now();
    let snap = pb.get_with_policy(GENERAL_PASTEBOARD, DataInclusionPolicy::PromiseSecondary).await?;
    eprintln!(
        "PULL promiseSecondary: text={:?} image={:?} ({:.2}s)",
        snap.text().map(|s| preview(&s)),
        snap.image().map(|(u, b)| (u, b.len())),
        t.elapsed().as_secs_f64()
    );

    // Write
    // Full pull of the text for restoring after the write tests (the primary type may be a URL).
    let snap = if args.set.is_some() {
        pb.get_with_policy(GENERAL_PASTEBOARD, DataInclusionPolicy::AllResolved).await?
    } else {
        snap
    };
    if let Some(text) = &args.set {
        let t = Instant::now();
        let mut pb2 = PasteboardServiceClient::connect_rsd(&mut adapter, &mut handshake).await?;
        pb2.set_text(text, GENERAL_PASTEBOARD).await?;
        eprintln!("SET text ({} bytes) ({:.2}s)", text.len(), t.elapsed().as_secs_f64());
        let back = pb2.get_with_policy(GENERAL_PASTEBOARD, DataInclusionPolicy::PromiseSecondary).await?;
        let ok = back.text().as_deref() == Some(text.as_str());
        eprintln!("read-back {}: {:?}", if ok { "MATCH" } else { "MISMATCH" }, back.text().map(|s| preview(&s)));
        // Put the user's own text back so the test leaves no trace on the phone.
        if let Some(original) = snap.text() {
            pb2.set_text(&original, GENERAL_PASTEBOARD).await?;
            eprintln!("restored original pasteboard text ({} bytes)", original.len());
        }
        // Fail only after restoring, so a regression run never reports a bad write as a pass.
        if !ok {
            return Err("pasteboard read-back did not match the text that was set".into());
        }
    }
    mem::report("after pasteboard");

    if args.idle == 0 {
        return Ok(());
    }

    // Idle like the real app: notification proxy for change events + a PULL on every change.
    let mut np = NotificationProxyClient::connect_rsd(&mut adapter, &mut handshake).await?;
    np.observe_notification(PASTEBOARD_CHANGED).await?;
    // Self-test: a SET from another connection must raise the Darwin notification.
    if let Some(original) = snap.text().filter(|_| args.set.is_some()) {
        let mut pb3 = PasteboardServiceClient::connect_rsd(&mut adapter, &mut handshake).await?;
        let t = Instant::now();
        pb3.set_text("CopiaGo notify self-test", GENERAL_PASTEBOARD).await?;
        let name = tokio::time::timeout(Duration::from_secs(5), np.receive_notification()).await;
        eprintln!("notify self-test: {name:?} after {:.2}s", t.elapsed().as_secs_f64());
        pb3.set_text(&original, GENERAL_PASTEBOARD).await?;
        // Swallow the notification for the restore.
        let _ = tokio::time::timeout(Duration::from_secs(2), np.receive_notification()).await;
    }
    let listing = pb.get_with_policy(GENERAL_PASTEBOARD, DataInclusionPolicy::AllPromised).await?;
    eprintln!("watching {PASTEBOARD_CHANGED} for {}s (copy something on the iPhone)", args.idle);
    let end = Instant::now() + Duration::from_secs(args.idle);
    let mut next_report = Instant::now() + Duration::from_secs(15);
    let mut last_cc = listing.change_count;
    while Instant::now() < end {
        let wait = next_report.min(end).saturating_duration_since(Instant::now());
        match tokio::time::timeout(wait, np.receive_notification()).await {
            Ok(Ok(name)) => {
                let s = pb.get_with_policy(GENERAL_PASTEBOARD, DataInclusionPolicy::PromiseSecondary).await?;
                if s.change_count != last_cc {
                    last_cc = s.change_count;
                    let promised = s.items.iter().flat_map(|i| &i.data).filter(|e| matches!(e.payload, PasteboardPayload::Promised { .. })).count();
                    eprintln!(
                        "  {name}: changeCount={:?} text={:?} image={:?} promised={promised}",
                        s.change_count,
                        s.text().map(|t| preview(&t)),
                        s.image().map(|(u, b)| (u, b.len()))
                    );
                }
            }
            Ok(Err(e)) => return Err(format!("notification stream: {e}").into()),
            Err(_) => {}
        }
        if Instant::now() >= next_report {
            mem::report(&format!("idle t+{:.0}s", args.idle as f64 - end.saturating_duration_since(Instant::now()).as_secs_f64()));
            next_report += Duration::from_secs(15);
        }
    }
    mem::report("idle end");
    let t = Instant::now();
    let s = pb.get_with_policy(GENERAL_PASTEBOARD, DataInclusionPolicy::AllPromised).await?;
    eprintln!("liveness PULL after idle: changeCount={:?} ({:.2}s)", s.change_count, t.elapsed().as_secs_f64());
    Ok(())
}

fn preview(s: &str) -> String {
    let p: String = s.chars().take(40).collect();
    if s.chars().count() > 40 { format!("{p}…") } else { p }
}
