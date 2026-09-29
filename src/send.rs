//! Sends files and links to one receiver with an `SFOperation`.

use std::cell::RefCell;
use std::ffi::c_void;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use core_foundation::array::CFArray;
use core_foundation::base::{CFIndex, CFRelease, TCFType};
use core_foundation::dictionary::CFDictionaryRef;
use core_foundation::runloop::{
    CFRunLoopGetMain, CFRunLoopRunInMode, CFRunLoopStop, kCFRunLoopDefaultMode,
};
use core_foundation::string::CFString;
use core_foundation::url::CFURL;
use core_foundation_sys::url::CFURLCreateWithString;
use serde::Serialize;
use serde_json::Value;

use crate::cf;
use crate::sharing::{Activation, ClientContext, SFOperationRef, Sharing, event, main_queue};

/// Something to send: a local file or folder, or a URL.
#[derive(Debug, Clone)]
pub enum Item {
    Path(PathBuf),
    Url(String),
}

impl Item {
    /// Treats `s` as a path if it exists, otherwise as a URL if it has a scheme.
    pub fn parse(s: &str) -> Result<Item, String> {
        if let Ok(path) = std::fs::canonicalize(s) {
            return Ok(Item::Path(path));
        }
        let has_scheme = s.split_once(':').is_some_and(|(scheme, rest)| {
            !scheme.is_empty()
                && !rest.is_empty()
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
        });
        if has_scheme {
            Ok(Item::Url(s.to_owned()))
        } else {
            Err(format!(
                "{s:?} is neither an existing file nor a URL with a scheme (e.g. https://…)"
            ))
        }
    }

    fn to_cf_url(&self) -> Result<CFURL, String> {
        match self {
            Item::Path(p) => CFURL::from_path(p, p.is_dir())
                .ok_or_else(|| format!("cannot make a file URL for {}", p.display())),
            Item::Url(u) => {
                let s = CFString::new(u);
                let r = unsafe {
                    CFURLCreateWithString(
                        std::ptr::null(),
                        s.as_concrete_TypeRef(),
                        std::ptr::null(),
                    )
                };
                if r.is_null() {
                    Err(format!("invalid URL {u:?}"))
                } else {
                    Ok(unsafe { CFURL::wrap_under_create_rule(r) })
                }
            }
        }
    }
}

impl std::fmt::Display for Item {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Item::Path(p) => write!(f, "{}", p.display()),
            Item::Url(u) => f.write_str(u),
        }
    }
}

/// One `SFOperation` callback.
#[derive(Debug, Clone, Serialize)]
pub struct Event {
    pub event: &'static str,
    pub code: isize,
    pub data: Value,
}

/// Transfer progress carried by `started`, `progress` and `finished` events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub bytes: u64,
    pub total: u64,
    pub secs_left: Option<u64>,
}

impl Event {
    pub fn progress(&self) -> Option<Progress> {
        let num = |k: &str| self.data.get(k).and_then(Value::as_u64);
        Some(Progress {
            bytes: num("BytesCopied")?,
            total: num("TotalBytes").filter(|&t| t > 0)?,
            secs_left: num("TimeRemaining"),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// The receiver accepted and the transfer completed.
    Finished,
    /// The receiver declined, or the transfer was canceled.
    Canceled,
    /// `sharingd` reported an error.
    Failed,
    /// Nothing terminal happened before the timeout; the operation was canceled.
    TimedOut,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub outcome: Outcome,
    pub peer_id: String,
    pub receiver_name: Option<String>,
    pub receiver_model: Option<String>,
    pub items: Vec<String>,
    /// The last non-terminal event, i.e. how far the transfer got: `connecting`,
    /// `waiting_for_answer`, `preprocess`, `started`, `progress`, ...
    pub stage: Option<&'static str>,
    /// Payload of the error event, when `outcome` is `failed`.
    pub error: Option<Value>,
}

thread_local! {
    /// Events queued by the operation callback, drained by `send` on the same (main) thread.
    static QUEUE: RefCell<Vec<(SFOperationRef, Event)>> = const { RefCell::new(Vec::new()) };
}

extern "C" fn on_event(
    op: SFOperationRef,
    code: CFIndex,
    data: CFDictionaryRef,
    _info: *mut c_void,
) {
    let ev = Event {
        event: event::name(code),
        code,
        data: cf::to_json(data as _),
    };
    QUEUE.with_borrow_mut(|q| q.push((op, ev)));
    unsafe { CFRunLoopStop(CFRunLoopGetMain()) };
}

fn is_terminal(code: isize) -> bool {
    matches!(code, event::FINISHED | event::CANCELED | event::ERROR)
}

/// Sends `items` to the receiver `peer_id` and blocks until the transfer ends or `timeout`
/// passes. Must be called on the main thread. `on_event` sees every raw event as it arrives.
pub fn send(
    peer_id: &str,
    items: &[Item],
    timeout: Duration,
    mut on_event_cb: impl FnMut(&Event),
) -> Result<Report, String> {
    if peer_id.is_empty() || peer_id.contains('.') {
        return Err(format!(
            "invalid peer ID {peer_id:?}: use an ID printed by `airdrop list`"
        ));
    }
    if items.is_empty() {
        return Err("nothing to send".into());
    }
    let urls = items
        .iter()
        .map(Item::to_cf_url)
        .collect::<Result<Vec<_>, _>>()?;
    let s = Sharing::get()?;
    let _activation = Activation::start(s);

    let mut report = Report {
        outcome: Outcome::TimedOut,
        peer_id: peer_id.to_owned(),
        receiver_name: None,
        receiver_model: None,
        items: items.iter().map(ToString::to_string).collect(),
        stage: None,
        error: None,
    };

    let id = CFString::new(peer_id);
    let items = CFArray::from_CFTypes(&urls);
    let mut ctx = Box::new(ClientContext::new(std::ptr::null_mut()));
    let (node, op) = unsafe {
        let empty = CFString::new("");
        let node = (s.node_create)(
            std::ptr::null(),
            empty.as_concrete_TypeRef(),
            empty.as_concrete_TypeRef(),
        );
        (s.node_set_real_name)(node, id.as_concrete_TypeRef());
        (s.node_set_display_name)(node, id.as_concrete_TypeRef());
        (s.node_set_service_name)(node, id.as_concrete_TypeRef());
        (s.node_set_domain)(node, CFString::new("local.").as_concrete_TypeRef());
        (s.node_add_bonjour_protocol)(node, CFString::new("_airdrop._tcp.").as_concrete_TypeRef());
        let kinds = CFArray::from_CFTypes(&[CFString::wrap_under_get_rule(s.k_node_kind_airdrop)]);
        (s.node_set_kinds)(node, kinds.as_CFTypeRef());

        let op = (s.operation_create)(
            std::ptr::null(),
            s.k_operation_kind_sender,
            std::ptr::null(),
            std::ptr::null(),
        );
        (s.operation_set_property)(op, s.k_operation_items, items.as_CFTypeRef());
        (s.operation_set_property)(op, s.k_operation_node, node as _);
        (s.operation_set_dispatch_queue)(op, main_queue());
        (s.operation_set_client)(op, on_event, &mut *ctx);
        (s.operation_resume)(op);
        (node, op)
    };

    let deadline = Instant::now() + timeout;
    'run: loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        unsafe { CFRunLoopRunInMode(kCFRunLoopDefaultMode, left.as_secs_f64(), 0) };
        let events = QUEUE.with_borrow_mut(std::mem::take);
        for (from, ev) in events {
            if from != op {
                continue;
            }
            if let Some(name) = ev.data.get("ReceiverComputerName").and_then(Value::as_str) {
                report.receiver_name = Some(name.to_owned());
            }
            if let Some(model) = ev.data.get("ReceiverModelName").and_then(Value::as_str) {
                report.receiver_model = Some(model.to_owned());
            }
            on_event_cb(&ev);
            if !is_terminal(ev.code) {
                report.stage = Some(ev.event);
            } else {
                report.outcome = match ev.code {
                    event::FINISHED => Outcome::Finished,
                    event::CANCELED => Outcome::Canceled,
                    _ => Outcome::Failed,
                };
                if ev.code == event::ERROR {
                    report.error = Some(ev.data);
                }
                break 'run;
            }
        }
    }

    unsafe {
        if report.outcome == Outcome::TimedOut {
            (s.operation_cancel)(op);
            // Let the cancel reach sharingd so the receiver's prompt is withdrawn.
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 1.0, 0);
        }
        QUEUE.with_borrow_mut(|q| q.retain(|(from, _)| *from != op));
        CFRelease(op as _);
        CFRelease(node as _);
    }
    // The client context must outlive the operation's last callback.
    std::mem::forget(ctx);
    Ok(report)
}

/// A one-line message for an error event payload.
pub fn describe(v: &Value) -> String {
    match v {
        Value::Null => "unknown error".into(),
        Value::Object(m) => m
            .values()
            .find_map(|e| e.get("description").and_then(Value::as_str))
            .map(str::to_owned)
            .unwrap_or_else(|| v.to_string()),
        _ => v.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::Item;

    #[test]
    fn items() {
        assert!(matches!(
            Item::parse("https://example.com"),
            Ok(Item::Url(_))
        ));
        assert!(matches!(Item::parse("mailto:a@b.c"), Ok(Item::Url(_))));
        assert!(matches!(Item::parse("Cargo.toml"), Ok(Item::Path(_))));
        assert!(Item::parse("no-such-file.txt").is_err());
        assert!(Item::parse("1:2").is_err());
    }
}
