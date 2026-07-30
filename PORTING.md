# Porting ledger

Authoritative handoff: [PORTING_STATUS.md](PORTING_STATUS.md)

A checked item means the Rust path is implemented and covered by automated
tests. It does not by itself mean stock-client end-to-end compatibility has
been demonstrated.

## Compatibility foundation

- [x] P5136 topology, port offsets, and boundary validation
- [x] zero-seeded packet-name Adler-32
- [x] little-endian primitives and .NET-compatible UTF-16 strings
- [x] bounded login TCP framing, encryption, checksum, and IV progression
- [x] exact production `PcFirstMessage` plaintext payload
- [x] fragmented and coalesced TCP frame coverage
- [x] encoded primitive substitution table
- [x] game/P2P UDP envelopes, routed headers, controls, and bounded opaque relay
- [x] bounded Messenger framing and logical packet codecs
- [x] workspace-wide `unsafe_code = "forbid"`

## Connector

- [x] exact P5136 `KartRider.xml` and launcher-profile XML bytes
- [x] Windows-compatible nickname validation on every host
- [x] PIN/BML/encoded-block read/write and build detection
- [x] endpoint replacement and NGS toggle
- [x] immutable backup, process lock, and atomic replacement
- [x] native Windows UAC launch specification
- [x] Wine and CrossOver launch specifications
- [x] no-argument desktop GUI and equivalent CLI
- [ ] verified launch of a stock client on Windows
- [ ] verified launch of the same client through Wine or CrossOver

## Login, identity, and migration

- [x] `PqCnAuthenLogin` / `PrCnAuthenLogin`
- [x] BML-backed `PqLogin` parser and startup response pairing
- [x] duplicate nickname rejection and stable user number
- [x] exact session generation and stale-owner rejection
- [x] channel-switch permit and `PqChannelMovein` transfer
- [x] source-disconnect deferral and permit expiry
- [x] linear per-generation identity-operation leases
- [x] actor/durable child leases survive requester cancellation
- [x] freeze-before-drain migration with exact abort/expiry wake-up
- [x] ordered ACK plus result-free identity/MyRoom/protocol commit boundary
- [x] deferred releases count toward capacity and shutdown barriers
- [x] cross-World capability misuse is rejected

## World, Messenger, and UDP

- [x] actor-owned state mutation and bounded mailboxes
- [x] typed standalone World startup and actor-termination errors
- [x] atomic eight-slot concurrent room admission
- [x] channel-isolated room create/list/join/leave integration
- [x] ready, team, master, and observer actor state
- [x] bounded AI slot/wire primitives
- [x] loading readiness, timeout, start ordering, and frozen race roster
- [x] generation-bound Game/P2P endpoint registration and relay
- [x] synchronized UDP receive/activation order across generation boundaries
- [x] exact-generation actor-owned UDP audience selection
- [x] Messenger identity validation, chat rooms, and single-writer queues
- [x] Messenger split/coalesced frame and mid-frame generation fencing
- [x] bounded actor-output flushing without read-loop starvation
- [ ] TCP-issued nonce/challenge for first UDP endpoint bind
- [ ] capture-derived per-sender movement sequence and tick-wrap policy

## Profile, MyRoom, and gameplay

- [x] versioned atomic profile store and canonical per-profile lanes
- [x] rider/account initialization and catalog-backed inventory preload
- [x] durable rider equipment/plant selection with actor cache publication
- [x] MyRoom core topology and generation-aware cleanup/migration
- [x] fresh MyRoom FirstState projection
- [x] durable owner-info update and exact owner echo
- [x] MyRoom Secede behavior
- [x] bounded MyRoom RequestItems TCP dispatch and atomic publication
- [x] exact requester/owner-generation item authorization and stale-plan retry
- [x] visitor secret redaction and one-shot protected owner-item authorization
- [x] exact-generation MyRoom character-position peer fanout
- [x] direct MyRoom self-bootstrap and actor-tracked public/protected entry
- [x] current-membership Reenter and bounded random public-room entry
- [x] strict MyRoom item-password status flow and one-shot owner-item grant
- [x] race start/grid/readiness with deterministic fallback track
- [ ] live track-pool/mode/random-track control surface
- [x] finish, ranking, settlement, team booster, and DNF deadline
- [x] idempotent per-player reward persistence and retry/dead-letter state
- [x] graceful/forced shutdown durability and visibility barriers
- [ ] MyRoom talk, emblem-catalog, and main-emblem flows
- [ ] P2P-port and club-name profile mutation/refresh paths
- [ ] remaining kart tuning/upgrades and quest/attendance/progression surface
- [ ] race-wide multi-profile reward journal/recovery

## Evidence and completion gates

- [ ] every supported packet serializer has a C#-derived golden fixture
- [x] C# and Rust synthetic PIN fixture compatibility
- [ ] differential request/response harness passes for the supported flow
- [ ] movement envelope, tick wrap, and fallback are capture-verified
- [ ] production AI start and nonzero AI-master behavior are capture-verified
- [ ] generic type-12 behavior is capture-verified
- [ ] Windows, macOS, and Linux CI pass
- [ ] native Windows connector launches a stock P5136 client
- [ ] Wine or CrossOver connector launches the same client
- [ ] two clients login, migrate, join a room, race, persist, and shut down
- [x] build, runtime, connector, and resume documentation exists
