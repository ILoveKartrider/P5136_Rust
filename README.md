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
- bounded messenger TCP sessions with generation-fenced identity publication;
- supervised game/P2P UDP sockets with exact Echo/TimeSync replies and
  generation-fenced room relay;
- exact ready-stage, race-control, race-start, settlement, and 235-byte kart
  physics codecs used by the actor-integrated human race flow;
- bounded login concurrency and opt-in remote profile creation;
- PIN/BML patching with immutable backups, a process lock, and atomic writes;
- executable/PIN build detection and live Windows UAC, Wine, and CrossOver launch;
- versioned JSON profile persistence compatible with legacy `Launcher.json`;
- bounded read-only KR RHO5 emblem loading with authenticated decompression;
- a no-argument desktop connector GUI and an equivalent headless CLI.

Room admission, first-state, messenger, and UDP/P2P runtime flows are
integrated. UDP authorization and room audience selection run inside the world
actor so channel migration cannot race a stale relay decision. Human
ready/loading, race start, finish, ranking, settlement, reward persistence, and
the MyRoom direct/re-enter/random-public-entry, FirstState, owner-info,
RequestItems, RequestEmblems, three-slot main-emblem update,
character-position, RiderTalk, and Secede paths are also actor-integrated.
Reenter restores an exact current membership before falling back to the
rider's own room. Random entry selects only actor-tracked, owner-present,
non-full public rooms. Direct entry strictly parses the required owner and
room-password strings: protected rooms prompt on empty input, return the
client's status-4 mismatch on an incorrect password, and admit only an exact
match. Every visitor reply redacts stored secrets. The separate item-password
flow parses kind plus password and returns the stock client's typed
`0/1/2/3` statuses. A successful protected check retains no plaintext and
mints one move-only, exact-generation grant for at most one matching follow-up;
Garage and Item Dictionary grants authorize exactly one `RequestItems`.
RequestItems loads a bounded owner snapshot under the canonical profile lane
and publishes its complete ordered response as one actor-owned queue batch.
The matching Emblem grant authorizes exactly one bounded `RequestEmblems`
response and is consumed even if its requester queue is full. Main-emblem
updates parse the stock client's exact three-`i16` body, require the present
owner, validate every nonzero ID against an immutable positive-ID catalog, and
publish success only after a transactional profile write is durable. The
ordinary room cache is refreshed silently; no unsupported MyRoom peer fanout
is invented.
Character positions use actor-derived sender slots and exact-generation peer
audiences with all-recipient atomic queue reservation. RiderTalk uses the same
atomic peer path, bounds and redacts the message-bearing request, enforces the
owner's live `TalkLock` policy (`0` off, nonzero on), derives the canonical
sender slot, and never echoes to the sender.
Migration freezes and drains exact generation-bound operation
leases, then crosses a pre-reserved ACK and result-free
identity/MyRoom/protocol commit boundary. Messenger frames are rechecked across
generation changes.
Authenticated packet dispatch is fail-closed: explicitly catalogued
compatibility no-reply packets remain no-reply, while an unclassified hash
returns a typed session error instead of being mistaken for a successful
handler. MyRoom dispatch is exhaustive so new protocol variants cannot
silently fall through.

The remaining compatibility work is concentrated in the unported MyRoom and
economy requests, capture-derived movement sequencing and UDP first-bind
capabilities, packet fixtures, green cross-platform CI evidence, and
stock-client end-to-end validation. See
[PORTING_STATUS.md](PORTING_STATUS.md) for the exact handoff and
[PORTING.md](PORTING.md) for the feature ledger.

## Build

```text
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo run -p p5136-cli
cargo run -p p5136-cli -- server --catalog /path/to/KartCatalog.xml --client-data-dir /path/to/client/Data
```

The configured port follows the original topology: login TCP is base `+ 1`,
game UDP is base `+ 0`, P2P UDP is base `+ 1`, and messenger TCP is base `+ 2`.

`KartCatalog.xml` is runtime data exported from a client installation and is
never committed. Without `--catalog`, login and channel startup remain
available, but `PqGetRider` is deliberately rejected rather than serving an
incomplete inventory. Profiles default to `./Profile`; use `--profile-root` to
change that location.

`--client-data-dir` points at the stock client's `Data` directory. At startup,
the server uses a bounded, read-only RHO5 reader to locate exactly one
`etc_/emblem/emblem@kr.xml`, authenticate and decompress it in memory, and load
its source-ordered positive IDs for `RequestEmblems` and main-selection
validation. The installed KR data fixture yields 586 unique IDs, minimum 1 and
maximum 8803. Startup fails before binding listeners if the configured archive
set or XML is missing, ambiguous, malformed, or exceeds its limits.

An optional `<Emblems>` section in the format-3 `KartCatalog.xml` remains a
portable fallback when `--client-data-dir` is omitted. RHO5 definitions take
precedence when both sources are configured. Without either source, the server
fails closed with an empty emblem response and permits only `0,0,0`; `0` is
solely the empty-selection sentinel. The existing C# exporter does not emit
`<Emblems>`. Client archives and extracted XML must never be committed.

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
