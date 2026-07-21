# PigTop

<img src="assets/pig.svg" width="120" alt="PigTop mascot: a pig with network signal waves by its ear" />

A macOS-only menu bar app that shows which process is currently consuming the most network
bandwidth, refreshed once per second — useful for diagnosing latency caused by a specific app.

It runs with no Dock icon and lives entirely in the menu bar.

## How it works

Per-process byte counters come from talking directly — via raw FFI, no shelling out — to the
private `com.apple.network.statistics` ("ntstat") kernel control socket, the same interface
Apple's own `nettop` and Activity Monitor use. This is deliberately not implemented by shelling
out to `nettop` or by building a NetworkExtension system extension. No root/sudo is required to
read your own processes' sources.

## Requirements

- macOS (tested on Darwin 24.6.0)
- Rust (2024 edition)

## Building & running

```sh
cargo build                        # build the app + spike binary
cargo run                          # run the menu bar app (must be run interactively; needs a main-thread run loop)
cargo run --bin ntstat_spike       # run the standalone ntstat protocol validation binary (prints top talkers to stdout, no UI)
cargo check                        # fast typecheck
```

There is no automated test suite. Correctness of the ntstat layer is validated empirically by
running `ntstat_spike` while generating real traffic (e.g. `curl -o /dev/null <large file url>`)
and confirming the responsible process's name and rate appear in the printed table.

## Architecture

Two binaries share one library crate (`src/lib.rs` exposes `ntstat` and `rates`):

- **`src/ntstat.rs`** — all `unsafe` FFI. Opens/connects the ntstat kernel control socket
  (`PF_SYSTEM`/`SYSPROTO_CONTROL`, control name `com.apple.network.statistics`), subscribes to all
  TCP/UDP sources (`ADD_ALL_SRCS` per provider), then polls everything in one round-trip per call
  via `GET_UPDATE` with the special `NSTAT_SRC_REF_ALL` srcref. Each poll returns `SRC_UPDATE`
  messages that bundle both cumulative counters *and* the process descriptor (pid/pname) together,
  so no separate per-source `GET_SRC_DESC` calls are needed. `poll_all()` is the main entry point
  and returns `Vec<SourceSample>` (one entry per live TCP/UDP flow).

  The wire-format structs (`NstatMsgHdr`, `NstatTcpDescriptor`, `NstatUdpDescriptor`,
  `NstatCounts`, etc.) are `#[repr(C)]` copies of the real kernel definitions in
  `bsd/net/ntstat.h` / `bsd/netinet/in_stat.h` from `apple-oss-distributions/xnu`, field-for-field
  in the same order — this is what makes the FFI parsing correct without needing manually computed
  byte offsets. All reads out of the raw response buffer go through `read_unaligned` rather than
  pointer-cast dereferencing, because sub-messages inside one aggregate ntstat response are not
  guaranteed to land on 8-byte boundaries.

- **`src/rates.rs`** — pure logic, no FFI. `RateTracker::update()` turns two consecutive
  `poll_all()` snapshots into per-process rates: it keeps previous cumulative byte counts keyed by
  `srcref` (not by pid, since one process commonly owns several concurrent flows), computes
  per-srcref deltas, and sums deltas onto the owning pid. A source's first-ever appearance is
  skipped for rate purposes (no baseline yet) rather than reporting a spurious spike. It also
  tracks a rolling per-pid `peak_rate`/`peak_age` (trailing 60s) so a short burst stays visible in
  the UI for a while after the flow that caused it closes — a peak only gets overwritten by a
  *new* high, and otherwise just ages out; there's no real sliding-window history, only a single
  decaying high-water mark per pid, and nothing persists across process restarts. Results are
  sorted by `max(current total rate, peak rate)`.

- **`src/menubar.rs`** (binary-only) — owns the `tray_icon::TrayIcon` and the dropdown `Menu` via
  the `MenuBar` struct. Only ever touched from the main thread. The dropdown is built **once**, as
  a fixed pool of menu-item slots (plus a separator and "Quit"); updates only call `set_text()` on
  existing slots rather than rebuilding the menu, since replacing the `Menu` object would dismiss
  it if currently open on screen.

- **`src/main.rs`** — wiring. Builds a `tao` event loop, sets `ActivationPolicy::Accessory` so
  there's no Dock icon, spawns a background thread that owns the `NtstatClient` + `RateTracker`
  and loops poll → compute rates → send via `EventLoopProxy` once per second, and only touches the
  `MenuBar` from inside the `tao` event loop.

- **`src/bin/ntstat_spike.rs`** — standalone diagnostic binary with no UI dependency, used to
  validate the ntstat protocol and `rates.rs` layers in isolation (prints per-pid rate *and* peak
  info to stdout).

## Working with the private ntstat protocol

This talks to an undocumented, private macOS kernel interface (no official Apple docs). When
changing `ntstat.rs`:

- Re-derive struct layouts from the actual kernel header rather than guessing — fetch
  `bsd/net/ntstat.h` (and `bsd/netinet/in_stat.h`) from
  [`apple-oss-distributions/xnu`](https://github.com/apple-oss-distributions/xnu) and copy struct
  fields in order.
- Message framing: one `recv()` on the `SOCK_DGRAM` control socket can contain several
  concatenated sub-messages ("aggregate" responses); walk the buffer using each sub-message's own
  `nstat_msg_hdr.length`, never `sizeof(struct)`, since kernel-side descriptor structs have grown
  fields across macOS releases.
- `NSTAT_MSG_HDR_FLAG_CONTINUATION` on a response means more fragments are coming for the same
  poll — `poll_all()` already loops on this; don't remove that handling.

If per-process attribution breaks on a future macOS version, re-diff the Rust structs against the
current kernel header before assuming anything else is wrong.

## Known rough edges (intentional, not bugs)

- Process names come straight from the kernel's abbreviated `pname` field (e.g. "Brave Browser
  Helper" rather than "Brave Browser"). Prettier names via `libproc`/`NSRunningApplication` were
  deliberately deferred as v1.1 polish, not implemented yet.
- No code signing/notarization — this is built and run locally for personal use, not distributed.
