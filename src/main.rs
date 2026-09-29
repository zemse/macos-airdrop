mod caps;
mod cf;
mod discovery;
mod mcp;
mod probe;
mod send;
mod sharing;

use std::process::ExitCode;
use std::time::Duration;

use clap::{Parser, Subcommand};
use serde_json::{Value, json};

use send::{Event, Item, Outcome, Report};

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
  airdrop       the receiver's IsAirDropable reply (true even for Contacts Only
                receivers that may decline you)
  id            what `send` takes
  network       interfaces (awdl0 = peer-to-peer Wi-Fi, en0 = shared network), the
                address that answered and its response time
  features      decoded TXT flags (links, archives, mixed item types, ...)
  video, hdr, images, photos
                media the receiver can handle (HEVC, ProRes, Dolby Vision, HEIC, ...)

A receiver's ID and host change when it switches AirDrop mode. An empty list
means nobody is discoverable: the receiver's screen may be off, or its AirDrop set to
Receiving Off.

JSON output (--json): {\"peers\": [{\"id\", \"name\", \"model\", \"airdropable\",
\"network\": {\"interfaces\", \"addresses\", \"host\", \"port\", \"responded_via\",
\"response_ms\"}, \"features\": {\"flags\", \"hex\", \"known\", \"unknown_bits\"},
\"media\": {\"video_codecs\", \"hdr\", \"dolby_vision\", \"image_formats\",
\"live_photo_version\", \"asset_bundle_version\"}, \"discover_error\",
\"raw\": {\"txt\", \"discover\"}}]}")]
    List {
        /// Seconds to browse for.
        #[arg(short, long, default_value_t = 5.0, value_name = "SECS")]
        wait: f64,
        /// Print JSON instead of a table.
        #[arg(long)]
        json: bool,
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
    let name = match (&p.name, &p.raw.discover) {
        (Some(n), _) => n.as_str(),
        // Contacts Only receivers answer but withhold their name.
        (None, Some(_)) => "(name hidden: Contacts Only)",
        (None, None) => "(no reply)",
    };
    match &p.model {
        Some(m) => println!("{name} ({m})"),
        None => println!("{name}"),
    }
    let line = |label: &str, value: &str| println!("  {label:<9} {value}");
    line("id", &p.id);
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

fn list(wait: f64, as_json: bool) -> Result<ExitCode, String> {
    let peers = discovery::discover(duration(wait)?)?;
    if as_json {
        println!("{}", json!({ "peers": peers }));
    } else if peers.is_empty() {
        eprintln!(
            "no AirDrop receivers found in {wait}s (is the receiver awake, nearby, and set to Everyone?)"
        );
    } else {
        for (i, p) in peers.iter().enumerate() {
            if i > 0 {
                println!();
            }
            print_peer(p);
        }
    }
    Ok(ExitCode::SUCCESS)
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
        Command::List { wait, json } => list(wait, json),
        Command::Send {
            peer_id,
            items,
            timeout,
            json,
            verbose,
        } => send_cmd(&peer_id, &items, timeout, json, verbose),
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
