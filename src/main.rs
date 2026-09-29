mod dnssd_probe;
mod send_probe;
mod sharing;

use std::ffi::c_void;

use core_foundation::array::CFArray;
use core_foundation::base::{CFType, CFTypeRef, TCFType, kCFAllocatorDefault};
use core_foundation::runloop::{CFRunLoopRunInMode, kCFRunLoopDefaultMode};
use core_foundation::string::CFStringRef;
use sharing::*;

fn desc(r: CFTypeRef) -> String {
    if r.is_null() {
        return "null".into();
    }
    let t = unsafe { CFType::wrap_under_create_rule(r) };
    format!("{t:?}")
}

extern "C" fn cb(
    b: SFBrowserRef,
    node: SFNodeRef,
    _p: CFStringRef,
    flags: u32,
    err: i32,
    _i: *mut c_void,
) {
    let s = Sharing::get().unwrap();
    eprintln!("callback flags={flags} err={err}");
    let children = unsafe { (s.browser_copy_children)(b, node) };
    if children.is_null() {
        return;
    }
    let arr: CFArray<CFType> = unsafe { CFArray::wrap_under_create_rule(children as _) };
    for n in arr.iter() {
        let n = n.as_CFTypeRef() as SFNodeRef;
        unsafe {
            eprintln!(
                "  node display={} computer={} secondary={} real={} model={} kinds={}",
                desc((s.node_copy_display_name)(n)),
                desc((s.node_copy_computer_name)(n)),
                desc((s.node_copy_secondary_name)(n)),
                desc((s.node_copy_real_name)(n)),
                desc((s.node_copy_model)(n)),
                desc((s.node_copy_kinds)(n)),
            );
        }
    }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    if a.len() == 4 && a[1] == "send" {
        send_probe::run(&a[2], &a[3]);
        return;
    }
    dnssd_probe::spawn();
    if let Ok(r) = std::env::var("RES") {
        dnssd_probe::resolve(&r);
    }
    let s = Sharing::get().unwrap();
    unsafe {
        let b = (s.browser_create)(kCFAllocatorDefault, s.k_browser_kind_airdrop);
        (s.browser_set_dispatch_queue)(b, main_queue());
        let mut ctx = ClientContext::new(std::ptr::null_mut());
        (s.browser_set_client)(b, cb, &mut ctx);
        (s.browser_open_node)(b, std::ptr::null_mut(), std::ptr::null(), 0);
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 15.0, 0);
        (s.browser_invalidate)(b);
    }
}
