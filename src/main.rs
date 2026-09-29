mod ble;
mod caps;
mod cf;
mod discovery;
mod known;
mod mcp;
mod presence;
mod probe;
mod send;
mod sharing;

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use send::{Event, Item, Outcome, Report};

/// Devices in the same room measured -28 to -52 dBm and a device out of AirDrop range -71 to
/// -82 dBm, so the default sits in the gap.
const DEFAULT_MIN_RSSI: i32 = -65;

const ABOUT: &str = "Send files and links over AirDrop from the terminal, with no UI.";

const LONG_ABOUT: &str = "\
Send files and links over AirDrop from the terminal, with no UI.

Uses the private Sharing.framework API that Finder's AirDrop window uses, so macOS's
own sharingd does discovery, encryption and the transfer.

Typical use:
  airdrop list                          # find receivers and their IDs
  airdrop send 571707478742 ./photo.jpg https://example.com

About receivers:
  - Receivers are addressed by opaque Bonjour IDs. `list` shows each one's device name
    by asking it the way Finder does (AirDrop /Discover); `send` takes the ID.
  - IDs can change when the receiver restarts AirDrop. List again if `send` stalls in
    `connecting`.
  - The receiver must be awake and nearby, with AirDrop set to Everyone (or Contacts
    Only, with you in their contacts).
  - Devices on the same Apple ID accept automatically. Anyone else gets an Accept/Decline
    prompt, and `send` waits for the answer.";

const EXIT_CODES: &str = "\
Exit codes:
  0  success (for send: the receiver accepted and the transfer finished)
  1  error (bad input, framework unavailable, or sharingd reported a failure)
  2  invalid command-line usage
  3  send was declined or canceled
  4  send timed out (the operation was canceled)";

#[derive(Parser)]
#[command(name = "airdrop", version, about = ABOUT, long_about = LONG_ABOUT, after_help = EXIT_CODES)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List nearby AirDrop receivers.
    #[command(long_about = "\
List nearby AirDrop receivers.

Browses Bonjour for `--wait` seconds, then asks each receiver about itself the way Finder
does (AirDrop /Discover). This Mac is left out. For each receiver it prints:

  name, model   device name (and model, when the receiver sends one). Receivers set to
                Contacts Only answer but withhold their name and TXT flags.
  mode          everyone, or contacts_only. A Contacts Only receiver only shows up at
                all if one of your identifiers is in its contacts (matched on a 2-byte
                hash, so about 1 in 20 such matches can be a false positive for a large
                address book); ones that do not have you stay invisible.
  airdrop       the receiver's IsAirDropable reply (true even for Contacts Only
                receivers that may decline you)
  id            what `send` takes
  network       interfaces (awdl0 = peer-to-peer Wi-Fi, en0 = shared network), the
                address that answered and its response time
  features      decoded TXT flags (links, archives, mixed item types, ...)
  video, hdr, images, photos
                media the receiver can handle (HEVC, ProRes, Dolby Vision, HEIC, ...)

After the receivers it lists \"hidden\" devices: ones whose Bluetooth LE advertisements
(Continuity Nearby Info) say AirDrop receiving is on. That bit only means AirDrop is not
Receiving Off; it is the same for Everyone and Contacts Only. Each line shows signal
strength, the device's Bluetooth identifier and a guess at its kind. They cannot be
matched to the receivers above, so those show up here too. When more nearby devices
(signal at least --min-rssi, default -65 dBm) have AirDrop on than there are receivers
above, the difference is reported as likely hiding from you (Contacts Only without you
as a contact); weaker ones are reported as likely out of AirDrop range. A device whose AirDrop is on but
screen is off may not be reachable. The first run asks for Bluetooth access for your
terminal app.

--debug adds two raw views:
  awdl          Apple devices answering an IPv6 ping on AWDL that are not receivers
                above. Anonymous (random link-local addresses), and AWDL is also used
                by AirPlay, Sidecar and Universal Control.
  bluetooth     every Apple device heard, with each Continuity message decoded where
                the format is known (activity, AirDrop receiving and Wi-Fi state;
                AirPods model and battery; Find My status) and its raw bytes.
The decoding comes from reverse engineering done on iOS 13 and can be wrong on newer
systems.

Known names: every name a receiver reports is remembered in ~/.airdrop/known.json with
its ID, host and link-local addresses. A receiver gets a new ID and host when it switches
AirDrop mode, but its AWDL address stayed the same across switches in testing, so when it
later answers in Contacts Only mode `list` shows the remembered name, marked remembered,
and a `known` line saying when it was named and which identifier matched. Name one by
hand with `airdrop name`, drop one with `airdrop forget`. Addresses can rotate (for
example after a restart), so a remembered name is a strong hint, not proof.

A receiver's ID and host change when it switches AirDrop mode. An empty list
means nobody is discoverable: the receiver's screen may be off, or its AirDrop set to
Receiving Off.

JSON output (--json): {\"peers\": [{\"id\", \"name\", \"model\", \"airdropable\", \"mode\",
\"network\": {\"interfaces\", \"addresses\", \"host\", \"port\", \"responded_via\",
\"response_ms\"}, \"features\": {\"flags\", \"hex\", \"known\", \"unknown_bits\"},
\"media\": {\"video_codecs\", \"hdr\", \"dolby_vision\", \"image_formats\",
\"live_photo_version\", \"asset_bundle_version\"}, \"discover_error\", \"known\": {\"name\",
\"model\", \"matched_by\", \"named_at\"},
\"raw\": {\"txt\", \"discover\"}}], \"likely_hiding\", \"min_rssi\", \"hidden\": [{\"id\", \"name\", \"rssi\", \"kind\",
\"airdrop\", \"messages\": [{\"type\", \"name\", \"details\", \"unverified\", \"hex\"}]}], \"hidden_error\",
\"awdl\": [{\"address\"}], \"awdl_error\", \"ble\": [same as hidden]}; awdl, awdl_error and
ble only with --debug; \"store_error\" when ~/.airdrop/known.json could not be updated.")]
    List {
        /// Seconds to browse for.
        #[arg(short, long, default_value_t = 8.0, value_name = "SECS")]
        wait: f64,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Also list every Apple device found over AWDL and Bluetooth LE.
        #[arg(long)]
        debug: bool,
        /// Hidden devices weaker than this (dBm) count as out of AirDrop range.
        #[arg(long, default_value_t = DEFAULT_MIN_RSSI, value_name = "DBM", allow_negative_numbers = true)]
        min_rssi: i32,
    },
    /// Send files, folders or links to a receiver.
    #[command(long_about = "\
Send files, folders or links to a receiver.

Each ITEM is a path to an existing file or folder, or a URL with a scheme (https://…,
mailto:…). Anything else is rejected. All items go in one transfer.

Blocks until the transfer finishes, is declined, fails, or `--timeout` passes (then the
operation is canceled and the receiver's prompt withdrawn). Progress goes to stderr, the
result to stdout. `stage` says how far it got: a timeout or failure in `connecting` means the
receiver is not reachable (list again), in `waiting_for_answer` that nobody answered the
prompt. sharingd itself gives up on an unanswered prompt after about 2 minutes and reports
that as a failure.

JSON output (--json): {\"outcome\": \"finished\"|\"canceled\"|\"failed\"|\"timed_out\",
\"peer_id\", \"receiver_name\", \"receiver_model\", \"items\", \"stage\", \"error\"}",
        after_help = EXIT_CODES)]
    Send {
        /// Receiver ID from `airdrop list`.
        peer_id: String,
        /// Files, folders or URLs to send.
        #[arg(required = true, value_name = "ITEM")]
        items: Vec<String>,
        /// Seconds to wait before canceling.
        #[arg(short, long, default_value_t = 150.0, value_name = "SECS")]
        timeout: f64,
        /// Print the result as JSON.
        #[arg(long)]
        json: bool,
        /// Also print every raw sharingd event to stderr as a JSON line.
        #[arg(short, long)]
        verbose: bool,
    },
    /// Remember a name for a receiver, shown when it withholds its own.
    #[command(long_about = "\
Remember a name for a receiver, shown by `list` when it withholds its own.

`list` already remembers the name of every receiver set to Everyone in
~/.airdrop/known.json, and shows it (marked remembered) when the same device later
answers in Contacts Only mode. Use this for a receiver that was never seen in Everyone
mode. ID is its current ID from `list`; `list` then learns its other identifiers.")]
    Name {
        /// Receiver ID from `airdrop list`.
        id: String,
        /// Name to show for it.
        name: String,
    },
    /// Forget a remembered receiver name.
    Forget {
        /// The remembered name, or one of the receiver's IDs.
        #[arg(value_name = "NAME|ID")]
        key: String,
    },
    /// Run an MCP server on stdio, exposing `list_peers` and `send` tools.
    #[command(long_about = "\
Run a Model Context Protocol server on stdio, exposing `list_peers` and `send` tools
with the same behaviour as the commands. For example, register it with Claude Code:

  claude mcp add airdrop -- airdrop mcp")]
    Mcp,
}

fn duration(secs: f64) -> Result<Duration, String> {
    Duration::try_from_secs_f64(secs)
        .ok()
        .filter(|d| !d.is_zero())
        .ok_or_else(|| format!("invalid duration {secs}"))
}

/// Prints one block per receiver: a heading, then labelled detail lines.
fn print_peer(p: &discovery::Peer) {
    let name = match (&p.name, &p.known, &p.raw.discover) {
        (Some(n), ..) => n.clone(),
        (None, Some(k), Some(_)) => format!(
            "{} (remembered; Contacts Only, likely has you as a contact)",
            k.name
        ),
        (None, Some(k), None) => format!("{} (remembered; no reply)", k.name),
        // Contacts Only receivers answer but withhold their name.
        (None, None, Some(_)) => "(name hidden: Contacts Only, likely has you as a contact)".into(),
        (None, None, None) => "(no reply)".into(),
    };
    match p
        .model
        .as_ref()
        .or(p.known.as_ref().and_then(|k| k.model.as_ref()))
    {
        Some(m) => println!("{name} ({m})"),
        None => println!("{name}"),
    }
    let line = |label: &str, value: &str| println!("  {label:<9} {value}");
    line("id", &p.id);
    if let Some(k) = &p.known {
        let by = match k.matched_by {
            "address" => "link-local address",
            other => other,
        };
        line(
            "known",
            &format!(
                "named {}, matched by {by}",
                ago(known::now().saturating_sub(k.named_at))
            ),
        );
    }
    if let Some(a) = p.airdropable {
        line("airdrop", if a { "available" } else { "unavailable" });
    }
    let net = &p.network;
    let mut via = net.interfaces.join(",");
    if let Some(addr) = net.responded_via.as_ref().or(net.addresses.first()) {
        via += &format!("  {addr}");
    }
    if let Some(ms) = net.response_ms {
        via += &format!("  ({ms} ms)");
    }
    line("network", &via);
    let others: Vec<&String> = net
        .addresses
        .iter()
        .filter(|a| Some(*a) != net.responded_via.as_ref())
        .collect();
    if !others.is_empty() && net.responded_via.is_some() {
        line(
            "",
            &format!(
                "also {}",
                others
                    .iter()
                    .map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    }
    if let (Some(h), Some(port)) = (&net.host, net.port) {
        line("host", &format!("{h}:{port}"));
    }
    if let Some(f) = &p.features {
        let mut v = format!("{}  {}", f.hex, f.describe().join(", "));
        if let Some(u) = &f.unknown_bits {
            v += &format!("  (+unknown bits {u})");
        }
        line("features", &v);
    }
    if let Some(m) = &p.media {
        for (label, value) in m.describe() {
            line(label, &value);
        }
    }
    if let Some(e) = &p.discover_error {
        line("error", &format!("/Discover failed: {e}"));
    }
}

/// Prints a device heading and, with `messages`, one line per Continuity message.
fn print_ble_device(d: &ble::BleDevice, messages: bool) {
    let mut head = format!("  {:>4} dBm  {}", d.rssi, d.id);
    if let Some(k) = d.kind {
        head += &format!("  {k}");
    }
    if let Some(n) = &d.name {
        head += &format!("  \"{n}\"");
    }
    println!("{head}");
    if !messages {
        return;
    }
    for m in &d.messages {
        let name = m
            .name
            .map_or_else(|| format!("type {:#04x}", m.kind), str::to_owned);
        let mut line = format!("    {name:<18} ");
        if !m.details.is_empty() {
            line += &format!("{}  ", m.details.join(", "));
        }
        if !m.unverified.is_empty() {
            line += &format!("(unverified: {})  ", m.unverified.join(", "));
        }
        line += &format!("[{}]", m.hex);
        println!("{line}");
    }
}

/// `secs` as a rough age, e.g. `3 hours ago`.
fn ago(secs: u64) -> String {
    let (n, unit) = match secs {
        0..60 => return "just now".into(),
        60..3600 => (secs / 60, "minute"),
        3600..86_400 => (secs / 3600, "hour"),
        _ => (secs / 86_400, "day"),
    };
    format!("{} ago", plural(n as usize, unit))
}

fn plural(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

fn list(wait: f64, as_json: bool, debug: bool, min_rssi: i32) -> Result<ExitCode, String> {
    let scan = discovery::discover(duration(wait)?, debug, min_rssi)?;
    if as_json {
        println!("{}", json!(scan));
        return Ok(ExitCode::SUCCESS);
    }
    // Blank line between blocks.
    let mut printed = false;
    let mut gap = || {
        if std::mem::replace(&mut printed, true) {
            println!();
        }
    };
    for p in &scan.peers {
        gap();
        print_peer(p);
    }
    if !scan.hidden.is_empty() {
        gap();
        println!(
            "hidden: {} with AirDrop on (Bluetooth; Everyone or Contacts Only), may include the receivers above",
            plural(scan.hidden.len(), "device")
        );
        for d in &scan.hidden {
            print_ble_device(d, false);
        }
        let near = scan
            .hidden
            .iter()
            .filter(|d| d.rssi >= scan.min_rssi)
            .count();
        let far = scan.hidden.len() - near;
        let mut summary = if scan.likely_hiding > 0 {
            let n = scan.likely_hiding;
            let devices = if n == 1 { "device is" } else { "devices are" };
            format!(
                "  so at least {n} {devices} likely hiding from you (Contacts Only without you as a contact): {near} nearby with AirDrop on, {} visible above",
                plural(scan.peers.len(), "receiver")
            )
        } else {
            format!(
                "  none hiding from you: {near} nearby with AirDrop on, {} visible above",
                plural(scan.peers.len(), "receiver")
            )
        };
        if far > 0 {
            summary += &format!(
                "; {far} more below {} dBm, likely out of AirDrop range",
                scan.min_rssi
            );
        }
        println!("{summary}");
    }
    let awdl = scan.awdl.as_deref().unwrap_or_default();
    if !awdl.is_empty() {
        gap();
        println!(
            "awdl: {} answering on AWDL, not offering AirDrop to you",
            plural(awdl.len(), "more Apple device")
        );
        for h in awdl {
            println!("  {}", h.address);
        }
    }
    let ble = scan.ble.as_deref().unwrap_or_default();
    if !ble.is_empty() {
        gap();
        println!(
            "bluetooth: {} advertising",
            plural(ble.len(), "Apple device")
        );
        for d in ble {
            print_ble_device(d, true);
        }
    }
    if scan.peers.is_empty() && scan.hidden.is_empty() {
        eprintln!(
            "no AirDrop receivers found in {wait}s (is the receiver awake, nearby, and set to Everyone?)"
        );
    }
    if let Some(e) = &scan.hidden_error {
        eprintln!("note: could not scan Bluetooth for hidden devices: {e}");
    }
    if let Some(e) = &scan.awdl_error {
        eprintln!("note: could not ping AWDL: {e}");
    }
    if let Some(e) = &scan.store_error {
        eprintln!("note: could not update known names: {e}");
    }
    Ok(ExitCode::SUCCESS)
}

fn name_cmd(id: &str, name: &str) -> Result<ExitCode, String> {
    let mut store = known::Store::load()?;
    let sighting = known::Sighting {
        id: id.to_owned(),
        ..Default::default()
    };
    store.named(&sighting, name, None, known::now());
    store.save()?;
    println!("{id} is now known as {name}");
    Ok(ExitCode::SUCCESS)
}

fn forget_cmd(key: &str) -> Result<ExitCode, String> {
    let mut store = known::Store::load()?;
    match store.forget(key) {
        0 => Err(format!("no known receiver is called or has the ID {key}")),
        n => {
            store.save()?;
            println!("forgot {}", plural(n, "receiver"));
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64 / 1000.0;
    let mut unit = 0;
    while v >= 1000.0 && unit < UNITS.len() - 1 {
        v /= 1000.0;
        unit += 1;
    }
    format!("{v:.1} {}", UNITS[unit])
}

/// Prints one human progress line per meaningful event.
fn progress_line(ev: &Event, last_pct: &mut Option<u64>) -> Option<String> {
    match ev.event {
        "connecting" => Some("connecting…".into()),
        "waiting_for_answer" => Some("waiting for the receiver to accept…".into()),
        "started" => Some("transferring…".into()),
        "progress" => {
            let p = ev.progress()?;
            let pct = p.bytes * 100 / p.total / 10 * 10;
            if *last_pct == Some(pct) {
                return None;
            }
            *last_pct = Some(pct);
            let left = match p.secs_left {
                Some(s) if pct < 100 => format!(", {s}s left"),
                _ => String::new(),
            };
            Some(format!(
                "{pct:>3}%  {} of {}{left}",
                human_bytes(p.bytes),
                human_bytes(p.total)
            ))
        }
        _ => None,
    }
}

fn print_report(r: &Report) {
    let who = match (&r.receiver_name, &r.receiver_model) {
        (Some(n), Some(m)) => format!("{n} ({m})"),
        (Some(n), None) => n.clone(),
        _ => r.peer_id.clone(),
    };
    let n = r.items.len();
    let items = if n == 1 {
        "1 item".into()
    } else {
        format!("{n} items")
    };
    let stage = r.stage.unwrap_or("start");
    let hint = match r.stage {
        None | Some("connecting") => {
            "receiver not reachable; run `airdrop list` to check it is still there"
        }
        Some("waiting_for_answer") => "nobody accepted the prompt in time",
        Some(_) => "the transfer was interrupted",
    };
    match r.outcome {
        Outcome::Finished => println!("sent {items} to {who}"),
        Outcome::Canceled => println!("{who} declined or canceled the transfer"),
        Outcome::Failed => println!(
            "sending to {who} failed at {stage}: {} ({hint})",
            send::describe(r.error.as_ref().unwrap_or(&Value::Null))
        ),
        Outcome::TimedOut => println!("timed out sending to {who} at {stage} ({hint})"),
    }
}

fn send_cmd(
    peer_id: &str,
    items: &[String],
    timeout: f64,
    as_json: bool,
    verbose: bool,
) -> Result<ExitCode, String> {
    let items = items
        .iter()
        .map(|s| Item::parse(s))
        .collect::<Result<Vec<_>, _>>()?;
    let mut last_pct = None;
    let report = send::send(peer_id, &items, duration(timeout)?, |ev| {
        if verbose {
            eprintln!("{}", json!(ev));
        }
        if let Some(line) = progress_line(ev, &mut last_pct) {
            eprintln!("{line}");
        }
    })?;
    if as_json {
        println!("{}", json!(report));
    } else {
        print_report(&report);
    }
    Ok(ExitCode::from(match report.outcome {
        Outcome::Finished => 0,
        Outcome::Failed => 1,
        Outcome::Canceled => 3,
        Outcome::TimedOut => 4,
    }))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::List {
            wait,
            json,
            debug,
            min_rssi,
        } => list(wait, json, debug, min_rssi),
        Command::Send {
            peer_id,
            items,
            timeout,
            json,
            verbose,
        } => send_cmd(&peer_id, &items, timeout, json, verbose),
        Command::Name { id, name } => name_cmd(&id, &name),
        Command::Forget { key } => forget_cmd(&key),
        Command::Mcp => mcp::serve()
            .map(|()| ExitCode::SUCCESS)
            .map_err(|e| format!("MCP server I/O error: {e}")),
    };
    result.unwrap_or_else(|e| {
        eprintln!("error: {e}");
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn bytes() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1_500), "1.5 KB");
        assert_eq!(human_bytes(302_083_218), "302.1 MB");
        assert_eq!(human_bytes(4_500_000_000), "4.5 GB");
    }
}
