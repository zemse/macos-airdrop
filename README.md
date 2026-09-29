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
# Find nearby receivers (browses for 8s by default)
airdrop list
# Office-Mac
#   id        571707478742
#   airdrop   available
#   network   awdl0  fe80::b855:9dff:fec0:8be6%awdl0  (63 ms)
#   host      63649ac8-04a4-4bbc-9303-251909ab89c5.local:8770
#   features  0x1b3fb  links, DVZIP archives, mixed item types, iris, discover, asset bundles  (+unknown bits 0x1b130)
#   video     HEVC (profiles 1,2,3,4, hardware); ProRes 422 Proxy, 422 LT, 422, 422 HQ, 4444, 4444 XQ
#   hdr       HDR; Dolby Vision profiles 05, 08
#   images    AVCI, AVIF, HEIC, HEICS, HEIF
#   photos    Live Photos v1, asset bundles v1
#
# hidden: 2 devices with AirDrop on (Bluetooth; Everyone or Contacts Only), may include the receivers above
#    -41 dBm  577CBE08-21FA-4004-2B76-11E95C601C73  iPhone, iPad, Mac or Watch
#    -42 dBm  1E6F1F87-EDB4-2940-FD76-6C48196470E4  iPhone, iPad, Mac or Watch
#   so at least 1 device is likely hiding from you (Contacts Only without you as a contact): 2 nearby with AirDrop on, 1 receiver visible above

# Send files, folders and links in one transfer
airdrop send 571707478742 ./photo.jpg ./notes/ https://example.com
# connecting…
# waiting for the receiver to accept…
# transferring…
# sent 3 items to Office-Mac (MacBook Air)

# Name a Contacts Only receiver by hand (names of receivers seen in Everyone mode are
# remembered automatically); list then shows "Office-Mac (remembered; Contacts Only ...)"
airdrop name 448b59406f6b Office-Mac
airdrop forget Office-Mac

# Also list every AWDL ping responder and Bluetooth LE (Continuity) advertiser,
# with decoded state
airdrop list --debug

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

- **`list_peers`** `{wait_secs?, debug?}` returns nearby receivers (and with `debug`,
  the AWDL and Bluetooth devices `list --debug` adds).
- **`send`** `{peer_id, items, timeout_secs?}` sends absolute paths or URLs and returns the
  outcome, receiver name and model. When the client passes a progress token it emits
  `notifications/progress` with bytes sent and total.

```sh
claude mcp add airdrop -- airdrop mcp
```

## How it behaves

- **Receivers are identified by opaque Bonjour IDs.** `list` also asks each one for its
  name and capabilities with AirDrop's `/Discover` request, as Finder does. Receivers
  set to *Contacts Only* answer but withhold their name and feature flags, and their ID
  changes whenever they switch AirDrop mode.
- **Contacts Only receivers are visible only if they have you.** `list` makes `sharingd`
  advertise 2-byte hashes of your phone numbers and emails over Bluetooth; a Contacts Only
  receiver opens its AirDrop service only when one matches its contacts. So a hidden-name
  receiver in `list` likely has you saved (2-byte hashes can collide), and one that does
  not have you does not appear at all.
- **Names are remembered.** `list` saves every name a receiver reports, with its ID, host
  and link-local addresses, in `~/.airdrop/known.json`. A receiver that switches to
  Contacts Only gets a new ID and host, but its AWDL address stayed the same across mode
  switches in testing, so `list` still shows its name, marked *remembered*, with a
  `known` line saying which identifier matched. `airdrop name ID NAME` names one by hand.
  Addresses can rotate (for example after a restart), so treat it as a strong hint.
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
  Bonjour (`_airdrop._tcp`, including AWDL) and sends `/Discover` itself over a socket
  opted into peer-to-peer interfaces (`SO_RECV_ANYIF`). It still opens an `SFBrowser`,
  because that is what makes `sharingd` bring up AWDL; without it sends stall.
- `list` scans Bluetooth for the hidden section, so its first run asks for Bluetooth
  access for your terminal app. Hidden devices are the ones whose Continuity Nearby Info
  says AirDrop receiving is on. That bit only tells Receiving Off apart; it is the same for
  Everyone and Contacts Only (verified by toggling an iPhone). Hidden devices cannot be
  matched to receivers, so visible receivers are counted there too, and `list` reports
  the surplus of nearby ones (signal at least `--min-rssi`, default -65 dBm, chosen from
  in-room devices at -28 to -52 dBm and an out-of-range one at -71 to -82 dBm) as likely
  hiding from you. Continuity message labels come from
  [furiousMAC](https://github.com/furiousMAC/continuity)'s iOS 13 era reverse engineering
  and can be wrong on newer systems (a phone on a desk reads as "driving"), so labels not
  confirmed on current devices are marked unverified and the raw bytes are shown alongside.
- Feature flag names come from [OpenDrop](https://github.com/seemoo-lab/opendrop)'s
  reverse engineering; newer bits are shown as unknown.

## License

MIT
