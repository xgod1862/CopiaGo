//! Owns the native requests and copies callback data before handing it to Tokio.

use std::collections::{BTreeMap, HashSet};
use std::ffi::c_void;
use std::io;
use std::mem::size_of;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::ptr::{null, null_mut, without_provenance_mut};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::mpsc::{self, Receiver, Sender};
use windows_sys::Win32::Foundation::{
    DNS_REQUEST_PENDING, ERROR_BUFFER_OVERFLOW, ERROR_CANCELLED, ERROR_NO_DATA, ERROR_SUCCESS,
};
use windows_sys::Win32::NetworkManagement::Dns::{
    DNS_QUERY_REQUEST_VERSION1, DNS_RECORDW, DNS_SERVICE_BROWSE_REQUEST,
    DNS_SERVICE_BROWSE_REQUEST_0, DNS_SERVICE_CANCEL, DNS_SERVICE_INSTANCE,
    DNS_SERVICE_RESOLVE_REQUEST, DNS_TYPE_PTR, DnsFree, DnsFreeRecordList, DnsServiceBrowse,
    DnsServiceBrowseCancel, DnsServiceFreeInstance, DnsServiceResolve, DnsServiceResolveCancel,
};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER, GAA_FLAG_SKIP_FRIENDLY_NAME,
    GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses, IP_ADAPTER_ADDRESSES_LH,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};
use windows_sys::core::PCWSTR;

use super::Advertisement;

const SERVICE: &str = "_remotepairing._tcp.local";

#[derive(Clone, Copy)]
struct Interface {
    ipv4_index: u32,
    ipv6_index: u32,
}

pub(super) struct Scan {
    sender: Sender<Event>,
    receiver: Receiver<Event>,
    browsers: Vec<Query>,
    resolves: BTreeMap<usize, (Query, String)>,
    seen: HashSet<(u32, String)>,
}

enum Event {
    Browse(Interface, String),
    Resolve(usize, Option<Advertisement>),
}

impl Scan {
    pub(super) fn start() -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel(64);
        let mut browsers = Vec::new();
        let mut first_error = None;
        for interface in ipv4_interfaces()? {
            match Query::start(Kind::Browse, interface, SERVICE, &sender) {
                Ok(query) => browsers.push(query),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        if browsers.is_empty()
            && let Some(error) = first_error
        {
            return Err(error);
        }
        Ok(Self {
            sender,
            receiver,
            browsers,
            resolves: BTreeMap::new(),
            seen: HashSet::new(),
        })
    }

    pub(super) async fn next(&mut self) -> Option<Advertisement> {
        if self.browsers.is_empty() {
            return None;
        }
        while let Some(event) = self.receiver.recv().await {
            match event {
                Event::Browse(interface, name) => {
                    let key = name.trim_end_matches('.').to_ascii_lowercase();
                    if self.seen.insert((interface.ipv4_index, key.clone())) {
                        match Query::start(Kind::Resolve, interface, &name, &self.sender) {
                            Ok(query) => {
                                self.resolves.insert(query.id, (query, key));
                            }
                            Err(_) => {
                                self.seen.remove(&(interface.ipv4_index, key));
                            }
                        }
                    }
                }
                Event::Resolve(id, result) => {
                    if let Some((query, key)) = self.resolves.remove(&id)
                        && result.is_none()
                    {
                        self.seen.remove(&(query.interface.ipv4_index, key));
                    }
                    if result.is_some() {
                        return result;
                    }
                }
            }
        }
        None
    }
}

#[derive(Clone, Copy)]
enum Kind {
    Browse,
    Resolve,
}

struct Query {
    id: usize,
    kind: Kind,
    interface: Interface,
    cancel: Box<DNS_SERVICE_CANCEL>,
    name: Vec<u16>,
    pending: bool,
}

// SAFETY: The native handle allocation stays at a stable address and is used by
// this owner only. The DNS-SD APIs have no thread affinity. Callback state is
// accessed separately through the synchronized registry.
unsafe impl Send for Query {}

#[derive(Clone)]
struct Callback {
    interface: Interface,
    sender: Sender<Event>,
}

// The opaque context is a number, never a dereferenced pointer. A callback
// already in flight can still free its result after cancellation. IDs are not
// reused, so a late callback cannot be dispatched to the next scan.
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static CALLBACKS: Mutex<BTreeMap<usize, Callback>> = Mutex::new(BTreeMap::new());

impl Query {
    fn start(
        kind: Kind,
        interface: Interface,
        query_name: &str,
        sender: &Sender<Event>,
    ) -> io::Result<Self> {
        let id = NEXT_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .map_err(|_| io::Error::other("DNS-SD callback IDs exhausted"))?;
        let mut query = Self {
            id,
            kind,
            interface,
            cancel: Box::new(DNS_SERVICE_CANCEL {
                reserved: null_mut(),
            }),
            name: query_name.encode_utf16().chain([0]).collect(),
            pending: false,
        };
        CALLBACKS.lock().unwrap().insert(
            id,
            Callback {
                interface,
                sender: sender.clone(),
            },
        );
        let context = without_provenance_mut::<c_void>(id);
        // SAFETY: The request fields are valid for the call. The UTF-16 name and
        // boxed cancellation handle stay allocated for the operation. The
        // callback context is an opaque ID registered before making the call.
        let status = unsafe {
            match kind {
                Kind::Browse => DnsServiceBrowse(
                    &DNS_SERVICE_BROWSE_REQUEST {
                        Version: DNS_QUERY_REQUEST_VERSION1,
                        InterfaceIndex: interface.ipv4_index,
                        QueryName: query.name.as_ptr(),
                        Anonymous: DNS_SERVICE_BROWSE_REQUEST_0 {
                            pBrowseCallback: Some(browse_callback),
                        },
                        pQueryContext: context,
                    },
                    &mut *query.cancel,
                ),
                Kind::Resolve => DnsServiceResolve(
                    &DNS_SERVICE_RESOLVE_REQUEST {
                        Version: DNS_QUERY_REQUEST_VERSION1,
                        InterfaceIndex: interface.ipv4_index,
                        QueryName: query.name.as_mut_ptr(),
                        pResolveCompletionCallback: Some(resolve_callback),
                        pQueryContext: context,
                    },
                    &mut *query.cancel,
                ),
            }
        };
        query.pending = status == DNS_REQUEST_PENDING;
        if query.pending {
            Ok(query)
        } else {
            Err(io::Error::from_raw_os_error(status))
        }
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        let registered = CALLBACKS.lock().unwrap().remove(&self.id).is_some();
        if registered && self.pending {
            // SAFETY: This handle belongs to a request successfully started
            // with the same API. Completion may race with cancellation. Any
            // resulting callback still owns and frees its native result.
            unsafe {
                match self.kind {
                    Kind::Browse => DnsServiceBrowseCancel(&*self.cancel),
                    Kind::Resolve => DnsServiceResolveCancel(&*self.cancel),
                };
            }
        }
    }
}

unsafe extern "system" fn browse_callback(
    status: u32,
    context: *const c_void,
    records: *const DNS_RECORDW,
) {
    let callback = {
        let mut callbacks = CALLBACKS.lock().unwrap();
        if status == ERROR_CANCELLED {
            callbacks.remove(&context.addr());
            None
        } else {
            callbacks.get(&context.addr()).cloned()
        }
    };
    if status == ERROR_SUCCESS
        && let Some(callback) = callback
    {
        let mut next = records;
        // SAFETY: DNS-SD supplies a linked list valid for the callback.
        while let Some(record) = unsafe { next.as_ref() } {
            next = record.pNext;
            if record.wType == DNS_TYPE_PTR && record.dwTtl != 0 {
                // SAFETY: The type designates PTR data. Windows supplies the
                // target as a NUL-terminated UTF-16 name.
                if let Some(name) = unsafe { read_wide(record.Data.PTR.pNameHost) }
                    && is_service_instance(&name)
                {
                    let _ = callback
                        .sender
                        .try_send(Event::Browse(callback.interface, name));
                }
            }
        }
    }
    if !records.is_null() {
        // SAFETY: The callback transferred ownership of this record list.
        unsafe { DnsFree(records.cast(), DnsFreeRecordList) };
    }
}

unsafe extern "system" fn resolve_callback(
    status: u32,
    context: *const c_void,
    instance: *const DNS_SERVICE_INSTANCE,
) {
    let id = context.addr();
    let callback = CALLBACKS.lock().unwrap().remove(&id);
    let result = if status == ERROR_SUCCESS
        && let Some(callback) = &callback
    {
        // SAFETY: Any non-null instance and its fields are valid until freed.
        unsafe {
            instance
                .as_ref()
                .map(|service| read_service(service, callback.interface))
        }
    } else {
        None
    };
    if !instance.is_null() {
        // SAFETY: The callback transferred ownership of this service instance.
        unsafe { DnsServiceFreeInstance(instance) };
    }
    if let Some(callback) = callback {
        let _ = callback.sender.try_send(Event::Resolve(id, result));
    }
}

fn is_service_instance(name: &str) -> bool {
    let name = name.strip_suffix('.').unwrap_or(name);
    name.len() > SERVICE.len() + 1
        && name.as_bytes()[name.len() - SERVICE.len() - 1] == b'.'
        && name.as_bytes()[name.len() - SERVICE.len()..].eq_ignore_ascii_case(SERVICE.as_bytes())
}

unsafe fn read_service(service: &DNS_SERVICE_INSTANCE, interface: Interface) -> Advertisement {
    let (mut ipv4, mut ipv6) = (None, None);
    let port = service.wPort;
    if port != 0 {
        // SAFETY: Non-null address fields point to initialized address values
        // owned by the native instance. Both are in network byte order.
        unsafe {
            ipv4 = service
                .ip4Address
                .as_ref()
                .map(|ip| Ipv4Addr::from(ip.to_ne_bytes()))
                .filter(|ip| usable_ipv4(*ip) && !ip.is_multicast() && !ip.is_broadcast())
                .map(|ip| SocketAddrV4::new(ip, port));
            ipv6 = service
                .ip6Address
                .as_ref()
                .and_then(|ip| socket_v6(Ipv6Addr::from(ip.IP6Byte), port, interface.ipv6_index));
        }
    }
    let mut advertisement = Advertisement {
        ipv4,
        ipv6,
        identifier: None,
        auth_tag: None,
    };
    if !service.keys.is_null() && !service.values.is_null() {
        for index in 0..service.dwPropertyCount as usize {
            // SAFETY: Both Windows-owned arrays have dwPropertyCount entries.
            // A non-null key or value is a NUL-terminated UTF-16 string.
            unsafe {
                let Some(key) = read_wide(*service.keys.add(index)) else {
                    continue;
                };
                if key.eq_ignore_ascii_case("identifier") && advertisement.identifier.is_none() {
                    advertisement.identifier =
                        Some(read_wide(*service.values.add(index)).unwrap_or_default());
                } else if key.eq_ignore_ascii_case("authTag") && advertisement.auth_tag.is_none() {
                    advertisement.auth_tag =
                        Some(read_wide(*service.values.add(index)).unwrap_or_default());
                }
            }
        }
    }
    advertisement
}

unsafe fn read_wide(pointer: PCWSTR) -> Option<String> {
    if pointer.is_null() {
        return None;
    }
    let mut len = 0;
    // SAFETY: Caller supplies a NUL-terminated UTF-16 string or a null pointer.
    unsafe {
        while *pointer.add(len) != 0 {
            len += 1;
        }
        String::from_utf16(std::slice::from_raw_parts(pointer, len)).ok()
    }
}

fn socket_v6(ip: Ipv6Addr, port: u16, interface_index: u32) -> Option<SocketAddrV6> {
    if ip.is_unspecified() || ip.is_loopback() || ip.is_multicast() || ip.to_ipv4_mapped().is_some()
    {
        return None;
    }
    let scope = if ip.is_unicast_link_local() {
        if interface_index == 0 {
            return None;
        }
        interface_index
    } else {
        0
    };
    Some(SocketAddrV6::new(ip, port, 0, scope))
}

fn usable_ipv4(ip: Ipv4Addr) -> bool {
    ip.octets()[0] != 0 && !ip.is_loopback() && !ip.is_link_local()
}

fn ipv4_interfaces() -> io::Result<Vec<Interface>> {
    let mut bytes = 15 * 1024;
    for _ in 0..3 {
        // The Windows adapter list contains 64-bit alignment unions.
        let mut buffer = vec![0u64; (bytes as usize).div_ceil(size_of::<u64>())];
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: head is aligned and points to at least bytes writable bytes.
        let status = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_INET),
                GAA_FLAG_SKIP_ANYCAST
                    | GAA_FLAG_SKIP_DNS_SERVER
                    | GAA_FLAG_SKIP_FRIENDLY_NAME
                    | GAA_FLAG_SKIP_MULTICAST,
                null(),
                head,
                &mut bytes,
            )
        };
        match status {
            ERROR_BUFFER_OVERFLOW => continue,
            ERROR_NO_DATA => return Ok(Vec::new()),
            ERROR_SUCCESS => {
                // SAFETY: Successful enumeration populated the linked list
                // and its address data in the live, aligned buffer.
                return Ok(unsafe { read_interfaces(head) });
            }
            error => return Err(io::Error::from_raw_os_error(error as i32)),
        }
    }
    Err(io::Error::from_raw_os_error(ERROR_BUFFER_OVERFLOW as i32))
}

unsafe fn read_interfaces(mut next: *const IP_ADAPTER_ADDRESSES_LH) -> Vec<Interface> {
    let mut interfaces = Vec::new();
    // SAFETY: All linked list pointers originate in the enumeration buffer.
    unsafe {
        while let Some(adapter) = next.as_ref() {
            next = adapter.Next;
            let index = adapter.Anonymous1.Anonymous.IfIndex;
            if adapter.OperStatus != IfOperStatusUp || index == 0 {
                continue;
            }
            let mut unicast = adapter.FirstUnicastAddress;
            while let Some(address) = unicast.as_ref() {
                unicast = address.Next;
                let sockaddr = address.Address;
                if sockaddr.iSockaddrLength >= size_of::<SOCKADDR_IN>() as i32
                    && let Some(sa) = sockaddr.lpSockaddr.as_ref()
                    && sa.sa_family == AF_INET
                {
                    let sa = &*sockaddr.lpSockaddr.cast::<SOCKADDR_IN>();
                    if usable_ipv4(Ipv4Addr::from(sa.sin_addr.S_un.S_addr.to_ne_bytes())) {
                        interfaces.push(Interface {
                            ipv4_index: index,
                            ipv6_index: adapter.Ipv6IfIndex,
                        });
                        break;
                    }
                }
            }
        }
    }
    interfaces
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::NetworkManagement::Dns::IP6_ADDRESS;

    use super::*;

    #[test]
    fn filters_ipv4_addresses_and_scopes_ipv6() {
        for address in ["0.1.2.3", "127.1.2.3", "169.254.1.2"] {
            assert!(!usable_ipv4(address.parse().unwrap()));
        }
        assert!(usable_ipv4("10.0.0.1".parse().unwrap()));
        assert!(usable_ipv4("198.18.0.1".parse().unwrap()));
        assert_eq!(
            socket_v6("fe80::2".parse().unwrap(), 123, 7)
                .unwrap()
                .scope_id(),
            7
        );
        assert_eq!(
            socket_v6("fd01::2".parse().unwrap(), 123, 7)
                .unwrap()
                .scope_id(),
            0
        );
        assert!(socket_v6("fe80::2".parse().unwrap(), 123, 0).is_none());
    }

    #[test]
    fn accepts_only_full_remote_pairing_instance_names() {
        assert!(is_service_instance("test._RemotePairing._tcp.local."));
        assert!(is_service_instance("任意名称._remotepairing._tcp.local"));
        assert!(!is_service_instance("_remotepairing._tcp.local."));
        assert!(!is_service_instance("test_remotepairing._tcp.local"));
        assert!(!is_service_instance("test._remotepairing._tcp.example"));
    }

    #[test]
    fn copies_windows_addresses_port_and_txt_values() {
        let mut ipv4 = u32::from_ne_bytes([192, 0, 2, 7]);
        let mut ipv6 = IP6_ADDRESS {
            IP6Byte: "fe80::123".parse::<Ipv6Addr>().unwrap().octets(),
        };
        let entries = ["IDENTIFIER", "AutHtaG", "test-id", "test-tag"];
        let [mut id, mut tag, mut id_value, mut tag_value] =
            entries.map(|text| text.encode_utf16().chain([0]).collect::<Vec<_>>());
        let mut keys = [id.as_mut_ptr(), tag.as_mut_ptr()];
        let mut values = [id_value.as_mut_ptr(), tag_value.as_mut_ptr()];
        let instance = DNS_SERVICE_INSTANCE {
            pszInstanceName: null_mut(),
            pszHostName: null_mut(),
            ip4Address: &mut ipv4,
            ip6Address: &mut ipv6,
            wPort: 49152,
            wPriority: 0,
            wWeight: 0,
            dwPropertyCount: keys.len() as u32,
            keys: keys.as_mut_ptr(),
            values: values.as_mut_ptr(),
            dwInterfaceIndex: 4,
        };
        let interface = Interface {
            ipv4_index: 4,
            ipv6_index: 7,
        };
        // SAFETY: Every non-null field points to initialized values in this scope.
        let result = unsafe { read_service(&instance, interface) };
        assert_eq!(result.ipv4, Some("192.0.2.7:49152".parse().unwrap()));
        assert_eq!(result.ipv6, Some("[fe80::123%7]:49152".parse().unwrap()));
        assert_eq!(result.identifier.as_deref(), Some("test-id"));
        assert_eq!(result.auth_tag.as_deref(), Some("test-tag"));
    }
}
