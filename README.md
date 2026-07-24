# KartRider P5136 Rust

An independent, clean Rust port of the KartRider P5136 private-server
implementation. The original C# repository is treated as a read-only protocol
reference and is not vendored into this repository.

## Status

This repository is an active compatibility port, not yet a complete game
server. The implemented foundation provides:

- exact P5136 packet-name hashing and primitive serialization;
- P5136 TCP and UDP checksum/encryption with bounded frame decoders;
- the Korean P5136 first-message payload;
- authentication, login, identity fencing, and channel migration over real TCP;
- request-driven startup replies plus catalog-backed rider inventory/equipment;
- actor-owned channel rooms with create/list/join/leave, bounded fan-out, and
  stale-generation cancellation;
- bounded login concurrency and opt-in remote profile creation;
- PIN/BML patching with immutable backups, a process lock, and atomic writes;
- executable/PIN build detection and live Windows UAC, Wine, and CrossOver launch;
- versioned JSON profile persistence compatible with legacy `Launcher.json`;
- a no-argument desktop connector GUI and an equivalent headless CLI.

Room admission and first-state runtime flows are integrated. Modern P5136 UDP
codecs, generation-bound endpoint state, and a bounded messenger hub are
present; UDP/P2P socket relay, messenger stream I/O, and race/gameplay flows
still need integration.

## Build

```text
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p p5136-cli
cargo run -p p5136-cli -- server --catalog /path/to/KartCatalog.xml
```

The configured port follows the original topology: login TCP is base `+ 1`,
game UDP is base `+ 0`, P2P UDP is base `+ 1`, and messenger TCP is base `+ 2`.

`KartCatalog.xml` is runtime data exported from a client installation and is
never committed. Without `--catalog`, login and channel startup remain
available, but `PqGetRider` is deliberately rejected rather than serving an
incomplete inventory. Profiles default to `./Profile`; use `--profile-root` to
change that location.

The server binds to `127.0.0.1` by default. To serve another machine, set both
`--bind` and `--advertise`. Existing profiles may log in remotely, but creating
new profiles from non-loopback clients additionally requires the explicit
`--allow-remote-profile-creation` option.

## Connector

The connector itself is a native Rust application on each host. With no
arguments it opens the desktop GUI. On macOS/Linux it launches only
`KartRider.exe` through Wine or CrossOver; on Windows, `auto` uses a UAC-backed
native launch and refuses elevation unless the executable still has the known
stock P5136 SHA-256. Use `p5136 connect --help` for the headless equivalent and
`--dry-run` to inspect the complete plan without touching files, sockets, or
processes. Closing the GUI cancels any uncommitted probe or launch; an atomic
file preparation already in progress is allowed to finish safely.

## Provenance

Protocol constants and wire behavior were reimplemented from the local
KartRider P5136 C# source. Keep new work free of proprietary client assets,
runtime captures, and unrelated analysis projects.
