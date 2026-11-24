//! Wi-Fi RemotePairing and the userspace CoreDevice tunnel.

use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::path::Path;
use std::time::Duration;

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

use crate::{discovery, pasteboard::PASTEBOARD_SERVICE};

mod record;
mod transport;

pub(crate) use record::{PairedDevice, find_paired_device};
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
    Connect,
    PairVerify,
    CreateListener,
    Tunnel,
    Rsd,
    Timeout,
    CloseTimeout,
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
            Self::Connect => "could not connect to the RemotePairing endpoint",
            Self::PairVerify => "RemotePairing pair-verify failed",
            Self::CreateListener => "could not create the CoreDevice tunnel listener",
            Self::Tunnel => "could not open the TLS-PSK tunnel",
            Self::Rsd => "could not discover tunnel services through RSD",
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
            let candidates = discovery::discover(Some(irk))
                .await
                .map_err(|_| WifiProbeError::Discovery)?;
            discovery::matched_addresses(&candidates)
        }
    };

    let deadline = Instant::now() + CONNECT_TIMEOUT;
    let mut last_error = WifiProbeError::NoEndpoint;
    for endpoint in endpoints {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        match timeout_at(
            deadline.min(now + ENDPOINT_TIMEOUT),
            open_at(endpoint, &mut record.pairing),
        )
        .await
        {
            Ok(Ok(tunnel)) if device.matches_rsd(&tunnel.rsd) => {
                let migration = record.persist_identifier(options.write_identifier);
                return Ok((tunnel, migration));
            }
            Ok(Ok(_)) => last_error = WifiProbeError::Rsd,
            Ok(Err(error)) => last_error = error,
            Err(_) => last_error = WifiProbeError::Timeout,
        }
    }
    Err(last_error)
}

async fn open_at(
    endpoint: SocketAddr,
    pairing: &mut RpPairingFile,
) -> Result<WifiTunnel, WifiProbeError> {
    let mut control = TcpStream::connect(endpoint)
        .await
        .map_err(|_| WifiProbeError::Connect)?;
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
        .map_err(|_| WifiProbeError::Rsd)?;
    let rsd = RsdHandshake::new(stream)
        .await
        .map_err(|_| WifiProbeError::Rsd)?;
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
