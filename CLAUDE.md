# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`PigTop` is a macOS-only menu bar app (no Dock icon) that shows which process is currently
consuming the most network bandwidth, refreshed once per second, to help diagnose latency caused
by a specific app. It gets per-process byte counters by talking directly — via raw FFI — to the
private `com.apple.network.statistics` ("ntstat") kernel control socket, the same interface
Apple's own `nettop` and Activity Monitor use. This is deliberately not implemented by shelling
out to `nettop` or by building a NetworkExtension system extension.

## Commands

```
cargo build                        # build the app + spike binary
cargo run                          # run the menu bar app (must be run interactively; needs a main-thread run loop)
cargo run --bin ntstat_spike       # run the standalone ntstat protocol validation binary (prints top talkers to stdout, no UI)
cargo check                        # fast typecheck
```

There is no test suite. Correctness of the ntstat layer is validated empirically by running
`ntstat_spike` while generating real traffic (e.g. `curl -o /dev/null <large file url>`) and
confirming the responsible process's name and rate appear in the printed table.

## Architecture

See README.md for the full per-file architecture breakdown (`ntstat.rs`, `rates.rs`,
`menubar.rs`, `main.rs`, `ntstat_spike.rs`). Two behavioral constraints worth restating here
since they're easy to accidentally violate while editing:

- **`src/menubar.rs`** — the dropdown `Menu` is built **once**, as a fixed pool of `SLOT_COUNT`
  `MenuItem`s (plus a separator and "Quit"). `MenuBar::update()` must never call
  `TrayIcon::set_menu()` again after the initial build — only `MenuItem::set_text()` on existing
  slots. Replacing the `Menu` object tears down and recreates the native `NSMenu`, which dismisses
  it if it's currently open on screen; mutating text does not. If the dropdown ever needs to
  grow/shrink its structure (not just swap text), keep this constraint in mind rather than
  reverting to a rebuild-every-tick model.
- All `TrayIcon`/`Menu` access must stay on the main thread, from inside the `tao` event loop
  closure — never from the background polling thread.

## Working with the private ntstat protocol

This talks to an undocumented, private macOS kernel interface (no official Apple docs). When
changing `ntstat.rs`:

- Re-derive struct layouts from the actual kernel header rather than guessing — fetch
  `https://raw.githubusercontent.com/apple-oss-distributions/xnu/main/bsd/net/ntstat.h` (and
  `bsd/netinet/in_stat.h` for `activity_bitmap_t`) and copy struct fields in order.
- Message framing: one `recv()` on the `SOCK_DGRAM` control socket can contain several
  concatenated sub-messages ("aggregate" responses); walk the buffer using each sub-message's own
  `nstat_msg_hdr.length`, never `sizeof(struct)`, since kernel-side descriptor structs have grown
  fields across macOS releases.
- `NSTAT_MSG_HDR_FLAG_CONTINUATION` on a response means more fragments are coming for the same
  poll — `poll_all()` already loops on this; don't remove that handling.
- No root/sudo is required to read your own processes' sources — this has been empirically
  confirmed on this machine (Darwin 24.6.0) and is asserted by `ntstat_spike`'s first check.

## Known rough edges (intentional, not bugs)

- Process names come straight from the kernel's abbreviated `pname` field (e.g. "Brave Browser
  Helper" rather than "Brave Browser"). Prettier names via `libproc`/`NSRunningApplication` were
  deliberately deferred as v1.1 polish, not implemented yet.
- No code signing/notarization — this is built and run locally for personal use, not distributed.
