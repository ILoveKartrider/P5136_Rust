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
- request-driven startup replies, legacy server time, and catalog-backed rider
  inventory/equipment;
- strict terminal empty protected-item list compatibility reply;
- exact normal/preset shop-buy decoding with a fail-closed compatibility reply
  and no economy mutation;
- actor-owned channel rooms with create/list/join/leave, bounded fan-out, and
  stale-generation cancellation;
- bounded messenger TCP sessions with generation-fenced identity publication;
- supervised game/P2P UDP sockets with exact Echo/TimeSync replies and
  generation-fenced room relay;
- exact client P2P-port reporting with durable, generation-bound ordinary-room
  and MyRoom cache refresh;
- exact ready-stage, race-control, race-start, settlement, and 235-byte kart
  physics codecs used by the actor-integrated human race flow;
- strict TCP `GameSlotPacket` decoding with generation-fenced, atomically
  reserved type-9/10/11/12 relay;
- bounded login concurrency and opt-in remote profile creation;
- PIN/BML patching with immutable backups, a process lock, and atomic writes;
- executable/PIN build detection and live Windows UAC, Wine, and CrossOver launch;
- versioned JSON profile persistence compatible with legacy `Launcher.json`;
- bounded read-only KR RHO5 emblem loading with authenticated decompression;
- a no-argument desktop GUI with separate Server and Connector tabs, plus an
  equivalent headless CLI.

Room admission, first-state, messenger, and UDP/P2P runtime flows are
integrated. UDP authorization and room audience selection run inside the world
actor so channel migration cannot race a stale relay decision. Human
ready/loading, race start, finish, ranking, settlement, reward persistence, and
the MyRoom direct/re-enter/random-public-entry, FirstState, owner-info,
RequestItems, RequestEmblems, terminal Career-list, three-slot main-emblem
update, character-position, RiderTalk, and Secede paths are also
actor-integrated.
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
response and is consumed even if its requester queue is full. The matching
Career grant likewise authorizes exactly one strict hash-only
`RequestCareerList`; Rust publishes only the conservative 16-byte terminal
empty list derived from the client's marker-equality rule and consumes the
grant even when publication is stale or backpressured. Nonempty Career
records, marker progression, and the separate owner-info exchange remain
unimplemented until stronger evidence defines their ownership and policy.
Main-emblem updates parse the stock client's exact three-`i16` body, require
the present owner, validate every nonzero ID against an immutable positive-ID
catalog, and publish success only after a transactional profile write is
durable. The ordinary room cache is refreshed silently; no unsupported
MyRoom peer fanout is invented.
Client endpoint reports are exact ten-byte packets. Rust discards the claimed
IPv4 bytes, derives the advertised address from the authenticated TCP peer,
persists only the reported P2P port, and publishes it only for the same active
identity generation. Port zero is an absolute clear and every login or
same-channel replacement starts unadvertised even if a historical profile
value exists. IPv4-mapped peers retain their embedded IPv4 address; native
IPv6 peers remain `(0.0.0.0, 0)` because the legacy room wire format cannot
represent them. The sibling game-UDP report is validated but cannot replace
the endpoint authority learned by the UDP ingress path. No speculative
endpoint ACK or live peer-refresh packet is emitted without capture evidence.
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
`PqLockedItemGet` is now a strict four-byte request and returns exactly one
eight-byte terminal empty `PrLockedItemGet` to the authenticated requester.
It performs no profile-store I/O or shared-state mutation. Nonempty protected
items and `PqLockedItemUpdate` remain unimplemented because their
P5136-specific wire and persistence policy lacks producer or capture proof.
`PqServerTime` likewise has an explicit authenticated path and returns the
exact eight-byte `PrServerTime`: reply hash, days since 1900 modulo 65,536,
and quarter-seconds since local midnight. The corroborating C# handlers do not
consume a request body, so Rust accepts an unused suffix only under the global
frame bound instead of claiming an unproven hash-only schema. The reply is
direct and read-only. It uses the shared identity-operation admission and
authorization actor commands but creates no ServerTime-specific mutation,
disk I/O, or fanout.

Stock producers prove that `PqRequestExtradata` and
`PqWebEventCompleteCheckPacket` are both exact hash-only requests. Rust rejects
the trailing bytes that the C# handlers silently accepted. The web-event reply
is the exact empty four-byte named packet; the six-byte extra-data reply is an
exact zero code plus an absent optional-value marker. Rust exposes no
speculative extra-data success code or value. Both paths reuse global identity
admission and authorization, return one direct requester reply, and leave
profile, disk, World domain state, and peer queues unchanged.

`PqGetRiderInfo` now has a strict stock-producer parser for its zero scalar,
empty reserved string, bounded target nickname, and raw mode byte. Successful
cross-profile projection is deliberately unavailable until visibility,
offline lookup, and public-profile policy are specified. The authenticated
path therefore returns only the exact five-byte failure reply. It never logs
the target nickname, loads or creates the target profile, mutates state, or
fans out a request.

`PqStartRiderSchool` is likewise exact: one encoded byte after the request hash
and no suffix. Its 240-byte reply uses the shared validated P5136 kart-physics
builder. This deliberately keeps the normal physics formula instead of
copying two drifted C# shortcut constants or depending on process-global
`SpeedPatch` state. Both the request and the direct reply are profile
read-only, generation-fenced, and covered by stale/quiesce error-priority
tests.

Both stock P5136 shop-buy aliases are also explicit. Rust strictly parses the
producer-derived 9-byte normal body and 11-byte item-preset body, then returns
the exact common 29-byte failure packet. It does not execute a purchase,
change inventory or currency, persist profile data, or fan out a request.
Malformed shop packets are bounded nonfatal drops so the authenticated session
remains usable; stale identity ownership, an unbound profile, quiesce, and
actor/system failures still propagate. Field widths and order are evidenced,
but unknown business meanings and value ranges are deliberately not invented.

TCP `GameSlotPacket` is handled separately from the opaque UDP packet that
shares its name. Rust bounds the complete TCP packet at 1013 bytes and each
nested blob at 960 bytes, validates P5136 types 1, 2, 9, 10, 11, and 12, and
freezes the audited 74 type-12 operation pairs in a static allowlist. The
World actor accepts only an exact frozen-generation human racer during
`Running` or the still-open `Settling` window. It relays the original bytes to
the exact current frozen audience, applies the type-11 recipient mask, and
reserves every recipient queue before publishing. Valid Barricade placement
is the sole sender-inclusive relay. Malformed, spoofed, unsupported,
inactive-frozen-generation, wrong-phase, and backpressured item events are
observable nonfatal drops; stale global identity ownership, actor termination,
and invariant failures still propagate.

Rust deliberately does not copy the C# item side effects called out by the
stability audit. Type 1/2 pickup packets require a server-selected item in a
different wire field, so they are validated and explicitly deferred instead
of relaying the client rank as an item ID. Type 10/11 packets are relayed
without speculative kart effects, bonus-item synthesis, probability rerolls,
or double item transformation. Those behaviors remain capture- and
design-blocked rather than bug-for-bug cloned.

The remaining compatibility work is concentrated in authoritative item-pickup
synthesis, capture-backed GameSlot bodies and side effects, the remaining
MyRoom and economy requests, capture-derived movement sequencing and UDP
first-bind capabilities, packet fixtures, green cross-platform CI evidence,
and stock-client end-to-end validation. See
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

### Runtime packet diagnostics

Every `p5136` run creates a log file at
`<p5136 executable directory>/logs/p5136-<timestamp>-<pid>.log`. The GUI shows
the exact path beneath its title; set `P5136_LOG_DIR` to place it elsewhere.
If the directory or file cannot be created, startup fails instead of silently
running without diagnostics.

The file records every server transport-boundary packet with direction, peer,
byte count, first-word little-endian value (the logical frame hash for
TCP/Messenger), and hexadecimal payload: login TCP plaintext
logical frames (including the first server frame), complete UDP wire datagrams
before decode and after a successful send, and Messenger TCP logical frames.
Malformed/partial TCP and Messenger wire frames are also recorded before their
decoder returns an error. To keep a hostile maximum-size frame from exhausting
disk, hexadecimal payload capture is limited to the first 4 KiB per record;
the record remains present and says `truncated = true` with its full byte
count.

Normal client traffic records every packet. This is deliberately not an
unbounded remote-disk-write capability: raw packet records are process-wide
limited to 512 per second, and the file writer has a bounded asynchronous
queue. A rate-limited interval emits a `packet diagnostics were rate-limited`
record with its dropped count; if the file queue itself is full, its newest
records are dropped rather than stalling the server. Preserve these warnings
alongside a crash log—there is no claim of complete raw capture during a
diagnostic-overload attack.

These files can contain authentication material, nicknames, and chat text.
They are local diagnostic data, are never committed, and should be handled as
sensitive. There is no automatic deletion: preserve the log from a failing
client run, then remove it manually when it is no longer needed.

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

### Two-client LAN smoke test

The P5136 connector prepares and launches one client installation; it does not
turn one game directory into concurrent client instances. Use two separately
installed, connector-recognized P5136 clients on the same LAN. On the server
host, bind to `0.0.0.0`, advertise that host's current LAN IPv4 address, and
enable remote profile creation only when the remote nickname does not already
exist. Both connector instances must use that advertised IPv4 address and the
same configured base port.

Allow the server host's inbound game UDP `base`, login TCP/P2P UDP `base + 1`,
and messenger TCP `base + 2` through its host firewall for the LAN profile in
use. Do not substitute virtual-adapter, VPN, or loopback addresses for the
physical LAN address unless every test client deliberately uses that overlay.
The server's optional `--client-data-dir` must refer to the exact stock client
`Data` directory; a mismatched client-data copy fails closed during RHO5 emblem
catalog discovery.

## Desktop GUI and connector

With no arguments, `p5136` opens the desktop GUI. The Server tab exposes the
same server options as the CLI: bind and advertised addresses, configured port,
profile root, optional catalog and client-data paths, remote profile creation,
and advanced session limits/timeouts. Starting and graceful stopping keep the
supervisor on a dedicated worker; if retained reward recovery blocks graceful
shutdown, the GUI reports that state and requires an explicit force-stop click.
Closing a window with a live server cancels the close, requests graceful
shutdown, waits for a bounded interval, then requests a force-stop and joins
the worker before allowing process exit.
The Server tab can copy its advertised address and configured port into the
Connector tab. GUI edits apply to the next server start and are intentionally
not persisted.

The Connector tab is a native Rust application on each host. On macOS/Linux it
launches only `KartRider.exe` through Wine or CrossOver; on Windows, `auto` uses
a UAC-backed native launch and refuses elevation unless the executable still
has the known stock P5136 SHA-256. Use `p5136 connect --help` for the headless
equivalent and `--dry-run` to inspect the complete plan without touching files,
sockets, or processes. Closing the GUI cancels any uncommitted probe or launch;
an atomic file preparation already in progress is allowed to finish safely.

## Provenance

Protocol constants and wire behavior were reimplemented from the local
KartRider P5136 C# source. Keep new work free of proprietary client assets,
runtime captures, and unrelated analysis projects.
