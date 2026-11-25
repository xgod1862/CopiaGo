//! Network hints for a paired phone that does not reappear on Wi-Fi.

use std::ptr::null;

use windows_sys::Win32::{
    Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS},
    NetworkManagement::{
        IpHelper::{
            GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_MULTICAST,
            GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
        },
        Ndis::IfOperStatusUp,
    },
};

/// Built-in Windows tunnels that never intercept LAN traffic.
const SYSTEM_TUNNELS: [&str; 4] = ["teredo", "isatap", "6to4", "ip-https"];
/// Names used by VPN clients and proxy "TUN mode" adapters. Such adapters can capture
/// routes or multicast so the phone's advertisement or the tunnel never reaches it.
const VPN_HINTS: [&str; 13] = [
    "wintun",
    "wireguard",
    "tap-windows",
    "openvpn",
    "clash",
    "mihomo",
    "sing-box",
    "v2ray",
    "tailscale",
    "zerotier",
    "vpn",
    "tun",
    "tap",
];

/// Friendly name of the first active adapter that looks like a VPN or proxy tunnel.
pub(super) fn vpn_adapter() -> Option<String> {
    let mut bytes = 16 * 1024u32;
    for _ in 0..3 {
        // The adapter list contains 64-bit alignment unions.
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(size_of::<u64>())];
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: head is aligned and points to at least `bytes` writable bytes.
        let status = unsafe {
            GetAdaptersAddresses(
                0,
                GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_DNS_SERVER | GAA_FLAG_SKIP_MULTICAST,
                null(),
                head,
                &mut bytes,
            )
        };
        match status {
            ERROR_BUFFER_OVERFLOW => continue,
            // SAFETY: A successful call filled a linked list inside the live buffer.
            ERROR_SUCCESS => return unsafe { first_vpn(head) },
            _ => return None,
        }
    }
    None
}

/// # Safety
/// `adapter` must be null or the head of a list filled by `GetAdaptersAddresses` whose
/// buffer outlives this call.
unsafe fn first_vpn(mut adapter: *const IP_ADAPTER_ADDRESSES_LH) -> Option<String> {
    while !adapter.is_null() {
        // SAFETY: Guaranteed by the caller for every node of the list.
        let current = unsafe { &*adapter };
        if current.OperStatus == IfOperStatusUp {
            // SAFETY: Both strings belong to the same live list node.
            let (name, description) =
                unsafe { (wide(current.FriendlyName), wide(current.Description)) };
            if looks_like_vpn(&format!("{name} {description}")) {
                return Some(name);
            }
        }
        adapter = current.Next;
    }
    None
}

/// # Safety
/// `text` must be null or a NUL-terminated UTF-16 string.
unsafe fn wide(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut length = 0;
    // SAFETY: The string is NUL-terminated per the caller's guarantee.
    while unsafe { *text.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `length` units before the terminator are readable.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, length) })
}

fn looks_like_vpn(names: &str) -> bool {
    let names = names.to_lowercase();
    !SYSTEM_TUNNELS.iter().any(|system| names.contains(system))
        && VPN_HINTS.iter().any(|hint| names.contains(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_vpn_and_proxy_adapters_only() {
        assert!(looks_like_vpn("Clash Wintun Userspace Tunnel"));
        assert!(looks_like_vpn("以太网 2 TAP-Windows Adapter V9"));
        assert!(looks_like_vpn("Meta Tunnel"));
        assert!(!looks_like_vpn("Teredo Tunneling Pseudo-Interface"));
        assert!(!looks_like_vpn("WLAN Intel(R) Wi-Fi 6E AX211 160MHz"));
        assert!(!looks_like_vpn(
            "vEthernet (Default Switch) Hyper-V Virtual Ethernet Adapter"
        ));
    }
}
