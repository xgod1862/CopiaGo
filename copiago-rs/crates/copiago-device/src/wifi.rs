//! Wi-Fi RemotePairing and the userspace CoreDevice tunnel.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::time::Duration;

use copiago_core::diagnostic;
use idevice::{
    remote_pairing::{
        RemotePairingClient, RpPairingFile, RpPairingSocket, connect_tls_psk_tunnel_native,
    },
    rsd::RsdHandshake,
    tcp::{adapter::Adapter, handle::AdapterHandle},
};
use tokio::net::TcpStream;
use tokio::time::{Instant, timeout_at};
use tracing::{Dispatch, instrument::WithSubscriber};

use crate::{
    discovery,
    pasteboard::{PASTEBOARD_SERVICE, PasteboardError, read_rsd_types},
};

mod record;
mod transport;

pub(crate) use record::{
    PairedDevice, choose_paired_device, find_paired_device, forget_remote_records, host_identifier,
    paired_devices, save_remote_record, windows_hostname,
};
/// Whether there are saved records to show in the desktop connection settings.
pub fn has_pairing_records(data_dir: &Path) -> bool {
    matches!(
        find_paired_device(data_dir),
        Ok(_) | Err(WifiProbeError::MultiplePairingRecords)
    )
}

const ENDPOINT_TIMEOUT: Duration = Duration::from_secs(4);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const IPV6_TCP_HEADERS: usize = 60;

/// Options for probing one already-paired iPhone.
#[derive(Clone, Copy, Debug, Default)]
pub struct WifiProbeOptions {
    /// Use this RemotePairing address directly. Otherwise, use Windows mDNS.
    pub address: Option<SocketAddr>,
    /// Persist a missing identifier after a successful verification and RSD handshake.
    pub write_identifier: bool,
    /// mDNS discovery budget. None uses the five-second default.
    pub discovery_timeout: Option<Duration>,
}

/// Result of adding the verified identifier to the pairing record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentifierMigration {
    AlreadyPresent,
    ReadOnly,
    Written,
    WriteFailed,
}

/// Aggregate service information from a verified Wi-Fi tunnel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WifiProbe {
    pub service_count: usize,
    pub pasteboard_advertised: bool,
    pub identifier_migration: IdentifierMigration,
}

/// Wi-Fi probe failures with device and connection details omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WifiProbeError {
    NoPairingRecord,
    MultiplePairingRecords,
    ReadPairingRecord,
    InvalidPairingRecord,
    HostName,
    Discovery,
    NoEndpoint,
    /// The phone was advertised, but no TCP connection to it ever completed: packets
    /// were dropped or the host was unreachable. Typical of Wi-Fi client (AP) isolation,
    /// where multicast discovery still works but unicast between clients is blocked.
    Unreachable,
    /// The phone answered but refused the TCP connection.
    Connect,
    PairVerify,
    CreateListener,
    Tunnel,
    Rsd,
    Pasteboard(PasteboardError),
    /// TCP to the phone succeeded, but pair-verify, the tunnel or RSD did not finish
    /// in time. Silence before TCP completes is [`Self::Unreachable`] instead.
    Timeout,
    CloseTimeout,
}

impl WifiProbeError {
    /// How far one endpoint attempt got. With several endpoints (IPv4 and IPv6), the
    /// furthest failure is reported, so a blocked IPv6 link-local address cannot hide
    /// an IPv4 handshake problem and vice versa.
    fn connect_progress(self) -> u8 {
        match self {
            Self::NoEndpoint => 0,
            Self::Unreachable => 1,
            Self::Connect => 2,
            Self::Timeout => 3,
            _ => 4,
        }
    }
}

impl fmt::Display for WifiProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::NoPairingRecord => "no RemotePairing record found",
            Self::MultiplePairingRecords => "expected one RemotePairing record",
            Self::ReadPairingRecord => "could not read or decrypt the RemotePairing record",
            Self::InvalidPairingRecord => "invalid RemotePairing record",
            Self::HostName => "could not determine the host name",
            Self::Discovery => "could not start DNS-SD discovery",
            Self::NoEndpoint => "no matching RemotePairing endpoint found",
            Self::Unreachable => "RemotePairing endpoint was advertised but did not answer",
            Self::Connect => "RemotePairing endpoint refused the connection",
            Self::PairVerify => "RemotePairing pair-verify failed",
            Self::CreateListener => "could not create the CoreDevice tunnel listener",
            Self::Tunnel => "could not open the TLS-PSK tunnel",
            Self::Rsd => "could not discover tunnel services through RSD",
            Self::Pasteboard(error) => return error.fmt(formatter),
            Self::Timeout => "Wi-Fi connection timed out",
            Self::CloseTimeout => "Wi-Fi tunnel did not close in time",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for WifiProbeError {}

/// Reads the advertised services through Wi-Fi and closes the tunnel.
/// Pass the CopiaGo data root and run on a Tokio runtime with I/O and time enabled.
pub async fn probe_wifi(
    data_dir: &Path,
    options: WifiProbeOptions,
) -> Result<WifiProbe, WifiProbeError> {
    let device = find_paired_device(data_dir)?;
    let (tunnel, identifier_migration) = open_wifi_tunnel(&device, options).await?;
    let summary = WifiProbe {
        service_count: tunnel.rsd.services.len(),
        pasteboard_advertised: tunnel.rsd.services.contains_key(PASTEBOARD_SERVICE),
        identifier_migration,
    };
    tunnel.close().await?;
    Ok(summary)
}

/// Reads the ordered pasteboard UTIs per item over an authenticated Wi-Fi tunnel.
pub async fn probe_wifi_pasteboard_types(
    data_dir: &Path,
    address: Option<SocketAddr>,
) -> Result<Vec<Vec<String>>, WifiProbeError> {
    let device = find_paired_device(data_dir)?;
    let options = WifiProbeOptions {
        address,
        write_identifier: false,
        ..WifiProbeOptions::default()
    };
    let (mut tunnel, _) = open_wifi_tunnel(&device, options).await?;
    let items = read_rsd_types(&mut tunnel.adapter, &tunnel.rsd, data_dir).await;
    let closed = tunnel.close().await;
    let items = items.map_err(WifiProbeError::Pasteboard)?;
    closed?;
    Ok(items)
}

/// Wait until the paired phone advertises itself on the local network.
/// Never resolves when the record has no IRK or browsing is unavailable.
pub(crate) async fn wait_for_advertisement(device: &PairedDevice) {
    let irk = record::RemoteRecord::read(&device.path)
        .ok()
        .and_then(|record| record.peer_alt_irk().copied());
    match irk {
        Some(irk) => {
            discovery::watch(&irk)
                .with_subscriber(Dispatch::none())
                .await
        }
        None => std::future::pending().await,
    }
}

pub(crate) async fn open_wifi_tunnel(
    device: &PairedDevice,
    options: WifiProbeOptions,
) -> Result<(WifiTunnel, IdentifierMigration), WifiProbeError> {
    // Upstream debug tracing can include RemotePairing and RSD payloads.
    open(device, options)
        .with_subscriber(Dispatch::none())
        .await
}

async fn open(
    device: &PairedDevice,
    options: WifiProbeOptions,
) -> Result<(WifiTunnel, IdentifierMigration), WifiProbeError> {
    let mut record = record::RemoteRecord::read(&device.path)?;
    let endpoints = match options.address {
        Some(address) => vec![address],
        None => {
            let irk = record.peer_alt_irk().ok_or(WifiProbeError::NoEndpoint)?;
            let budget = options
                .discovery_timeout
                .unwrap_or(discovery::DISCOVERY_TIMEOUT);
            let candidates = discovery::discover_within(Some(irk), budget)
                .await
                .map_err(|_| WifiProbeError::Discovery)?;
            let matched = discovery::matched_addresses(&candidates);
            if matched.is_empty() {
                // Tells "phone not advertising on this network" (0) apart from
                // "advertisements seen, none from the paired phone" (>0).
                diagnostic::event(format_args!(
                    "event=wifi_no_endpoint advertisements={}",
                    candidates.len()
                ));
            }
            matched
        }
    };

    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut last_error = WifiProbeError::NoEndpoint;
    for endpoint in endpoints {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let mut reached = false;
        let error = match timeout_at(
            deadline.min(now + ENDPOINT_TIMEOUT),
            open_at(endpoint, &mut record.pairing, &mut reached),
        )
        .await
        {
            Ok(Ok(tunnel)) => {
                // Logged, not enforced: see `PairedDevice::rsd_identity_fields`.
                diagnostic::event(format_args!(
                    "event=wifi_tunnel_opened services={} pasteboard={} {}",
                    tunnel.rsd.services.len(),
                    tunnel.rsd.services.contains_key(PASTEBOARD_SERVICE),
                    device.rsd_identity_fields(&tunnel.rsd)
                ));
                let migration = record.persist_identifier(options.write_identifier);
                return Ok((tunnel, migration));
            }
            Ok(Err(error)) => error,
            Err(_) if reached => WifiProbeError::Timeout,
            Err(_) => WifiProbeError::Unreachable,
        };
        diagnostic::event(format_args!(
            "event=wifi_endpoint_failed family={} error={error:?}",
            if endpoint.is_ipv4() { "v4" } else { "v6" }
        ));
        if error.connect_progress() >= last_error.connect_progress() {
            last_error = error;
        }
    }
    Err(last_error)
}

/// Only an explicit refusal proves the phone answered. Dropped packets surface as a
/// timeout and isolation or routing filters as unreachable host or network errors.
fn connect_failure(kind: std::io::ErrorKind) -> WifiProbeError {
    use std::io::ErrorKind;
    match kind {
        ErrorKind::ConnectionRefused => WifiProbeError::Connect,
        ErrorKind::TimedOut | ErrorKind::HostUnreachable | ErrorKind::NetworkUnreachable => {
            WifiProbeError::Unreachable
        }
        _ => WifiProbeError::Connect,
    }
}

/// Records which RSD step failed; both map to [`WifiProbeError::Rsd`].
fn rsd_failed(step: &str) -> WifiProbeError {
    diagnostic::event(format_args!("event=wifi_rsd_failed step={step}"));
    WifiProbeError::Rsd
}

/// Sets `reached` once TCP to `endpoint` completes, so a caller that times this out can
/// tell an unreachable phone from a stalled handshake.
async fn open_at(
    endpoint: SocketAddr,
    pairing: &mut RpPairingFile,
    reached: &mut bool,
) -> Result<WifiTunnel, WifiProbeError> {
    let mut control = TcpStream::connect(endpoint)
        .await
        .map_err(|error| connect_failure(error.kind()))?;
    *reached = true;
    let tunnel = {
        let mut client =
            RemotePairingClient::new(RpPairingSocket::new(&mut control), pairing.identifier());
        client
            .attempt_pair_verify()
            .await
            .map_err(|_| WifiProbeError::PairVerify)?;
        client
            .validate_pairing(pairing)
            .await
            .map_err(|_| WifiProbeError::PairVerify)?;
        let port = client
            .create_tcp_listener()
            .await
            .map_err(|_| WifiProbeError::CreateListener)?;
        let mut listener = endpoint;
        listener.set_port(port);
        let data = TcpStream::connect(listener)
            .await
            .map_err(|_| WifiProbeError::Tunnel)?;
        connect_tls_psk_tunnel_native(data, client.encryption_key())
            .await
            .map_err(|_| WifiProbeError::Tunnel)?
    };

    let info = &tunnel.info;
    let host_ip = info
        .client_address
        .parse::<IpAddr>()
        .map_err(|_| WifiProbeError::Tunnel)?;
    let peer_ip = info
        .server_address
        .parse::<IpAddr>()
        .map_err(|_| WifiProbeError::Tunnel)?;
    let rsd_port = info.server_rsd_port;
    let mtu = usize::from(info.mtu);
    if rsd_port == 0 || mtu <= IPV6_TCP_HEADERS || host_ip.is_ipv4() != peer_ip.is_ipv4() {
        return Err(WifiProbeError::Tunnel);
    }

    let (stream, lifetime) = transport::Transport::new(tunnel.into_inner(), control);
    let mut adapter = Adapter::new(Box::new(stream), host_ip, peer_ip);
    adapter.set_mss(mtu - IPV6_TCP_HEADERS);
    let mut adapter = adapter.to_async_handle();
    let stream = adapter
        .connect(rsd_port)
        .await
        .map_err(|_| rsd_failed("connect"))?;
    let rsd = RsdHandshake::new(stream)
        .await
        .map_err(|_| rsd_failed("handshake"))?;
    Ok(WifiTunnel {
        rsd,
        adapter,
        lifetime,
    })
}

pub(crate) struct WifiTunnel {
    pub(crate) rsd: RsdHandshake,
    pub(crate) adapter: AdapterHandle,
    lifetime: transport::Lifetime,
}

impl WifiTunnel {
    pub(crate) async fn close(mut self) -> Result<(), WifiProbeError> {
        let _ = self.adapter.close().await;
        self.lifetime.close().await
    }
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::*;

    #[test]
    fn only_a_refusal_counts_as_an_answer() {
        assert_eq!(
            connect_failure(ErrorKind::ConnectionRefused),
            WifiProbeError::Connect
        );
        for kind in [
            ErrorKind::TimedOut,
            ErrorKind::HostUnreachable,
            ErrorKind::NetworkUnreachable,
        ] {
            assert_eq!(
                connect_failure(kind),
                WifiProbeError::Unreachable,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn the_furthest_endpoint_failure_wins() {
        let order = [
            WifiProbeError::NoEndpoint,
            WifiProbeError::Unreachable,
            WifiProbeError::Connect,
            WifiProbeError::Timeout,
            WifiProbeError::PairVerify,
        ];
        for pair in order.windows(2) {
            assert!(pair[0].connect_progress() < pair[1].connect_progress());
        }
    }
}
