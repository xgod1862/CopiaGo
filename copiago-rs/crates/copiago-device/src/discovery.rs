//! Windows discovery of local iPhone RemotePairing endpoints.

use std::io;
use std::net::{SocketAddr, SocketAddrV4, SocketAddrV6};
use std::time::Duration;

use idevice::remote_pairing::PeerDevice;
use tokio::time::{Instant, timeout_at};

mod windows;

pub const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Budget when the paired phone is also on USB, so a silent Wi-Fi side costs little.
pub const QUICK_DISCOVERY_TIMEOUT: Duration = Duration::from_millis(1500);

/// A RemotePairing advertisement and its match result.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    ipv4: Option<SocketAddrV4>,
    ipv6: Option<SocketAddrV6>,
    auth_tag_matches: bool,
}

impl Candidate {
    /// Addresses in connection order, with IPv4 first.
    pub fn addresses(&self) -> impl Iterator<Item = SocketAddr> {
        self.ipv4
            .map(SocketAddr::V4)
            .into_iter()
            .chain(self.ipv6.map(SocketAddr::V6))
    }

    /// Whether the advertised tag matches the supplied pairing record.
    pub fn auth_tag_matches(&self) -> bool {
        self.auth_tag_matches
    }
}

/// Browse and resolve `_remotepairing._tcp.local` over eligible Windows interfaces.
///
/// A matching IPv4 advertisement completes discovery early. Other candidates are
/// collected up to the five-second deadline and ordered by tag match, then IPv4.
/// Unmatched advertisements without a tag or saved IRK are available for
/// diagnostics. Automatic connections use only matched addresses and still
/// require pair-verify.
pub async fn discover(peer_alt_irk: Option<&[u8; 16]>) -> io::Result<Vec<Candidate>> {
    discover_within(peer_alt_irk, DISCOVERY_TIMEOUT).await
}

/// Like [`discover`], with a caller-chosen deadline.
pub async fn discover_within(
    peer_alt_irk: Option<&[u8; 16]>,
    budget: Duration,
) -> io::Result<Vec<Candidate>> {
    let deadline = Instant::now() + budget;
    let mut scan = windows::Scan::start()?;
    Ok(collect_candidates(deadline, peer_alt_irk, async || scan.next().await).await)
}

/// Resolve once Windows reports an advertisement that matches the pairing record.
/// Windows keeps browsing in the background and delivers the phone's own announcement
/// when it rejoins the network, so waiting here costs no polling. If browsing cannot
/// start this never resolves and callers fall back to their timer.
pub(crate) async fn watch(peer_alt_irk: &[u8; 16]) {
    if let Ok(mut scan) = windows::Scan::start() {
        while let Some(advertisement) = scan.next().await {
            if advertisement
                .into_candidate(Some(peer_alt_irk))
                .is_some_and(|candidate| candidate.auth_tag_matches)
            {
                return;
            }
        }
    }
    std::future::pending().await
}

pub(crate) fn matched_addresses(candidates: &[Candidate]) -> Vec<SocketAddr> {
    candidates
        .iter()
        .filter(|candidate| candidate.auth_tag_matches)
        .flat_map(Candidate::addresses)
        .collect()
}

struct Advertisement {
    ipv4: Option<SocketAddrV4>,
    ipv6: Option<SocketAddrV6>,
    identifier: Option<String>,
    auth_tag: Option<String>,
}

impl Advertisement {
    fn into_candidate(self, peer_alt_irk: Option<&[u8; 16]>) -> Option<Candidate> {
        if self.ipv4.is_none() && self.ipv6.is_none() {
            return None;
        }
        let auth_tag_matches = match (peer_alt_irk, self.auth_tag.as_deref()) {
            (Some(irk), Some(tag)) => {
                let identifier = self.identifier.as_deref().filter(|id| !id.is_empty())?;
                if !PeerDevice::validate_auth_tag(irk, identifier, tag) {
                    return None;
                }
                true
            }
            _ => false,
        };
        Some(Candidate {
            ipv4: self.ipv4,
            ipv6: self.ipv6,
            auth_tag_matches,
        })
    }
}

async fn collect_candidates(
    deadline: Instant,
    peer_alt_irk: Option<&[u8; 16]>,
    mut next: impl AsyncFnMut() -> Option<Advertisement>,
) -> Vec<Candidate> {
    let mut candidates = Vec::new();
    while Instant::now() < deadline {
        let Ok(Some(advertisement)) = timeout_at(deadline, next()).await else {
            break;
        };
        let Some(candidate) = advertisement.into_candidate(peer_alt_irk) else {
            continue;
        };
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
        if candidate.auth_tag_matches && candidate.ipv4.is_some() {
            break;
        }
    }
    candidates.sort_by_key(|candidate| (!candidate.auth_tag_matches, candidate.ipv4.is_none()));
    candidates
}

#[cfg(test)]
mod tests;
