use crate::sharing::*;
use core_foundation::array::CFArray;
use core_foundation::base::{CFIndex, CFType, TCFType, kCFAllocatorDefault};
use core_foundation::dictionary::CFDictionaryRef;
use core_foundation::runloop::{
    CFRunLoopGetMain, CFRunLoopRunInMode, CFRunLoopStop, kCFRunLoopDefaultMode,
};
use core_foundation::string::CFString;
use core_foundation::url::CFURL;
use core_foundation_sys::url::CFURLCreateWithString;
use std::ffi::c_void;

extern "C" fn op_cb(_op: SFOperationRef, ev: CFIndex, res: CFDictionaryRef, _i: *mut c_void) {
    let d = if res.is_null() {
        "null".to_string()
    } else {
        format!("{:?}", unsafe { CFType::wrap_under_get_rule(res as _) })
    };
    eprintln!("op event {ev} ({}) {d}", event::name(ev));
    if ev == event::FINISHED || ev == event::ERROR || ev == event::CANCELED {
        unsafe { CFRunLoopStop(CFRunLoopGetMain()) };
    }
}

pub fn run(id: &str, file: &str) {
    let s = Sharing::get().unwrap();
    unsafe {
        let empty = CFString::new("");
        let node = (s.node_create)(
            kCFAllocatorDefault,
            empty.as_concrete_TypeRef(),
            empty.as_concrete_TypeRef(),
        );
        let idc = CFString::new(id);
        (s.node_set_real_name)(node, idc.as_concrete_TypeRef());
        (s.node_set_display_name)(node, idc.as_concrete_TypeRef());
        if std::env::var("DOM").is_ok() {
            (s.node_set_domain)(node, CFString::new("local.").as_concrete_TypeRef());
        }
        if std::env::var("PROTO").is_ok() {
            (s.node_add_bonjour_protocol)(
                node,
                CFString::new("_airdrop._tcp.").as_concrete_TypeRef(),
            );
        }
        if std::env::var("SVC").is_ok() {
            (s.node_set_service_name)(node, idc.as_concrete_TypeRef());
        }
        let kinds = CFArray::from_CFTypes(&[CFString::wrap_under_get_rule(s.k_node_kind_airdrop)]);
        (s.node_set_kinds)(node, kinds.as_CFTypeRef());
        let url = if file.contains("://") {
            let u = CFString::new(file);
            CFURL::wrap_under_create_rule(CFURLCreateWithString(
                kCFAllocatorDefault,
                u.as_concrete_TypeRef(),
                std::ptr::null(),
            ))
        } else {
            CFURL::from_path(std::fs::canonicalize(file).unwrap(), false).unwrap()
        };
        let items = CFArray::from_CFTypes(&[url]);
        let op = (s.operation_create)(
            kCFAllocatorDefault,
            s.k_operation_kind_sender,
            std::ptr::null(),
            std::ptr::null(),
        );
        (s.operation_set_property)(op, s.k_operation_items, items.as_CFTypeRef());
        (s.operation_set_property)(op, s.k_operation_node, node as _);
        (s.operation_set_dispatch_queue)(op, main_queue());
        let mut ctx = ClientContext::new(std::ptr::null_mut());
        (s.operation_set_client)(op, op_cb, &mut ctx);
        if std::env::var("BROWSE").is_ok() {
            let b = (s.browser_create)(kCFAllocatorDefault, s.k_browser_kind_airdrop);
            (s.browser_set_dispatch_queue)(b, main_queue());
            let mut bctx = ClientContext::new(std::ptr::null_mut());
            (s.browser_set_client)(b, crate::cb, &mut bctx);
            (s.browser_open_node)(b, std::ptr::null_mut(), std::ptr::null(), 0);
            std::mem::forget(bctx);
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 3.0, 0);
        }
        (s.operation_resume)(op);
        CFRunLoopRunInMode(kCFRunLoopDefaultMode, 45.0, 0);
    }
}
