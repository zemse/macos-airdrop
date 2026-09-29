//! Finds AirDrop receivers by browsing `_airdrop._tcp` with `dns_sd`, including AWDL.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::ffi::{CStr, c_char, c_void};
use std::time::Duration;

use core_foundation::runloop::{CFRunLoopRunInMode, kCFRunLoopDefaultMode};
use serde::Serialize;

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
    fn DNSServiceSetDispatchQueue(sd: DNSServiceRef, queue: *const c_void) -> i32;
    fn DNSServiceRefDeallocate(sd: DNSServiceRef);
    fn if_indextoname(index: u32, name: *mut c_char) -> *mut c_char;
}

const FLAG_ADD: u32 = 0x2;
const FLAG_INCLUDE_AWDL: u32 = 0x10_0000;
const SERVICE_TYPE: &CStr = c"_airdrop._tcp";

/// A receiver advertising the AirDrop service.
#[derive(Debug, Clone, Serialize)]
pub struct Peer {
    /// Bonjour instance name. This is what `send` takes.
    pub id: String,
    /// Interfaces the service was seen on, e.g. `awdl0`, `en0`.
    pub interfaces: Vec<String>,
    pub host: Option<String>,
    pub port: Option<u16>,
    /// The `flags` TXT value, a capability bitmask.
    pub flags: Option<u64>,
    pub txt: BTreeMap<String, String>,
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
            peer.interfaces.retain(|i| *i != ifname);
            if peer.interfaces.is_empty() {
                st.peers.remove(&id);
            }
        }
        return;
    }
    let peer = st.peers.entry(id.clone()).or_insert_with(|| Peer {
        id,
        interfaces: Vec::new(),
        host: None,
        port: None,
        flags: None,
        txt: BTreeMap::new(),
    });
    if !peer.interfaces.contains(&ifname) {
        peer.interfaces.push(ifname);
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
    _iface: u32,
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
    if let Some(peer) = state.borrow_mut().peers.get_mut(id) {
        peer.host = Some(text(host).trim_end_matches('.').to_owned());
        peer.port = Some(u16::from_be(port));
        peer.txt = parse_txt(raw);
        peer.flags = peer.txt.get("flags").and_then(|f| f.parse().ok());
    }
}

/// Browses for `wait`, then returns every receiver still advertising.
pub fn discover(wait: Duration) -> Result<Vec<Peer>, String> {
    let sharing = Sharing::get()?;
    let _activation = Activation::start(sharing);
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
    let mut st = state.borrow_mut();
    for sd in st.resolving.drain(..) {
        unsafe { DNSServiceRefDeallocate(sd) };
    }
    if let Some(e) = st.error.take() {
        return Err(e);
    }
    Ok(std::mem::take(&mut st.peers).into_values().collect())
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
