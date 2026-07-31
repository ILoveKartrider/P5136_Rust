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
- [x] per-run bounded file logging at every transport packet boundary, server
  configuration validation, and typed TCP-session failure boundary
- [x] GUI Windows Korean system-font fallback for localized OS errors
- [x] stock-client/`Profile` path resolution for the C#-exported catalog and
  sibling client `Data` directory
- [x] exact `PqGetRiderTaskContext` startup reply from the stock-client crash
  log (`PrGetRiderTaskContext | i32(0)`)
- [x] strict post-rider startup query replies for ranker info, versus rank-one,
  and rider-school expiration state
- [x] C# login/identity/rider/menu initialization path audit, including the
  captured `SpRqGetMaxGiftIdPacket` failure
- [x] strict read-only startup replies for gift sequence, KOIN, favorite-track
  projection, cash inventory, Cash, and TC Cash
- [x] exact captured five-byte `SpRqKoinBalance` request shape
- [x] retained client packet-trace RX inventory and explicit coverage ledger
- [x] five capture-derived read-only menu/mode query replies
- [x] workspace-wide `unsafe_code = "forbid"`

## Connector

- [x] exact P5136 `KartRider.xml` and launcher-profile XML bytes
- [x] Windows-compatible nickname validation on every host
- [x] PIN/BML/encoded-block read/write and build detection
- [x] endpoint replacement and NGS toggle
- [x] immutable backup, process lock, and atomic replacement
- [x] native Windows UAC launch specification
- [x] Wine and CrossOver launch specifications
- [x] no-argument Server/Connector GUI and equivalent CLI
- [x] GUI-configurable server lifecycle with explicit graceful/forced shutdown
- [x] GUI item-probability rank/table editor with client archive, portable XML,
  automatic-client, and bounded-fallback sources; automatic rows require an
  explicit load-and-pin before editing
- [x] explicit client-reported live-rank trust toggle, enabled by default for
  the current LAN/friends model with Combined fallback when disabled
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
- [x] authenticated `PqServerTime` / `PrServerTime` legacy clock reply
- [x] strict stock `PqRequestExtradata` and web-event completion replies
- [x] strict fail-closed cross-profile `PqGetRiderInfo` boundary
- [x] strict `PqStartRiderSchool` and canonical 240-byte physics reply
- [x] strict read-only club state/join/create/list/capacity query boundaries
- [x] strict item-state wire parsing and safe no-reply delete/unlock boundaries

## World, Messenger, and UDP

- [x] actor-owned state mutation and bounded mailboxes
- [x] typed standalone World startup and actor-termination errors
- [x] atomic eight-slot concurrent room admission
- [x] channel-isolated room create/list/join/leave integration
- [x] table-driven public race-channel mapping for speed/item individual/team
  and matching newbie channels
- [x] ready, team, master, and observer actor state
- [x] bounded AI slot/wire primitives
- [x] loading readiness, timeout, start ordering, and frozen race roster
- [x] generation-bound Game/P2P endpoint registration and relay
- [x] synchronized UDP receive/activation order across generation boundaries
- [x] exact-generation actor-owned UDP audience selection
- [x] exact-generation UDP reconnect reset for both Game/P2P routes with an
  arrival-epoch fence against stale datagrams
- [x] bounded TCP GameSlot type 1/2/4/6/9/10/11/12/16 parsing and
  exact-generation atomic relay for evidence-backed audiences
- [x] strict 67-class type-12 state/length/count manifest with typed
  retained/static/default evidence and fail-closed static-only actions
- [x] Messenger identity validation, chat rooms, and single-writer queues
- [x] Messenger split/coalesced frame and mid-frame generation fencing
- [x] bounded actor-output flushing without read-loop starvation
- [ ] TCP-issued nonce/challenge for first UDP endpoint bind
- [ ] capture-derived per-sender movement sequence and tick-wrap policy

## Profile, MyRoom, and gameplay

- [x] versioned atomic profile store and canonical per-profile lanes
- [x] rider/account initialization and catalog-backed inventory preload,
  excluding named kart entries whose exported spec cannot be resolved and
  sanitizing persisted equipment/sidecars that still reference them
- [x] durable rider equipment/plant selection with actor cache publication
- [x] MyRoom core topology and generation-aware cleanup/migration
- [x] fresh MyRoom FirstState projection
- [x] durable owner-info update and exact owner echo
- [x] MyRoom Secede behavior
- [x] bounded MyRoom RequestItems TCP dispatch and atomic publication
- [x] exact requester/owner-generation item authorization and stale-plan retry
- [x] visitor secret redaction and one-shot protected owner-item authorization
- [x] exact-generation MyRoom character-position peer fanout
- [x] TalkLock-aware MyRoom rider-talk atomic peer fanout
- [x] direct MyRoom self-bootstrap and actor-tracked public/protected entry
- [x] current-membership Reenter and bounded random public-room entry
- [x] strict MyRoom item-password status flow and one-shot owner-item grant
- [x] bounded MyRoom RequestEmblems authorization and exact catalog packet
- [x] three-slot transactional main-emblem persistence and cache refresh
- [x] race start/grid/readiness with deterministic fallback track
- [x] actor-owned room track, basic-AI, closed-slot, rider-talk, and macro-chat
  transitions with bounded atomic fanout
- [ ] live track-pool/mode/random-track control surface
- [x] finish, ranking, settlement, team booster, and DNF deadline
- [x] idempotent per-player reward persistence and retry/dead-letter state
- [x] graceful/forced shutdown durability and visibility barriers
- [x] bounded read-only native RHO5 emblem-definition extraction
- [x] bounded read-only legacy Rh-layer-1.1 `item.rho` probability extraction,
  authenticated block decoding, BML parsing, and strict bounded portable XML
  override
- [x] exhaustive MyRoom dispatch and explicit rejection of unclassified identity packets
- [ ] stock-client RequestEmblems/main-emblem E2E
- [x] generation-bound P2P-port report, persistence, and cache refresh
- [x] strict terminal empty protected-item list
- [x] exact normal/preset shop-buy parsing and fail-closed failure reply
- [x] atomic durable canonical favorite-item Get/Update and session-cache refresh
- [x] lease-bound/no-follow C# `Favorite.json` import and encrypted-TCP favorite E2E
- [x] atomic durable canonical locked-item Get/Update and lease-bound/no-follow
  C# `Locked.json` import
- [x] lease-bound/no-follow X-parts update with atomic sidecar publication
  before the exact success reply
- [x] shared generated X-parts grant/serialization table and non-terminal
  inferred failure reply for unpublished values or sidecar persistence errors
- [x] requested kart/speed single-player physics with explicit bounded
  contribution fallbacks
- [x] atomic time-attack start/finish persistence, checked economy arithmetic,
  and one-shot finish replay protection
- [x] bounded complete telemetry codecs for every retained report shape,
  including the isolated four-length unidentified driving report
- [x] authoritative type-1/type-2 item pickup probability roll, exact response
  synthesis, strict captured request/token validation, replay/rate admission,
  and sender-inclusive atomic room broadcast in game types 2/4
- [ ] actor-owned live race position and track-box spawn/collision authority
- [ ] evidence-backed GameSlot item-use/reaction server side effects
- [ ] authoritative actor-owned club repository and atomic membership/create/join/rename flow
- [x] exact MyRoom Career empty-list reply and kind-2 grant consumption
- [x] durable single-player scenario start and bound-revision completion reply
- [ ] evidence-backed nonempty MyRoom Career ownership and marker semantics
- [x] evidence ledger for every deliberate no-reply/unsupported packet in the
  retained corpus
- [x] all 28 formerly unclassified retained TCP request families assigned to
  typed query, actor, durable-state, client-event, single-player, or telemetry
  domains
- [ ] X-parts categories outside the retained category-63 producer evidence
- [ ] remaining kart tuning/upgrades and quest/attendance/progression surface
- [ ] race-wide multi-profile reward journal/recovery

## Evidence and completion gates

- [ ] every supported packet serializer has a C#-derived golden fixture
- [x] C# and Rust synthetic PIN fixture compatibility
- [ ] differential request/response harness passes for the supported flow
- [ ] movement envelope, tick wrap, and fallback are capture-verified
- [ ] production AI start and nonzero AI-master behavior are capture-verified
- [x] all 1,471 retained TCP GameSlot records cross the strict parser
- [ ] static-only type-12 reachability, source/target/object ownership, and
  type 4/6/16 routing are capture-verified
- [ ] Windows, macOS, and Linux CI pass
- [ ] native Windows connector launches a stock P5136 client
- [ ] Wine or CrossOver connector launches the same client
- [ ] two clients login, migrate, join a room, race, persist, and shut down
- [x] opt-in external corpus audit parses all 19,496 retained inbound records,
  resolves all 100 hashes, fully parses all former 28 gap families, strictly
  parses all 1,471 TCP GameSlot records, and decodes every retained routed
  UDP/P2P packet
- [x] build, runtime, connector, and resume documentation exists

Detailed retained-log coverage:
[CAPTURED_PACKET_COVERAGE.md](CAPTURED_PACKET_COVERAGE.md)
