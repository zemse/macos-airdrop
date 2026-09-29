# macos-airdrop

[![crates.io](https://img.shields.io/crates/v/macos-airdrop.svg)](https://crates.io/crates/macos-airdrop)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

AirDrop **files, folders and links** from the macOS terminal, with **no UI**, and an
**MCP server** so agents can do the same.

It drives the private `Sharing.framework` API that Finder's AirDrop window uses, so
macOS's own `sharingd` does the discovery, encryption and transfer. Nothing is
reimplemented, and no special entitlements or root are needed.

The crate is published as **`macos-airdrop`**; it installs a binary named **`airdrop`**.

## Install

```sh
cargo install macos-airdrop   # or: cargo install --path .
```

## Usage

```sh
# Find nearby receivers (browses for 5s by default)
airdrop list
# ID               INTERFACES   FLAGS    HOST
# 571707478742     awdl0        111611   63649ac8-….local:8770

# Send files, folders and links in one transfer
airdrop send 571707478742 ./photo.jpg ./notes/ https://example.com
# connecting…
# waiting for the receiver to accept…
# transferring…
# sent 3 items to Office-Mac (MacBook Air)

# Machine-readable output
airdrop list --json
airdrop send 571707478742 report.pdf --json --timeout 60
```

`airdrop --help` and `airdrop <command> --help` describe every field and outcome.

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | Success (`send`: accepted and finished) |
| 1 | Error: bad input, framework unavailable, or `sharingd` reported a failure |
| 2 | Invalid command-line usage |
| 3 | `send` was declined or canceled |
| 4 | `send` timed out; the operation was canceled |

## MCP server

`airdrop mcp` serves the Model Context Protocol over stdio with two tools:

- **`list_peers`** `{wait_secs?}` returns nearby receivers.
- **`send`** `{peer_id, items, timeout_secs?}` sends absolute paths or URLs and returns the
  outcome, receiver name and model. When the client passes a progress token it emits
  `notifications/progress` with bytes sent and total.

```sh
claude mcp add airdrop -- airdrop mcp
```

## How it behaves

- **Receivers are identified by opaque Bonjour IDs.** Device names are only revealed
  by the receiver during a transfer, so `send` reports them and `list` cannot.
- **IDs can change** when the receiver restarts AirDrop. List again if a send times out
  in `connecting`.
- **The receiver must be awake and nearby**, with AirDrop set to *Everyone* (or
  *Contacts Only*, with you in their contacts).
- **Devices on the same Apple ID accept automatically.** Anyone else gets an
  Accept/Decline prompt and `send` waits for the answer. `sharingd` gives up on an
  unanswered prompt after about 2 minutes and `send` reports it as failed at
  `waiting_for_answer`. `--timeout` (150s by default) is a backstop: on expiry the
  operation is canceled and the prompt withdrawn.

## Caveats

- **macOS only**, and built on a **private framework**: a macOS update can change or
  remove the API without notice.
- Discovery through `SFBrowser` needs a private Apple entitlement, so `list` browses
  Bonjour (`_airdrop._tcp`, including AWDL) directly. It still opens an `SFBrowser`,
  because that is what makes `sharingd` bring up AWDL; without it sends stall.

## License

MIT
