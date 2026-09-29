use std::ffi::{CStr, c_char, c_void};

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
    fn DNSServiceProcessResult(sd: DNSServiceRef) -> i32;
}

extern "C" fn reply(
    _sd: DNSServiceRef,
    flags: u32,
    iface: u32,
    err: i32,
    name: *const c_char,
    ty: *const c_char,
    dom: *const c_char,
    _c: *mut c_void,
) {
    let s = |p: *const c_char| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    eprintln!(
        "dnssd flags={flags:#x} if={iface} err={err} name={} type={} dom={}",
        s(name),
        s(ty),
        s(dom)
    );
}

pub fn spawn() {
    std::thread::spawn(|| unsafe {
        let mut sd: DNSServiceRef = std::ptr::null_mut();
        let e = DNSServiceBrowse(
            &mut sd,
            0x100000,
            0,
            c"_airdrop._tcp".as_ptr(),
            std::ptr::null(),
            reply,
            std::ptr::null_mut(),
        );
        eprintln!("browse start {e}");
        loop {
            if DNSServiceProcessResult(sd) != 0 {
                break;
            }
        }
    });
}

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
}
extern "C" fn rreply(
    _sd: DNSServiceRef,
    _f: u32,
    iface: u32,
    err: i32,
    full: *const c_char,
    host: *const c_char,
    port: u16,
    len: u16,
    txt: *const u8,
    _c: *mut c_void,
) {
    let s = |p: *const c_char| unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    let t = unsafe { std::slice::from_raw_parts(txt, len as usize) };
    eprintln!(
        "resolved if={iface} err={err} full={} host={} port={} txt={:?}",
        s(full),
        s(host),
        u16::from_be(port),
        String::from_utf8_lossy(t)
    );
}
pub fn resolve(name: &str) {
    let n = std::ffi::CString::new(name).unwrap();
    std::thread::spawn(move || unsafe {
        let mut sd: DNSServiceRef = std::ptr::null_mut();
        DNSServiceResolve(
            &mut sd,
            0x100000,
            0,
            n.as_ptr(),
            c"_airdrop._tcp".as_ptr(),
            c"local.".as_ptr(),
            rreply,
            std::ptr::null_mut(),
        );
        loop {
            if DNSServiceProcessResult(sd) != 0 {
                break;
            }
        }
    });
}
