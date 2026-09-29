//! Finds AirDrop receivers by browsing `_airdrop._tcp` with `dns_sd`, including AWDL.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_void};
use std::time::Duration;

use core_foundation::runloop::{CFRunLoopRunInMode, kCFRunLoopDefaultMode};
use serde::Serialize;
use serde_json::Value;

use crate::ble;
use crate::caps::{Features, Media};
use crate::presence;
use crate::probe;
use crate::sharing::{Activation, Sharing, main_queue};

type DNSServiceRef = *mut c_void;

type BrowseReply = extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut c_void,
);
type ResolveReply = extern "C" fn(
    DNSServiceRef,
    u32,
    u32,
    i32,
    *const c_char,
    *const c_char,
    u16,
    u16,
    *const u8,
    *mut c_void,
);
type AddrInfoReply =
    extern "C" fn(DNSServiceRef, u32, u32, i32, *const c_char, *const u8, u32, *mut c_void);

unsafe extern "C" {
    fn DNSServiceBrowse(
        sd: *mut DNSServiceRef,
        flags: u32,
        iface: u32,
        regtype: *const c_char,
        domain: *const c_char,
        cb: BrowseReply,
        ctx: *mut c_void,
    ) -> i32;
    fn DNSServiceResolve(
        sd: *mut DNSServiceRef,
        flags: u32,
        iface: u32,
        name: *const c_char,
        regtype: *const c_char,
        domain: *const c_char,
        cb: ResolveReply,
        ctx: *mut c_void,
    ) -> i32;
    fn DNSServiceGetAddrInfo(
        sd: *mut DNSServiceRef,
        flags: u32,
        iface: u32,
        protocol: u32,
        hostname: *const c_char,
        cb: AddrInfoReply,
        ctx: *mut c_void,
    ) -> i32;
    fn DNSServiceSetDispatchQueue(sd: DNSServiceRef, queue: *const c_void) -> i32;
    fn DNSServiceRefDeallocate(sd: DNSServiceRef);
    fn if_indextoname(index: u32, name: *mut c_char) -> *mut c_char;
    fn if_nametoindex(name: *const c_char) -> u32;
    fn getifaddrs(out: *mut *mut IfAddrs) -> i32;
    fn freeifaddrs(list: *mut IfAddrs);
}

/// `struct ifaddrs` from `<ifaddrs.h>`.
#[repr(C)]
struct IfAddrs {
    next: *mut IfAddrs,
    name: *const c_char,
    flags: u32,
    addr: *const u8,
    netmask: *const u8,
    dstaddr: *const u8,
    data: *mut c_void,
}

const FLAG_ADD: u32 = 0x2;
const FLAG_INCLUDE_AWDL: u32 = 0x10_0000;
const SERVICE_TYPE: &CStr = c"_airdrop._tcp";

/// Everything `list` finds.
#[derive(Debug, Clone, Serialize)]
pub struct Scan {
    pub peers: Vec<Peer>,
    /// Devices advertising AirDrop receiving on over Bluetooth LE, nearest first, e.g.
    /// Contacts Only receivers without us in their contacts. Cannot be matched to `peers`,
    /// so receivers listed there show up here too.
    pub hidden: Vec<ble::BleDevice>,
    /// Why the Bluetooth scan failed, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden_error: Option<String>,
    /// Apple devices answering on AWDL that are not in `peers`: Contacts Only receivers
    /// without us in their contacts, and AirPlay, Sidecar or Universal Control peers.
    /// Anonymous: link-local addresses are randomized. Only probed in debug mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awdl: Option<Vec<AwdlDevice>>,
    /// Why the AWDL probe failed, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub awdl_error: Option<String>,
    /// Every Apple device heard over Bluetooth LE, nearest first. Only in debug mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ble: Option<Vec<ble::BleDevice>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AwdlDevice {
    pub address: String,
}

/// A receiver's AirDrop setting, inferred from its `/Discover` reply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Replied with its name.
    Everyone,
    /// Replied without a name. Such receivers only show up for senders whose Bluetooth
    /// short identity hash matches one of their contacts, so it likely has you saved
    /// (2-byte hashes collide, so not certainly).
    ContactsOnly,
}

/// A receiver advertising the AirDrop service.
#[derive(Debug, Clone, Serialize)]
pub struct Peer {
    /// Bonjour instance name. This is what `send` takes.
    pub id: String,
    /// Device name from the receiver's `/Discover` reply.
    pub name: Option<String>,
    /// Model name, when the receiver includes it in its reply.
    pub model: Option<String>,
    /// The receiver's `IsAirDropable` reply. It was true in both Everyone and Contacts Only
    /// modes, including from strangers, so it does not mean the receiver accepts from you.
    pub airdropable: Option<bool>,
    /// `None` when the receiver did not answer `/Discover`.
    pub mode: Option<Mode>,
    pub network: Network,
    /// Decoded TXT `flags`.
    pub features: Option<Features>,
    /// Decoded `ReceiverMediaCapabilities`.
    pub media: Option<Media>,
    /// Why `/Discover` failed, when it did.
    pub discover_error: Option<String>,
    pub raw: Raw,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Network {
    /// Interfaces the service was seen on, e.g. `awdl0`, `en0`.
    pub interfaces: Vec<String>,
    /// Resolved addresses; IPv6 link-local ones carry their `%interface` scope.
    pub addresses: Vec<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    /// The address that answered `/Discover`.
    pub responded_via: Option<String>,
    pub response_ms: Option<u64>,
}

/// Everything as received, for fields this tool does not interpret.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Raw {
    pub txt: BTreeMap<String, String>,
    pub discover: Option<Value>,
}

#[derive(Default)]
struct State {
    peers: BTreeMap<String, Peer>,
    resolving: Vec<DNSServiceRef>,
    error: Option<String>,
}

fn interface_name(index: u32) -> String {
    let mut buf = [0 as c_char; 17];
    let p = unsafe { if_indextoname(index, buf.as_mut_ptr()) };
    if p.is_null() {
        index.to_string()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

fn text(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
    }
}

/// Splits a DNS TXT record (length-prefixed `key=value` strings).
fn parse_txt(mut raw: &[u8]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    while let Some((&len, rest)) = raw.split_first() {
        let len = (len as usize).min(rest.len());
        let (entry, rest) = rest.split_at(len);
        raw = rest;
        let entry = String::from_utf8_lossy(entry);
        match entry.split_once('=') {
            Some((k, v)) => out.insert(k.to_owned(), v.to_owned()),
            None if !entry.is_empty() => out.insert(entry.into_owned(), String::new()),
            None => None,
        };
    }
    out
}

extern "C" fn on_browse(
    _sd: DNSServiceRef,
    flags: u32,
    iface: u32,
    err: i32,
    name: *const c_char,
    regtype: *const c_char,
    domain: *const c_char,
    ctx: *mut c_void,
) {
    let state = unsafe { &*(ctx as *const RefCell<State>) };
    let mut st = state.borrow_mut();
    if err != 0 {
        st.error = Some(format!("Bonjour browse failed (DNSServiceErrorType {err})"));
        return;
    }
    let id = text(name);
    let ifname = interface_name(iface);
    if flags & FLAG_ADD == 0 {
        if let Some(peer) = st.peers.get_mut(&id) {
            peer.network.interfaces.retain(|i| *i != ifname);
            if peer.network.interfaces.is_empty() {
                st.peers.remove(&id);
            }
        }
        return;
    }
    let peer = st.peers.entry(id.clone()).or_insert_with(|| Peer {
        id,
        name: None,
        model: None,
        airdropable: None,
        mode: None,
        network: Network::default(),
        features: None,
        media: None,
        discover_error: None,
        raw: Raw::default(),
    });
    if !peer.network.interfaces.contains(&ifname) {
        peer.network.interfaces.push(ifname);
    }
    let mut sd: DNSServiceRef = std::ptr::null_mut();
    let e = unsafe {
        DNSServiceResolve(
            &mut sd,
            FLAG_INCLUDE_AWDL,
            iface,
            name,
            regtype,
            domain,
            on_resolve,
            ctx,
        )
    };
    if e == 0 {
        unsafe { DNSServiceSetDispatchQueue(sd, main_queue()) };
        st.resolving.push(sd);
    }
}

extern "C" fn on_resolve(
    _sd: DNSServiceRef,
    _flags: u32,
    iface: u32,
    err: i32,
    fullname: *const c_char,
    host: *const c_char,
    port: u16,
    txt_len: u16,
    txt: *const u8,
    ctx: *mut c_void,
) {
    if err != 0 {
        return;
    }
    let state = unsafe { &*(ctx as *const RefCell<State>) };
    let fullname = text(fullname);
    let Some((id, _)) = fullname.split_once("._airdrop._tcp") else {
        return;
    };
    let raw = if txt.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(txt, txt_len as usize) }
    };
    let mut st = state.borrow_mut();
    let Some(peer) = st.peers.get_mut(id) else {
        return;
    };
    peer.network.host = Some(text(host).trim_end_matches('.').to_owned());
    peer.network.port = Some(u16::from_be(port));
    peer.raw.txt = parse_txt(raw);
    peer.features = peer
        .raw
        .txt
        .get("flags")
        .and_then(|f| f.parse().ok())
        .map(Features::decode);
    let mut sd: DNSServiceRef = std::ptr::null_mut();
    let e =
        unsafe { DNSServiceGetAddrInfo(&mut sd, FLAG_INCLUDE_AWDL, iface, 0, host, on_addr, ctx) };
    if e == 0 {
        unsafe { DNSServiceSetDispatchQueue(sd, main_queue()) };
        st.resolving.push(sd);
    }
}

/// Formats a `sockaddr_in`/`sockaddr_in6`.
fn sockaddr_text(sa: *const u8, iface: u32) -> Option<String> {
    const AF_INET: u8 = 2;
    const AF_INET6: u8 = 30;
    let family = unsafe { *sa.add(1) };
    match family {
        AF_INET => {
            let b: [u8; 4] = unsafe { *(sa.add(4) as *const [u8; 4]) };
            Some(std::net::Ipv4Addr::from(b).to_string())
        }
        AF_INET6 => {
            let mut b: [u8; 16] = unsafe { *(sa.add(8) as *const [u8; 16]) };
            if b[0] == 0xfe && b[1] & 0xc0 == 0x80 {
                // The kernel embeds the scope in bytes 2-3 of link-local addresses.
                b[2] = 0;
                b[3] = 0;
            }
            let ip = std::net::Ipv6Addr::from(b);
            Some(if ip.is_unicast_link_local() {
                format!("{ip}%{}", interface_name(iface))
            } else {
                ip.to_string()
            })
        }
        _ => None,
    }
}

extern "C" fn on_addr(
    _sd: DNSServiceRef,
    flags: u32,
    iface: u32,
    err: i32,
    host: *const c_char,
    addr: *const u8,
    _ttl: u32,
    ctx: *mut c_void,
) {
    if err != 0 || addr.is_null() || flags & FLAG_ADD == 0 {
        return;
    }
    let Some(addr) = sockaddr_text(addr, iface) else {
        return;
    };
    let state = unsafe { &*(ctx as *const RefCell<State>) };
    let host = text(host);
    let host = host.trim_end_matches('.');
    for peer in state.borrow_mut().peers.values_mut() {
        let net = &mut peer.network;
        if net.host.as_deref() == Some(host) && !net.addresses.contains(&addr) {
            net.addresses.push(addr.clone());
        }
    }
}

/// This Mac's own interface addresses, formatted like `Peer::addresses`.
fn local_addresses() -> Vec<String> {
    let mut list: *mut IfAddrs = std::ptr::null_mut();
    if unsafe { getifaddrs(&mut list) } != 0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut cur = list;
    while let Some(ifa) = unsafe { cur.as_ref() } {
        if !ifa.addr.is_null() {
            let index = unsafe { if_nametoindex(ifa.name) };
            out.extend(sockaddr_text(ifa.addr, index));
        }
        cur = ifa.next;
    }
    unsafe { freeifaddrs(list) };
    out
}

/// Asks every peer for its name in parallel, preferring AWDL addresses.
fn identify(peers: &mut [Peer], timeout: Duration) {
    std::thread::scope(|scope| {
        for peer in peers.iter_mut() {
            let (Some(host), Some(port)) = (peer.network.host.clone(), peer.network.port) else {
                continue;
            };
            scope.spawn(move || {
                let mut addresses = peer.network.addresses.clone();
                addresses.sort_by_key(|a| !a.ends_with("%awdl0"));
                match probe::discover(&addresses, port, &host, timeout) {
                    Ok(r) => {
                        let text = |k: &str| r.reply[k].as_str().map(str::to_owned);
                        peer.name = text("ReceiverComputerName");
                        peer.model = text("ReceiverModelName");
                        peer.airdropable = r.reply["IsAirDropable"].as_bool();
                        peer.mode = Some(if peer.name.is_some() {
                            Mode::Everyone
                        } else {
                            Mode::ContactsOnly
                        });
                        let caps = &r.reply["ReceiverMediaCapabilities"];
                        peer.media = caps.is_object().then(|| Media::parse(caps));
                        peer.network.responded_via = Some(r.via);
                        peer.network.response_ms = Some(r.response_ms);
                        peer.raw.discover = Some(r.reply);
                    }
                    Err(e) => peer.discover_error = Some(e),
                }
            });
        }
    });
}

/// Browses for `wait`, then returns every other receiver still advertising, with the names
/// they report, and the devices with AirDrop on that Bluetooth hears. `debug` also returns
/// AWDL ping responders and every Bluetooth device.
pub fn discover(wait: Duration, debug: bool) -> Result<Scan, String> {
    let sharing = Sharing::get()?;
    let _activation = Activation::start(sharing);
    // Listens during the browse, while the run loop delivers its callbacks.
    let ble_scan = ble::Scan::start();
    let state = Box::new(RefCell::new(State::default()));
    let ctx = &*state as *const RefCell<State> as *mut c_void;
    let mut browse: DNSServiceRef = std::ptr::null_mut();
    let e = unsafe {
        DNSServiceBrowse(
            &mut browse,
            FLAG_INCLUDE_AWDL,
            0,
            SERVICE_TYPE.as_ptr(),
            std::ptr::null(),
            on_browse,
            ctx,
        )
    };
    if e != 0 {
        return Err(format!(
            "cannot start Bonjour browse (DNSServiceErrorType {e})"
        ));
    }
    unsafe {
        DNSServiceSetDispatchQueue(browse, main_queue());
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, wait.as_secs_f64(), 0);
        DNSServiceRefDeallocate(browse);
    }
    let (ble, hidden_error) = match ble_scan.and_then(ble::Scan::finish) {
        Ok(devices) => (devices, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let hidden = ble
        .iter()
        .filter(|d| d.airdrop == Some(true))
        .cloned()
        .collect();
    let mut st = state.borrow_mut();
    for sd in st.resolving.drain(..) {
        unsafe { DNSServiceRefDeallocate(sd) };
    }
    if let Some(e) = st.error.take() {
        return Err(e);
    }
    let local = local_addresses();
    let mut peers: Vec<Peer> = std::mem::take(&mut st.peers)
        .into_values()
        .filter(|p| !p.network.addresses.iter().any(|a| local.contains(a)))
        .collect();
    drop(st);
    // Still inside the activation, so AWDL stays up while the peers are asked.
    let neighbours = std::thread::scope(|scope| {
        let probe =
            debug.then(|| scope.spawn(|| presence::awdl_neighbours(Duration::from_millis(1500))));
        identify(&mut peers, Duration::from_secs(4));
        probe.map(|p| {
            p.join()
                .unwrap_or_else(|_| Err("presence probe panicked".into()))
        })
    });
    let seen: Vec<&String> = peers.iter().flat_map(|p| &p.network.addresses).collect();
    let (awdl, awdl_error) = match neighbours {
        None => (None, None),
        Some(Ok(found)) => (
            Some(
                found
                    .into_iter()
                    .filter(|a| !local.contains(a) && !seen.contains(&a))
                    .map(|address| AwdlDevice { address })
                    .collect(),
            ),
            None,
        ),
        Some(Err(e)) => (Some(Vec::new()), Some(e)),
    };
    Ok(Scan {
        peers,
        hidden,
        hidden_error,
        awdl,
        awdl_error,
        ble: debug.then_some(ble),
    })
}

#[cfg(test)]
mod tests {
    use super::parse_txt;

    #[test]
    fn txt_record() {
        let t = parse_txt(b"\x0cflags=111611\x04bare\x00");
        assert_eq!(t["flags"], "111611");
        assert_eq!(t["bare"], "");
        assert_eq!(t.len(), 2);
    }

    #[test]
    fn truncated_txt_record() {
        let t = parse_txt(b"\x10ab=c");
        assert_eq!(t["ab"], "c");
    }
}
