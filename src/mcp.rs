//! A Model Context Protocol server over stdio (newline-delimited JSON-RPC 2.0).
//!
//! Requests are handled one at a time on the main thread, because every AirDrop call runs the
//! main run loop until it is done.

use std::io::{BufRead, Write};
use std::time::Duration;

use serde_json::{Value, json};

use crate::discovery;
use crate::send::{self, Item, Outcome};

const PROTOCOL_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "\
Send files and links to nearby Apple devices over AirDrop, from this Mac, with no UI.

Workflow: call list_peers, then send with a peer_id from its result.
- Peer IDs are opaque Bonjour instance names (e.g. \"571707478742\"); list_peers also \
returns each receiver's device name, so match on name and send by id. Receivers set to \
Contacts Only withhold their name (mode = contacts_only) and only appear at all if they \
likely have this Mac's owner as a contact; their ID changes when they switch AirDrop mode.
- IDs can change when the receiver restarts AirDrop, so list again if send times out in \
\"connecting\".
- The receiver must be awake and nearby, with AirDrop set to \"Everyone\" (or \"Contacts \
Only\" with this Mac's Apple ID in their contacts).
- Devices signed into the same Apple ID accept automatically. Anyone else sees an \
Accept/Decline prompt, and send blocks until they answer or timeout_secs passes.";

fn tools() -> Value {
    json!([
        {
            "name": "list_peers",
            "title": "List AirDrop receivers",
            "description": "Browse for nearby AirDrop receivers for wait_secs seconds, then ask \
                each one about itself (AirDrop /Discover). Each peer has: id (pass it to send), \
                name, model (when sent), airdropable (its IsAirDropable reply; true even in \
                Contacts Only, so not a promise it will accept), network \
                (interfaces: awdl0 = peer-to-peer Wi-Fi, en0 = shared network; addresses; \
                response_ms), features (decoded capability flags), media (video codecs, HDR, \
                Dolby Vision, image formats) and raw (TXT record and full /Discover reply). \
                mode is everyone or contacts_only; a contacts_only receiver answers without \
                a name or features and is only visible if it likely has you as a contact. \
                discover_error is set when the receiver did not answer. \
                hidden lists devices whose Bluetooth \
                advertisements say AirDrop receiving is on (Everyone or Contacts Only; the \
                bit only distinguishes Receiving Off); they cannot be matched to peers, so \
                peers show up there too. likely_hiding = hidden devices at or above min_rssi \
                minus peers count: at least that many are likely Contacts Only without you as \
                a contact; weaker ones are likely just out of AirDrop range. \
                Message fields under unverified come from old research and may be wrong. With debug, awdl lists anonymous Apple devices answering on AWDL \
                and ble every Apple device heard over Bluetooth LE with decoded Continuity \
                messages. This Mac is excluded. Empty peers means nobody is \
                discoverable to you: the \
                receiver's screen may be off, or AirDrop set to Receiving Off.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "wait_secs": {
                        "type": "number",
                        "description": "How long to browse. Default 8.",
                        "minimum": 1,
                        "maximum": 60
                    },
                    "min_rssi": {
                        "type": "integer",
                        "description": "Hidden devices weaker than this (dBm) count as out of AirDrop range and are left out of likely_hiding. Default -65.",
                        "minimum": -100,
                        "maximum": -20
                    },
                    "debug": {
                        "type": "boolean",
                        "description": "Also return every Apple device found over AWDL and Bluetooth LE. Default false."
                    }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": true }
        },
        {
            "name": "send",
            "title": "AirDrop files or links",
            "description": "AirDrop one or more items to a receiver found by list_peers. Items \
                are absolute paths to files or folders on this Mac, or URLs with a scheme \
                (https://..., mailto:...). Blocks until the transfer ends. Result outcome: \
                finished (delivered), canceled (declined or canceled), failed (see error), or \
                timed_out (canceled after timeout_secs). stage says how far it got: failing or \
                timing out in connecting = receiver not reachable (list_peers again), in \
                waiting_for_answer = nobody answered the prompt (sharingd gives up after about \
                2 minutes and reports failed).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "peer_id": {
                        "type": "string",
                        "description": "A peer id from list_peers."
                    },
                    "items": {
                        "type": "array",
                        "items": { "type": "string" },
                        "minItems": 1,
                        "description": "Absolute file/folder paths or URLs."
                    },
                    "timeout_secs": {
                        "type": "number",
                        "description": "Give up and cancel after this long. Default 150.",
                        "minimum": 5,
                        "maximum": 900
                    }
                },
                "required": ["peer_id", "items"],
                "additionalProperties": false
            },
            "annotations": {
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true
            }
        }
    ])
}

fn secs(args: &Value, key: &str, default: f64) -> Duration {
    let v = args.get(key).and_then(Value::as_f64).unwrap_or(default);
    Duration::from_secs_f64(v.clamp(1.0, 900.0))
}

fn tool_result(value: Value, is_error: bool) -> Value {
    json!({
        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&value).unwrap_or_default() }],
        "structuredContent": value,
        "isError": is_error,
    })
}

fn tool_error(msg: String) -> Value {
    json!({ "content": [{ "type": "text", "text": msg }], "isError": true })
}

/// Handles `tools/call`. `notify` sends a JSON-RPC notification to the client.
fn call_tool(params: &Value, notify: &mut dyn FnMut(Value)) -> Result<Value, (i64, String)> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "list_peers" => Ok(
            match discovery::discover(
                secs(&args, "wait_secs", 8.0),
                args.get("debug").and_then(Value::as_bool).unwrap_or(false),
                args.get("min_rssi")
                    .and_then(Value::as_i64)
                    .map_or(-65, |v| v.clamp(-100, -20) as i32),
            ) {
                Ok(scan) => tool_result(json!(scan), false),
                Err(e) => tool_error(e),
            },
        ),
        "send" => {
            let Some(peer_id) = args.get("peer_id").and_then(Value::as_str) else {
                return Ok(tool_error("missing peer_id".into()));
            };
            let raw: Vec<&str> = match args.get("items").and_then(Value::as_array) {
                Some(a) => a.iter().filter_map(Value::as_str).collect(),
                None => return Ok(tool_error("missing items".into())),
            };
            let items = match raw
                .iter()
                .map(|s| Item::parse(s))
                .collect::<Result<Vec<_>, _>>()
            {
                Ok(items) => items,
                Err(e) => return Ok(tool_error(e)),
            };
            let token = params
                .get("_meta")
                .and_then(|m| m.get("progressToken"))
                .cloned();
            let mut sent = None;
            let timeout = secs(&args, "timeout_secs", 150.0);
            let result = send::send(peer_id, &items, timeout, |ev| {
                // Progress must strictly increase, so only byte counts are reported.
                let (Some(token), Some(p)) = (&token, ev.progress()) else {
                    return;
                };
                if sent.is_some_and(|s| p.bytes <= s) {
                    return;
                }
                sent = Some(p.bytes);
                notify(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/progress",
                    "params": {
                        "progressToken": token,
                        "progress": p.bytes,
                        "total": p.total,
                        "message": format!("{} of {} bytes sent", p.bytes, p.total),
                    },
                }));
            });
            Ok(match result {
                Ok(report) => {
                    let failed = report.outcome != Outcome::Finished;
                    tool_result(serde_json::to_value(&report).unwrap_or_default(), failed)
                }
                Err(e) => tool_error(e),
            })
        }
        _ => Err((-32602, format!("unknown tool {name:?}"))),
    }
}

fn handle(msg: &Value, notify: &mut dyn FnMut(Value)) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str);
    let Some(method) = method else {
        // A response to a request we never make, or garbage.
        return id.map(|id| error(id, -32600, "invalid request".into()));
    };
    // Notifications (no id) never get a reply.
    let id = id?;
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str);
            let version = asked
                .filter(|v| PROTOCOL_VERSIONS.contains(v))
                .unwrap_or(PROTOCOL_VERSIONS[0]);
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": {
                    "name": "airdrop",
                    "title": "AirDrop",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "instructions": INSTRUCTIONS,
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(&params, notify),
        _ => Err((-32601, format!("method not found: {method}"))),
    };
    Some(match result {
        Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
        Err((code, m)) => error(id, code, m),
    })
}

fn error(id: Value, code: i64, message: String) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

fn write_line(out: &mut impl Write, v: &Value) -> std::io::Result<()> {
    serde_json::to_writer(&mut *out, v)?;
    out.write_all(b"\n")?;
    out.flush()
}

/// Serves until stdin closes.
pub fn serve() -> std::io::Result<()> {
    let stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(&msg, &mut |n| {
                // Progress is best effort; a broken pipe surfaces on the reply below.
                let _ = write_line(&mut stdout, &n);
            }),
            Err(e) => Some(error(Value::Null, -32700, format!("parse error: {e}"))),
        };
        if let Some(reply) = reply {
            write_line(&mut stdout, &reply)?;
        }
    }
    Ok(())
}
