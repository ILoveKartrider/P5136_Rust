# KartRider P5136 Rust

An independent, clean Rust port of the KartRider P5136 private-server
implementation. The original C# repository is treated as a read-only protocol
reference and is not vendored into this repository.

## Status

This repository is an early compatibility foundation, not yet a complete game
server. The first milestone provides:

- exact P5136 packet-name hashing and primitive serialization;
- the P5136 TCP checksum/encryption and bounded frame decoder;
- the Korean P5136 first-message payload;
- a Tokio login listener with explicit startup and shutdown;
- an actor-owned room roster, avoiding shared mutable room collections;
- native, Wine, and CrossOver launch-command construction;
- one `p5136` command-line entry point.

The login listener can send the initial client handshake. Login, channel, room,
gameplay, persistence, PIN/XML patching, and a desktop GUI remain to be ported.

## Build

```text
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p p5136-cli -- server --configured-port 39311
```

The configured port follows the original topology: login TCP is base `+ 1`,
game UDP is base `+ 0`, P2P UDP is base `+ 1`, and messenger TCP is base `+ 2`.

## Connector direction

The planned desktop connector is native Rust on each host. It launches only
`KartRider.exe` through Wine or CrossOver on macOS/Linux; on Windows it launches
the executable directly. CLI arguments select headless behavior. A no-argument
launch will become the GUI entry point after PIN/XML patching is ported.

## Provenance

Protocol constants and wire behavior were reimplemented from the local
KartRider P5136 C# source. Keep new work free of proprietary client assets,
runtime captures, and unrelated analysis projects.
