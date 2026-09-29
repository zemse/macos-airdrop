mod cf;
mod discovery;
mod mcp;
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
  - Receivers are listed by opaque Bonjour IDs. Device names are not available before
    sending; `send` reports the receiver's name and model once it has accepted.
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

Browses for `--wait` seconds and prints each receiver's ID (pass it to `send`), the
interfaces it was seen on (awdl0 = peer-to-peer Wi-Fi, en0 = shared network) and its
raw TXT `flags` capability value. An empty list means nobody is discoverable: the
receiver's screen may be off, or its AirDrop set to Receiving Off.

JSON output (--json): {\"peers\": [{\"id\", \"interfaces\", \"host\", \"port\", \"flags\", \"txt\"}]}")]
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
        /// Print every raw sharingd event to stderr as a JSON line.
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

fn list(wait: f64, as_json: bool) -> Result<ExitCode, String> {
    let peers = discovery::discover(duration(wait)?)?;
    if as_json {
        println!("{}", json!({ "peers": peers }));
    } else if peers.is_empty() {
        eprintln!(
            "no AirDrop receivers found in {wait}s (is the receiver awake, nearby, and set to Everyone?)"
        );
    } else {
        println!("{:<16} {:<12} {:<8} HOST", "ID", "INTERFACES", "FLAGS");
        for p in &peers {
            let host = match (&p.host, p.port) {
                (Some(h), Some(port)) => format!("{h}:{port}"),
                _ => "-".into(),
            };
            let flags = p.flags.map_or("-".into(), |f| f.to_string());
            println!(
                "{:<16} {:<12} {:<8} {host}",
                p.id,
                p.interfaces.join(","),
                flags
            );
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Prints one human progress line per meaningful event.
fn progress_line(ev: &Event, last_pct: &mut Option<u64>) -> Option<String> {
    let num = |k: &str| {
        ev.data
            .as_object()?
            .iter()
            .find(|(key, _)| key.contains(k))
            .and_then(|(_, v)| v.as_f64())
    };
    match ev.event {
        "connecting" => Some("connecting…".into()),
        "waiting_for_answer" => Some("waiting for the receiver to accept…".into()),
        "started" => Some("transferring…".into()),
        "progress" => {
            let (done, total) = (num("BytesCopied")?, num("TotalBytes")?);
            if total <= 0.0 {
                return None;
            }
            let pct = (done / total * 100.0) as u64 / 10 * 10;
            (*last_pct != Some(pct)).then(|| {
                *last_pct = Some(pct);
                format!("{pct}%")
            })
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
        } else if let Some(line) = progress_line(ev, &mut last_pct) {
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
