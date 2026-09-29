//! Bindings to the private `Sharing.framework` C API (`SFBrowser`, `SFNode`, `SFOperation`).
//!
//! This is the API Finder's AirDrop window uses. All work is delegated to `sharingd`, so no UI is
//! shown. The framework lives only in the dyld shared cache, so it is loaded with `dlopen`.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::OnceLock;

use core_foundation::base::{CFAllocatorRef, CFIndex, CFTypeRef};
use core_foundation::dictionary::CFDictionaryRef;
use core_foundation::string::CFStringRef;

pub type SFBrowserRef = *mut c_void;
pub type SFNodeRef = *mut c_void;
pub type SFOperationRef = *mut c_void;

#[repr(C)]
pub struct ClientContext {
    pub version: CFIndex,
    pub info: *mut c_void,
    pub retain: *const c_void,
    pub release: *const c_void,
    pub copy_description: *const c_void,
}

impl ClientContext {
    pub fn new(info: *mut c_void) -> Self {
        Self {
            version: 0,
            info,
            retain: std::ptr::null(),
            release: std::ptr::null(),
            copy_description: std::ptr::null(),
        }
    }
}

pub type BrowserCallback =
    extern "C" fn(SFBrowserRef, SFNodeRef, CFStringRef, u32, i32, *mut c_void);
pub type OperationCallback = extern "C" fn(SFOperationRef, CFIndex, CFDictionaryRef, *mut c_void);

/// `SFOperationEvent` values.
pub mod event {
    pub const NEW_OPERATION: isize = 1;
    pub const ASK_USER: isize = 2;
    pub const WAIT_FOR_ANSWER: isize = 3;
    pub const CANCELED: isize = 4;
    pub const STARTED: isize = 5;
    pub const PREPROCESS: isize = 6;
    pub const PROGRESS: isize = 7;
    pub const POSTPROCESS: isize = 8;
    pub const FINISHED: isize = 9;
    pub const ERROR: isize = 10;
    pub const CONNECTING: isize = 11;
    pub const INFORMATION: isize = 12;
    pub const CONFLICT: isize = 13;
    pub const BLOCKED: isize = 14;

    pub fn name(e: isize) -> &'static str {
        match e {
            NEW_OPERATION => "new_operation",
            ASK_USER => "ask_user",
            WAIT_FOR_ANSWER => "waiting_for_answer",
            CANCELED => "canceled",
            STARTED => "started",
            PREPROCESS => "preprocess",
            PROGRESS => "progress",
            POSTPROCESS => "postprocess",
            FINISHED => "finished",
            ERROR => "error",
            CONNECTING => "connecting",
            INFORMATION => "information",
            CONFLICT => "conflict",
            BLOCKED => "blocked",
            _ => "unknown",
        }
    }
}

type NodeCopyFn = unsafe extern "C" fn(SFNodeRef) -> CFTypeRef;

pub struct Sharing {
    pub browser_create: unsafe extern "C" fn(CFAllocatorRef, CFStringRef) -> SFBrowserRef,
    pub browser_set_client: unsafe extern "C" fn(SFBrowserRef, BrowserCallback, *mut ClientContext),
    pub browser_set_dispatch_queue: unsafe extern "C" fn(SFBrowserRef, *const c_void),
    pub browser_open_node: unsafe extern "C" fn(SFBrowserRef, SFNodeRef, CFStringRef, u64),
    pub browser_copy_children: unsafe extern "C" fn(SFBrowserRef, SFNodeRef) -> CFTypeRef,
    pub browser_invalidate: unsafe extern "C" fn(SFBrowserRef),

    pub operation_create: unsafe extern "C" fn(
        CFAllocatorRef,
        CFStringRef,
        *const c_void,
        *const c_void,
    ) -> SFOperationRef,
    pub operation_set_client:
        unsafe extern "C" fn(SFOperationRef, OperationCallback, *mut ClientContext),
    pub operation_set_dispatch_queue: unsafe extern "C" fn(SFOperationRef, *const c_void),
    pub operation_set_property: unsafe extern "C" fn(SFOperationRef, CFStringRef, CFTypeRef),
    pub operation_resume: unsafe extern "C" fn(SFOperationRef),
    pub operation_cancel: unsafe extern "C" fn(SFOperationRef),

    pub node_copy_display_name: NodeCopyFn,
    pub node_copy_computer_name: NodeCopyFn,
    pub node_copy_secondary_name: NodeCopyFn,
    pub node_copy_real_name: NodeCopyFn,
    pub node_copy_model: NodeCopyFn,
    pub node_copy_kinds: NodeCopyFn,
    pub node_create: unsafe extern "C" fn(CFAllocatorRef, CFStringRef, CFStringRef) -> SFNodeRef,
    pub node_set_real_name: unsafe extern "C" fn(SFNodeRef, CFStringRef),
    pub node_set_display_name: unsafe extern "C" fn(SFNodeRef, CFStringRef),
    pub node_set_kinds: unsafe extern "C" fn(SFNodeRef, CFTypeRef),
    pub k_node_kind_airdrop: CFStringRef,
    pub node_add_domain: unsafe extern "C" fn(SFNodeRef, CFStringRef),
    pub node_set_domain: unsafe extern "C" fn(SFNodeRef, CFStringRef),
    pub node_set_service_name: unsafe extern "C" fn(SFNodeRef, CFStringRef),
    pub node_add_bonjour_protocol: unsafe extern "C" fn(SFNodeRef, CFStringRef),

    pub k_browser_kind_airdrop: CFStringRef,
    pub k_operation_kind_sender: CFStringRef,
    pub k_operation_items: CFStringRef,
    pub k_operation_node: CFStringRef,
    pub k_operation_error: CFStringRef,
    pub k_operation_bytes_copied: CFStringRef,
    pub k_operation_total_bytes: CFStringRef,
}

// The CFStringRef constants are immutable framework globals.
unsafe impl Send for Sharing {}
unsafe impl Sync for Sharing {}

const SHARING_PATH: &CStr = c"/System/Library/PrivateFrameworks/Sharing.framework/Sharing";
const RTLD_NOW: c_int = 2;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    static _dispatch_main_q: u8;
}

/// `dispatch_get_main_queue()` is a macro over this symbol.
pub fn main_queue() -> *const c_void {
    (&raw const _dispatch_main_q).cast()
}

fn dl_error() -> String {
    let e = unsafe { dlerror() };
    if e.is_null() {
        "unknown dlopen error".into()
    } else {
        unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned()
    }
}

impl Sharing {
    pub fn get() -> Result<&'static Sharing, String> {
        static LIB: OnceLock<Result<Sharing, String>> = OnceLock::new();
        LIB.get_or_init(|| unsafe { Self::load() })
            .as_ref()
            .map_err(Clone::clone)
    }

    unsafe fn load() -> Result<Sharing, String> {
        let handle = unsafe { dlopen(SHARING_PATH.as_ptr(), RTLD_NOW) };
        if handle.is_null() {
            return Err(format!("cannot load Sharing.framework: {}", dl_error()));
        }
        let sym = |name: &CStr| -> Result<*mut c_void, String> {
            let p = unsafe { dlsym(handle, name.as_ptr()) };
            if p.is_null() {
                Err(format!(
                    "Sharing.framework is missing symbol {}",
                    name.to_string_lossy()
                ))
            } else {
                Ok(p)
            }
        };
        macro_rules! func {
            ($name:literal) => {
                unsafe { std::mem::transmute(sym($name)?) }
            };
        }
        macro_rules! string_const {
            ($name:literal) => {
                unsafe { *(sym($name)? as *const CFStringRef) }
            };
        }
        Ok(Sharing {
            browser_create: func!(c"SFBrowserCreate"),
            browser_set_client: func!(c"SFBrowserSetClient"),
            browser_set_dispatch_queue: func!(c"SFBrowserSetDispatchQueue"),
            browser_open_node: func!(c"SFBrowserOpenNode"),
            browser_copy_children: func!(c"SFBrowserCopyChildren"),
            browser_invalidate: func!(c"SFBrowserInvalidate"),

            operation_create: func!(c"SFOperationCreate"),
            operation_set_client: func!(c"SFOperationSetClient"),
            operation_set_dispatch_queue: func!(c"SFOperationSetDispatchQueue"),
            operation_set_property: func!(c"SFOperationSetProperty"),
            operation_resume: func!(c"SFOperationResume"),
            operation_cancel: func!(c"SFOperationCancel"),

            node_copy_display_name: func!(c"SFNodeCopyDisplayName"),
            node_copy_computer_name: func!(c"SFNodeCopyComputerName"),
            node_copy_secondary_name: func!(c"SFNodeCopySecondaryName"),
            node_copy_real_name: func!(c"SFNodeCopyRealName"),
            node_copy_model: func!(c"SFNodeCopyModel"),
            node_copy_kinds: func!(c"SFNodeCopyKinds"),
            node_create: func!(c"SFNodeCreate"),
            node_set_real_name: func!(c"SFNodeSetRealName"),
            node_set_display_name: func!(c"SFNodeSetDisplayName"),
            node_set_kinds: func!(c"SFNodeSetKinds"),
            k_node_kind_airdrop: string_const!(c"kSFNodeKindAirDrop"),
            node_add_domain: func!(c"SFNodeAddDomain"),
            node_set_domain: func!(c"SFNodeSetDomain"),
            node_set_service_name: func!(c"SFNodeSetServiceName"),
            node_add_bonjour_protocol: func!(c"SFNodeAddBonjourProtocol"),

            k_browser_kind_airdrop: string_const!(c"kSFBrowserKindAirDrop"),
            k_operation_kind_sender: string_const!(c"kSFOperationKindSender"),
            k_operation_items: string_const!(c"kSFOperationItemsKey"),
            k_operation_node: string_const!(c"kSFOperationNodeKey"),
            k_operation_error: string_const!(c"kSFOperationErrorKey"),
            k_operation_bytes_copied: string_const!(c"kSFOperationBytesCopiedKey"),
            k_operation_total_bytes: string_const!(c"kSFOperationTotalBytesKey"),
        })
    }
}
