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
- [x] GUI platform Korean system-font fallback for Windows, macOS, and Linux
- [x] stock-client root/`Profile`/`Data` resolution to the canonical client
  `Data` directory without requiring a C#-exported catalog
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
- [x] Wine, CrossOver, and preconfigured macOS Sikarugir wrapper launch specifications
- [x] no-argument Server/Connector GUI and equivalent CLI
- [x] GUI-configurable server lifecycle with explicit graceful/forced shutdown
- [x] GUI item-probability rank/table editor with client archive, portable XML,
  automatic-client, and bounded-fallback sources; automatic rows require an
  explicit load-and-pin before editing
- [x] explicit client-reported live-rank trust toggle, enabled by default for
  the current LAN/friends model with Combined fallback when disabled
- [x] Korean GUI labels, LAN IPv4 discovery/apply control, explicit IP-only
  bind/advertise guidance, invalid advertised-address rejection, and exact
  server-start item.rho snapshot confirmation
- [x] bounded GUI persistence for server/connector inputs, including paths,
  runner settings, limits, probability tables, and random-track overrides;
  client root/Profile selection is required at the GUI server boundary
- [x] bounded legacy RHO 1.0/1.1 reader and client `track_common.rho` random
  catalog with per-mode/selector pools, GUI overrides, AI filtering, and
  process-RNG selection and room-owned no-repeat history
- [x] C#-exact ASCII-boundary S0-S8 room-title parser and per-equipment modern
  speed-physics variants selected transactionally at race start
- [x] exact P5136 `PqChangeRoomInfoPacket` / `PrChangeRoomInfoPacket` codec,
  room-master-only atomic title/password update and all-room reply; a changed
  S0-S8 token selects that variant in the next `GrCommandStartPacket` while the
  existing channel/session speed byte remains unchanged, matching the C# server
- [x] channel-consistent no-title physics fallback: S6 for individual/team
  infinite-booster channels, S7 for speed, and S8 for item game-type rows
- [ ] verified launch of a stock client on Windows
- [ ] verified launch of the same client through Wine, CrossOver, or Sikarugir

## Login, identity, and migration

- [x] `PqCnAuthenLogin` / `PrCnAuthenLogin`
- [x] BML-backed `PqLogin` parser and startup response pairing
- [x] duplicate nickname rejection and stable user number
- [x] exact session generation and stale-owner rejection
- [x] channel-switch permit and `PqChannelMovein` transfer
- [x] C#-compatible local `PqClubChannelSwitch` UI hand-off (`mode=1`,
  channel 13, zero endpoint) without invalidating the authenticated session
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
- [x] strict `PcStartMatching` seven-word envelope and cancellation parsing,
  with the complete three-byte empty/create `PcMatchingFound` state rather
  than the C# server's truncated two-byte reply

## World, Messenger, and UDP

- [x] actor-owned state mutation and bounded mailboxes
- [x] typed standalone World startup and actor-termination errors
- [x] atomic eight-slot concurrent room admission
- [x] channel-isolated room create/list/join/leave integration
- [x] table-driven public race-channel mapping for speed/item individual/team
  and matching newbie channels
- [x] ready, team, master, and observer actor state
- [x] team-room admission balances active human/AI counts and assigns matching
  blue/red physical slot ranges, choosing blue only when counts are tied
- [x] bounded AI slot/wire primitives
- [x] C#-compatible frozen-roster loading handshake: `GameControl(state=0)`
  arms, successful UDP time-sync marks ready, then timeout and ordered start
- [x] generation-bound Game/P2P endpoint registration, authenticated runtime
  P2P-source fallback, and relay
- [x] synchronized UDP receive/activation order across generation boundaries
- [x] exact-generation actor-owned UDP audience selection
- [x] eight-client real-socket UDP relay mock: all eight senders fan out exact
  movement packets to the other seven (56 datagrams), paired with an eight-human
  frozen-roster target-resolution test
- [x] configurable eight-client UDP stress test (120 seconds by default):
  deterministic timing jitter and per-client latency force ping/movement ingress
  reordering; deliberately old sender ticks and unordered exact-output matching
  prove 7-way relay without loss, duplication, route corruption, or global-tick
  assumptions
- [x] exact-generation UDP reconnect reset for both Game/P2P routes with an
  arrival-epoch fence against stale datagrams
- [x] bounded TCP GameSlot type 1/2/4/5/6/7/8/9/10/11/12/13/16/17 parsing and
  exact-generation atomic relay for evidence-backed audiences
- [x] bounded type-12 compatibility relay: exact common envelope, authenticated
  sender and peer mask, then a known operation/base pair from the 80-class
  native-writer manifest plus five C# enum-derived pairs; native shapes retain
  typed retained/static/default diagnostics without capture-gated routing
- [x] capture-backed `GopBarricade` state 1 placement, state 2 impact, state 3
  post-impact resolution, and state 4 terminal relay, including the valid
  zero-peer single-racer audience
- [x] race-epoch-bound object registry for strictly decoded type-12 operations:
  class/owner/generation binding, class-specific lifecycle admission, exact
  `(state, transition token, target)` duplicate-impact suppression, terminal
  suppression, and commit only after the complete exact-generation peer
  audience has been reserved
- [x] producer/consumer-derived source/target/state semantics for 79 of 80
  direct-writer classes: the original 15 common contracts plus Coke/Snow/
  Infected bombs, rolling variants, WaterMine/TimeMine, timed bomb variants,
  BigTimebomb's actor-guarded activation/impact/SpecialShield phase map, Shield/SpecialShield,
  three UFO forms, LockdownRocket, Thunderbolt's counted target set, ForceZone,
  Oil, Silence, Siren/SirenShield, SpecialSmall, Cloud/Cloud2, Magnet, and
  SpeedDown, Devil/MqDevil/NewDevil, Angel, GoldShield, EMP, Ghost, Icefly, Scanning,
  SlotLock, SpecialSiren, SpaceCraft, StraightRocket, Balloon, HeadBand,
  Dynamite, Hammer, Press, RobotBeam, TombStone, Block, BoundWall, Cube,
  CubeForBoss, EventObject, GiantTalisman, WitchUnionMagic, TargetKart,
  BossPrison, BoundRoad, Course, Falling, and Piratebomb; 75 have a named
  lifecycle transition. Native source omissions in Block,
  RobotBeam, and TombStone remain explicit instead of fabricating raw fields
- [x] corrected `GopCubeForBoss` writer lengths to 77/69 (the earlier 73/65
  census omitted a class dword), and recorded that Course/EventObject raw 12
  is an object ID rather than a shared lifecycle state
- [x] recovered occurrence paths for the last five in-scope classes:
  BossPrison from GoBossKart target selection; BoundRoad from BombRobot/
  MechanicBall lane patterns; Falling from PetitMeteor/SpaceBombing lane
  patterns; Piratebomb from controller branch 12 target selection; and Course
  from `goal`/`Ev_*` notifications whose concrete peer consumer is a no-op
- [x] audited all 218 ordinary `_Rnn/_Inn` track archives and separated static
  shared objects from client-local course effects: base scenes place Banana,
  Cube, Mine, WaterMine, and EventObject runtime classes already covered by
  exact schemas, while obstacle/dummy, warp/weather/rail/lens-flare/flash
  controls remain client scene/physics state; see
  `analysis/P5136_RI_TRACK_GIMMICK_AUDIT.md`
- [x] semantic decoding is crate-private and requires the private-field
  `ValidatedItemOperation` capability returned by exact schema validation;
  callers cannot bypass state/length validation with an arbitrary raw slice
- [x] semantic registry transitions preserve Barricade initialize -> place and
  Mine remove -> respawn -> impact; unresolved/no-op bodies cannot mutate hit
  fingerprints, post-terminal updates are suppressed, and consumer-only
  removal evidence cannot mint unseen tombstones
- [x] IDB-ledger-literal 15-class type-12 wire -> parser -> semantic -> registry
  table plus an independent 29-state second-pass literal table, real Barricade
  0 -> 1 World fanout, Mine 1 -> 5 -> 6 -> 2 parse/commit, and per-race
  registry reset regression paths
- [x] shield/UFO/Lockdown/Thunderbolt 23-state and ordinary-effect 27-state
  literal wire -> parser -> semantic -> registry tables, including conditional
  ForceZone/Oil success guards, Silence's explicit no-op, SpecialSmall's
  runtime-only flag update, Cloud's target-only hit, and SpeedDown teardown
- [x] all 54 headings from the supplied Korean item page represented in a
  typed gameplay-reference catalog with category/target/effect hints, 41
  proven P5136 numeric/name links, evidence-graded `Gop*` candidates, literal
  54-row semantic and 41-pair ID manifests, separate class/heading evidence,
  and an explicit rule that modern page timers/probabilities never define the
  wire codec; see [ITEM_GAMEPLAY_COVERAGE.md](ITEM_GAMEPLAY_COVERAGE.md)
- [x] all active ambiguous gameplay joins closed with native producer plus
  direct-RHO evidence: Guide Rocket 33=`GopRocket`, Timebomb 13=`GopTimebomb`,
  Ice Waterfly 118=`GopSnowWaterfly`, Super Shield 18=`GopShield`, and exact
  Cloud/Cloud2 discriminator maps. Giant Missile 73, Abyss special 122, Kefi
  special 80, SpecialShield 40, and RainbowCloud2 116 are retained as distinct
  non-page associations. Rolling Waterbomb, Jiangshi, and first-place Devil
  are explicitly deferred by user scope.
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
- [x] nickname-scoped duplicate-kart inventory grants with Korean catalog-name/
  decimal-ID search, retry-safe atomic serial allocation, immutable revision
  publication, offline root leasing, orphan sidecar-serial reservation,
  store-identity validation, catalog path/content snapshot fencing, and GUI
  inspection; serial-2+ copies share the existing `(kart_id, serial)`
  equipment/plant/parts identity
- [x] durable rider equipment/plant selection with actor cache publication
  and lobby-only next-race kart-physics refresh; Loading/Running physics remain
  frozen against mid-race equipment changes
- [x] exact stock 20-byte `PqEquipTuningExPacket` body, including the typed
  displaced-part descriptor, fixed-width success/failure replies, and
  lease-bound/no-follow atomic `PlantData.json` publication
- [x] exact P5136 rider-equipment plant-slot order: engine, wheel, handle, kit
  map to inventory categories `43, 45, 44, 46`; invalid or ungranted equipment
  remains fail-closed and terminates the login TCP session
- [x] `PrKartLevelUpProbText` keeps the 100%-success server policy separate from
  its client-facing result code: accepted selection replies use the retained C#
  golden `49 08 4C 59 00 00 00 00`, avoiding the client crash caused by writing
  the percentage (`100`) into the result field
- [x] all 91 recovered P5136 plant-part contributions, matched by exact
  `(kart_id, serial)` and composed into distinct speed/item plus S0-S8
  room-start physics blocks
- [x] exact legacy kart-level request/reply codecs and `LevelData.json` preload,
  including bounded 0..10 slots and 35-point validation, reconnect durability,
  and kart-level physics composition
- [x] sidecar-isolated lenient login/inventory preload with strict target-only
  mutation, canonical last-wins `(kart_id, serial)` deduplication, and explicit
  post-rename committed-but-durability-uncertain outcomes
- [x] intentional free-server enhancement policy: owned target and donor are
  authenticated, success probability/result are always 100%/success, request
  cost and balances are not deducted, and the zero consumed-material descriptor
  preserves the donor kart on both client and server; signed point deltas allow
  bounded redistribution without exceeding any slot or the total budget
- [x] native-consumer-correct Floater socket/tune/protect/reset codecs, including
  full-width failure replies, category-3 target validation, and fixes for the C#
  socket record-state and reset pre/post-state divergence
- [x] lease-bound/no-follow atomic `TuneData.json`, reconnect preload ahead of
  plant/level/parts, serial-normalized per-kart state, non-consuming activation
  kits, bounded protection/reset transitions, and duplicate-protection rejection
- [x] exact C# speed-Floater physics for codes 103..903; Black-H's fixed
  603/703/903 set reaches the frozen room kart-physics block while item-mode
  codes remain valid persistent client semantics without fabricated speed values
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
- [x] actor-owned room track, basic-AI, and closed-slot transitions plus
  sender-excluding rider-talk and C#-exact macro-chat relay with bounded atomic
  fanout; racers and observers can send through Lobby, Loading, Running, and
  Settling, with nonzero-team senders filtered to their team and team zero
  broadcasting to all room peers
- [x] requester-only `GrFirstRequestPacket` room-state rehydration, preventing
  a redundant peer `GrSlotDataPacket` from crossing a later race/ceremony
  scene boundary
- [x] live track-pool/mode/random-track control surface
- [x] finish, ranking, settlement, team booster, and DNF deadline, including
  the deployed C# ceremony order `GameControl(state=4) -> GameNextStage ->
  GameResult`
- [x] durable settlement rank projection back into lobby `RoomPlayer.ranking`
  so the following `GrCommandStart` carries the previous race's start grid
- [x] settlement-owned next-lobby master selection: highest remaining human in
  individual races, or the highest remaining human on the server-decided
  winning team; AI, observers, and departed racers are ineligible
- [x] idempotent per-player reward persistence and retry/dead-letter state
- [x] graceful/forced shutdown durability and visibility barriers
- [x] bounded read-only native RHO5 emblem-definition extraction
- [x] bounded read-only legacy Rh-layer-1.0/1.1 `item.rho` and
  `track_common.rho` extraction, authenticated block decoding, BML parsing,
  and strict bounded portable XML override
- [x] exhaustive MyRoom dispatch plus authorized/logged/no-reply consumption
  of genuinely unclassified identity packets
- [ ] stock-client RequestEmblems/main-emblem E2E
- [x] generation-bound P2P-port report, durable presentation fallback, and
  authenticated runtime cache refresh
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
- [x] immutable Korean P5136 flying-pet physics table applied to room and
  single-player kart-spec replies; normal-pet defense remains a client-side
  `Set_Pet`/type-11 relay with no duplicate server probability roll
- [x] deliberate P5136 exclusion of later-version V2 Exceed sidecars:
  `Parts12Data.json` and `Level12Data.json` are per-account C# server files,
  not client data, and their login/item streams are byte-incompatible with
  Korean P5136
- [x] atomic time-attack start/finish persistence, checked economy arithmetic,
  and one-shot finish replay protection
- [x] bounded complete telemetry codecs for every retained report shape,
  including counted career state, the empty booster signal, the four-word
  in-game heartbeat, neutral client-frame metrics, and the named
  `PcRideSwithInfoPacket` map/vector/eight-aggregate container
- [x] diagnostic-only fixed state-2 `GameControlPacket` finish snapshot
  parsing (session envelope, 54-byte subobject, 235-byte effective KartSpec,
  length-prefixed 22-byte shared-object payload, participant slots, global
  metric, and terminal state); only the server-owned finish transition remains
  authoritative
- [x] exact unsigned `LoRqUseItemPacket` category/id/remaining-quantity words
  and raw GameSlot type-10/type-16 byte/effect-code labels without inventing
  boolean or barricade-only semantics
- [x] native-length type-17 `GameKartPacket`/`GameKartQuadPacket` fallback
  movement relay, exact empty-body type-13 no-reply consumption, and exact
  evidence-gated type-5/7/8 team-flag transition codecs
- [x] authoritative type-1/type-2 item pickup probability roll, exact response
  synthesis, strict captured request/token validation, replay/rate admission,
  sender-inclusive atomic room broadcast in game types 2/4, and the stock
  2-8-racer Top/High/Middle/Low matrix over frozen humans plus AI racers
- [x] bounded direct `kart.rho`/`item.rho`/RHO5 catalog reconstruction with no
  generated XML sidecar, plus compatibility-only `KartCatalog.xml` parsing and
  exactly-once frozen-kart item acquisition remapping, including Gigantes V1
  `5 -> 103`, `7/127 -> 99`, Sebek V1's eight 25% `-> goldShield(36)`
  paths, and separate partial-probability rolls
- [x] conservative category-3 auto-grant admission requiring a resolved name,
  `BodyParam`, and effective `model.1s` folder while excluding development-like
  rows; quarantined catalog IDs remain available only through exact numeric GUI
  lookup and a nickname-scoped serial-2+ manual grant
- [ ] actor-owned live race position and track-box spawn/collision authority
- [ ] evidence-backed held-slot/type-10 use and type-11 reaction side effects;
  common type-12 object lifecycle admission is actor-owned, while
  class-specific state 0/4+ meanings remain evidence-gated
- [x] explicit scope exclusion for Lucci world objects, bonus-item world
  objects, and team flags (`GameSlot` types 4/5/6/7/8); their strict codecs
  remain diagnostic/evidence boundaries, but no actor-owned spawn registry or
  gameplay support is required for this port
- [ ] actor-owned directional `PcGameRequestRelay` / `GameRelayBroadcasting`
  pairing after a two-client capture fixes its output slot transformation
- [ ] authoritative actor-owned club repository and atomic membership/create/join/rename flow;
  the implemented local Club UI hand-off is intentionally not a fabricated
  global club service
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

- [x] reproducible native-client consumer reachability census: 2,886
  RTTI-backed serialized classes, 534 consumer-reachable classes, generated
  typed-cast adapters followed to their actual callers, and an explicit
  event/social/commerce-excluded LAN baseline of 63 outer packet classes plus
  the 80 emitted type-12 item-operation schemas; see
  [CLIENT_CONSUMER_AUDIT.md](CLIENT_CONSUMER_AUDIT.md)
- [x] dependency-isolated `p5136-client-oracle` crate with no normal
  `p5136-core` dependency or shared packet reader/hash/layout constants
- [x] native-client exact layout/consumption oracle with selected recovered
  semantics for `GameResultPacket`, plus exact native codec/consumer coverage
  for `GameNextStagePacket`; immutable IDB-derived raw fixtures, truncation,
  suffix-drift, and rejection of the malformed C# 217-byte result record are
  covered while opaque result fields remain explicitly unmodeled
- [x] exact native `PqStartCollectRecord` hash-only request and
  `PrStartCollectRecord` hash-plus-raw-boolean codec, common-`GameStage`
  consumer truthiness/side-effect oracle, plus a server reply policy tied to
  the authenticated rider's category-12 replay-camera equipment
- [x] exact native 24-byte `PcReportUserCollectedRecord` finish report and
  hash-only `PqReportGameCollectedRecord`; both are authenticated,
  non-authoritative diagnostic no-reply inputs, matching retained C# fallthrough
- [x] forward-compatible unknown authenticated packet policy: preserve the
  bounded raw logical receive record and hash/identity warning, emit no reply,
  and keep the session alive; classified malformed packets remain typed errors
- [x] evidence-graded structural oracle for ceremony ordering, room
  list/admission/initial state, login/migration, and read-only club consumer
  branches; C#-golden/live-trace cases are not labeled IDB-exact
- [x] protocol-visible hierarchical client FSM for login/reconnect, room
  admission, self-contained command-start, UDP readiness, ordinary speed/item
  race control, settlement, podium ordering, leave, and disconnect; native
  state effects and deployed-order assumptions are recorded separately in
  [CLIENT_PROTOCOL_FSM.md](CLIENT_PROTOCOL_FSM.md)
- [x] executable independent `ItemClientFsm` for the fixed 149-branch item
  consumer corpus (74 local, 70 deferred, zero immediate, 5 unknown), plus
  the 15 later boss/controller/Course branches; lifecycle observation,
  deferred scheduler markers, and transactional malformed-input rejection are
  tested without synthesizing unproven outbound packets
- [x] production-server cross-wire gate for all original 149 branches: each
  fixture passes the real `GameSlot` decoder and the same item-to-registry
  admission mapping used by the World actor, then retains byte-exact relay
  output. The pinned server census is 88 tracked, 61 relay-only/untracked, and
  zero fresh-registry suppressions. Representative World/network tests remain
  the integration proof; this exhaustive synthetic gate does not claim that
  every rare native client animation or controller condition was live-fired
- [x] `GopAngel` state 2 re-audited against the pinned IDB: exact
  `token@16/source@20/target@24`, native phase 2, and repeatable defense impact
  are modeled; the shared resolver inserts the protected kart into the
  attack-owned processed-target container rather than removing the timed
  Angel effect, and the client's stale phase-argument member is
  retained as a documented native quirk instead of making the branch unknown
- [x] post-Angel-correction release validation: complete workspace tests and
  warnings-as-errors Clippy pass; the fixed release binary starts all four
  transports against the real `KartRider_5136` catalog/Data tree, messenger
  probing succeeds, and the installed `item.rho` yields the expected 14/18
  rows with combined weights 400/410
- [x] IDB-reconstructed local podium scheduler for individual/team final
  stages, including strict timer boundaries, animation-completion gates,
  flag-`0x80` manual confirmation, virtual-slot-103 handoff, and
  `GameReadyStage`/`ObserverReadyStage` selection; executable oracle tests are
  independent of the production server
- [ ] every supported packet serializer has a C#-derived golden fixture
- [x] C# and Rust synthetic PIN fixture compatibility
- [ ] differential request/response harness passes for the supported flow
- [ ] every supported server-to-client serializer is covered by an
  independently reconstructed client consumer; current oracle coverage is a
  high-risk first slice (12 of the 63 core outer consumers, at mixed evidence
  grades), not global semantic compatibility
- [ ] movement envelope, tick wrap, and fallback are capture-verified
- [ ] production AI start and nonzero AI-master behavior are capture-verified
- [x] all 1,471 retained TCP GameSlot records cross the strict parser
- [x] all 174 newest type-17 quad snapshots cross the native flag-derived
  length rule and sender-excluding peer-mask relay codec
- [ ] remaining class-specific type-12 source/target/effect meanings and
  multiplayer type-16 routing are capture-verified; 79 direct-writer classes
  now expose recovered field semantics (75 with named lifecycle transitions), while unknown
  and bounded fallback bodies do not acquire invented authority;
  scope-excluded types 4/5/6/7/8 and the three explicitly deferred gameplay
  headings are not a completion gate
- [ ] Windows, macOS, and Linux CI pass
- [ ] native Windows connector launches a stock P5136 client
- [ ] Wine, CrossOver, or Sikarugir connector launches the same client
- [ ] two clients login, migrate, join a room, race, persist, and shut down
- [x] opt-in external corpus audit parses all 19,496 retained inbound records,
  resolves all 100 hashes, fully parses all former 28 gap families, strictly
  parses all 1,471 TCP GameSlot records, and decodes every retained routed
  UDP/P2P packet
- [x] build, runtime, connector, and resume documentation exists

Detailed retained-log coverage:
[CAPTURED_PACKET_COVERAGE.md](CAPTURED_PACKET_COVERAGE.md)
