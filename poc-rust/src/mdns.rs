//! Bonjour browse for `_remotepairing._tcp` (the iPhone's Wi-Fi RemotePairing endpoint).

use std::net::IpAddr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent};

pub struct Candidate {
    pub name: String,
    pub addrs: Vec<IpAddr>,
    pub port: u16,
    pub txt: Vec<(String, String)>,
}

pub async fn browse_remotepairing(timeout: Duration) -> Result<Vec<Candidate>, Box<dyn std::error::Error>> {
    let daemon = ServiceDaemon::new()?;
    let rx = daemon.browse("_remotepairing._tcp.local.")?;
    let end = Instant::now() + timeout;
    let mut out: Vec<Candidate> = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match tokio::time::timeout(left, rx.recv_async()).await {
            Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                let mut addrs: Vec<IpAddr> = info.get_addresses().iter().copied().collect();
                // IPv4 first: link-local IPv6 needs a scope id the daemon may not report.
                addrs.sort_by_key(|a| a.is_ipv6());
                let txt = info.get_properties().iter().map(|p| (p.key().to_string(), p.val_str().to_string())).collect();
                // Collect every answer in the window: the first device to answer is not
                // necessarily the paired iPhone, and the caller tries each candidate in turn.
                if !out.iter().any(|c| c.name == info.get_fullname()) {
                    out.push(Candidate { name: info.get_fullname().to_string(), addrs, port: info.get_port(), txt });
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(_)) | Err(_) => break,
        }
    }
    let _ = daemon.shutdown();
    Ok(out)
}
