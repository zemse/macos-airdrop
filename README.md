# macos-airdrop

[![crates.io](https://img.shields.io/crates/v/macos-airdrop.svg)](https://crates.io/crates/macos-airdrop)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

AirDrop **files, folders and links** from the macOS terminal, with **no UI**, and an
**MCP server** so agents can do the same.

It drives the private `Sharing.framework` API behind Finder's AirDrop window, so macOS's
own `sharingd` does the discovery, encryption and transfer. No root or entitlements needed.

- `list` finds nearby receivers with their device names, and counts devices hiding from
  you (Contacts Only without you as a contact).
- `send` sends files, folders and links in one transfer and waits for the answer.
- Names are remembered in `~/.airdrop/known.json`, so a receiver that switches to
  Contacts Only (and withholds its name) still shows up by name.

## Install

```sh
cargo install macos-airdrop
```

This installs a binary named `airdrop`. The first `list` asks for Bluetooth access for
your terminal app.

## Usage

```sh
# Find nearby receivers
airdrop list
# Office-Mac
#   id        571707478742
#   network   awdl0  fe80::b855:9dff:fec0:8be6%awdl0  (63 ms)
#   ...
#
# k's iPhone (remembered; Contacts Only, likely has you as a contact)
#   id        319f34ed1ccb
#   known     named 2 days ago, matched by id

# Send to a receiver by its id
airdrop send 571707478742 ./photo.jpg ./notes/ https://example.com
# sent 3 items to Office-Mac (MacBook Air)

# Name a receiver that was never seen in Everyone mode
airdrop name 319f34ed1ccb "k's iPhone"

# JSON output, and every raw AWDL and Bluetooth device
airdrop list --json
airdrop list --debug
```

`airdrop <command> --help` explains every field, option and exit code.

## MCP server

```sh
claude mcp add airdrop -- airdrop mcp
```

Exposes `list_peers` and `send` tools with the same behaviour as the commands.

## Caveats

- macOS only, on a private framework, so a macOS update can break it.
- A Contacts Only receiver is only visible if it has you in its contacts.

## License

MIT
