# Rust port status and resumable handoff

Last updated: 2026-08-10

This is the authoritative resume document for the independent Rust port. The
short feature ledger is in [PORTING.md](PORTING.md).

## 2026-08-10 v0.2.5 RHO abilities, inventory, licenses, and offline profiles

- Added live/offline nickname administration for license progression, manual
  PRO mission-set selection, and a client-default-or-S0-S8 time-attack physics
  override under the consolidated Server management GUI tab.
- Rider lookup and MyRoom visits now resolve persisted offline profiles instead
  of treating every disconnected rider as nonexistent.
- Kart grants expose three vertical Floater selectors with the complete
  RHO-verified speed/item effect names, while X/V1 parts compatibility is
  derived from each client BodyParam instead of a fixed kart list.

- Replaced the guessed item-Floater labels with all 20 exact group meanings
  recovered from `zeta_/kr/enchant/desc.xml`. The encoded value is confirmed
  by `enchant.xml` as `groupId * 100 + tuneId`.
- The item-box path now uses each Tune's original probability and full source
  set. This includes timed/rolling water-bomb variants, big banana, and animal
  booster; the former global “chance per displayed level” setting was removed.
- Added a stock-catalog startup sentinel for both Pharaoh HT bodies (498/585):
  booster and the other seven listed sources transform to Gold Shield 36 at
  the RHO-defined 20%, with pass/miss regression coverage.
- Gold Booster is a separate client-data path from Gold Shield. The direct
  loader now merges the base and Korean `slot/animalBooster` tables into 133
  kart rules and maps an awarded normal booster 6 to animal booster 31 using
  each row's exact probability. The client then selects the kart-specific
  visual/effect; the icon-197 sentinels include Pharaoh HT 498/585 and Bastet X
  1139. Ordinary `TransformByKart` acquisition, equipped Floater acquisition,
  and special-booster fallback are evaluated in that order and never stack.
- Corrected the ownership audit for the 78 `firing2Gain` and 150 `fired2Gain`
  rows. `KartRiderU.exe` loads both tables into its item manager
  (`sub_79FB30`, offset `+0x1400`; `sub_79F5F0`, offset `+0x1408`) with the
  source item, target item, probability, game type, and firing-step fields.
  Confirmed runtime behaviour (including Hongryeon V1 magnet-to-siren) works
  without any Rust-side reward synthesis. GameSlot type 10 use and type 11
  reaction reports therefore remain byte-exact client-owned relay paths. The
  older C# `AddItemSkill`/`AttackedSkill` extra award packets are intentionally
  not ported because they can double-grant an already client-resolved effect.
- Audited all 14 quarantined kart rows. Restored Kartneck/Kartneck X, whose
  released internal names contain `dummyBox`, and the three Boxter HT variants
  that share the `boxter7` model. Four rows lacking a Korean/default BodyParam
  and five explicit dummy/test rows remain excluded from implicit grants.

## 2026-08-05 v0.1.5 room control and customization compatibility

- Corrected the P5136 plant snapshot's engine/wheel/handle/kit category order
  and the kart-level probability-query result field that previously terminated
  the client when it was confused with a success percentage.
- Added the exact room-title/password change codec and atomic master-only room
  update. A changed S0-S8 title token selects the next race's physics variant
  without rewriting the existing channel/session speed byte.
- Rider and macro chat now retain C# room-membership scope through loading,
  racing, and settlement. Racers and observers can send; observer player IDs
  remain 8..15, sender exclusion is preserved, and nonzero teams retain their
  team filter while team zero broadcasts to every peer.
- Settlement now chooses the next lobby master from the highest-ranked
  remaining human in individual races, or the highest-ranked remaining human
  on the server-decided winning team. AI, observers, and departed racers are
  not eligible.
- The fixed-path Windows release candidate is
  `target/p5136-finish-kart-abilities/release/p5136.exe` (18,356,736 bytes,
  SHA-256
  `4293B8F1245CB28277677A0E6263356FC8917626B65686A42649B2A378576376`).
  `--version` reports `p5136 0.1.5`. Workspace all-target tests, warning-denying
  Clippy, formatting, `git diff --check`, and the release build pass.

## 2026-08-04 lobby balance, channel fallback, GUI settings memory, and next grid

- Team-room admission now counts active human and actor-owned AI racers by team
  and places the next human in the smaller team. A tie selects Blue, producing
  Blue/Red/Blue/Red from an empty room while retaining physical slots 0..3 for
  Blue and 4..7 for Red. If the preferred half is physically unavailable, the
  other half is used without violating the slot/team mapping.
- Room creation derives its advertised `speed_type` from the channel whenever
  the title has no standalone S0-S8 token. Individual/team infinite-booster
  channels 23/24 use the stock S4 preset, item channels use S8, and speed/newbie
  channels use S7. S6 remains available only as an explicit event preset.
  The race-start physics lookup receives that same fallback, while game types
  2/4 still select their individual/team item matrix rows. Thus channel/session
  metadata and the emitted 235-byte physics block no longer disagree.
- The native GUI now enables eframe desktop persistence and stores a bounded
  (2 MiB maximum) versioned snapshot of all server/connector input fields:
  addresses, ports, paths, nickname, runner/Wine/CrossOver/Sikarugir settings,
  advanced limits, checkboxes, edited item-probability tables, and random-track
  overrides. Runtime state, logs, loaded catalogs, and search results remain
  transient. GUI server startup now requires a client root, `Profile`, or
  `Data` selection and resolves it to an existing `Data` folder. A persistent
  footer displays the compiled Cargo package version.
- Settlement finalization sorts every still-present human by the server's final
  rank and writes a compact 0-based lobby `RoomPlayer.ranking` before clearing
  race state. The next `RoomSlotData` embedded in `GrCommandStart` therefore
  carries the prior race order (including DNF ordering) instead of the original
  join order, without leaving an empty grid position when a racer disconnected.
- Regression coverage includes alternating team admission and physical slots,
  true team-full rejection, S4 infinite/S8 item channel creation, matrix
  fallback selection, GUI persistence and required-path rejection, and exact
  next-start `RoomPlayer.ranking` serialization.
- The refreshed fixed-path release is
  `target/p5136-finish-kart-abilities/release/p5136.exe` (18,347,008 bytes,
  SHA-256
  `F1A8093D794BAD57CCD1B98C1F3FD860EE46EA338544ACEB5BE04290B20BF86F`).
  Workspace tests with all features, warning-denying workspace Clippy, release
  build, `--help` smoke, formatting, and diff checks pass at this checkpoint.

## 2026-08-04 conservative kart inventory admission

- Replaced blanket category-3 inventory publication with a conservative
  resource check. Automatic serial-1 ownership now requires a nonempty resolved
  internal name, a parsed KR/default `BodyParam`, and an actual `model.1s` in
  the effective folder (`addModelFolder` when present, otherwise the kart's own
  folder). Internal/display names that look like dummy, test, NPC, or AI rows
  are also excluded.
- The stock P5136 client resolves 1,296 shop karts: 1,282 remain automatic and
  14 are quarantined (`199, 312, 323, 352, 657, 658, 659, 744, 745, 746, 795,
  814, 886, 1167`). Quarantine removes only implicit serial-1 ownership and
  ordinary name search; it does not discard catalog identity.
- An operator who has verified a conservative false negative can enter its
  exact decimal ID in the GUI. The result is marked `[수동 확인]`, requires an
  explicit selection, and creates only a nickname-scoped serial-2+ grant.
  Equipment validation and inventory serialization accept that exact persisted
  pair but do not silently restore serial 1 for the same quarantined kart.
- Structural parser tests cover the bounded `autoGrant` attribute, name search
  hiding versus exact-ID discovery, durable manual grant allocation, and manual
  equipment ownership. Loader tests cover direct and shared model folders,
  missing spec/model rejection, development-name rejection, and the exact
  stock-data 1,282/14 split.

## 2026-08-04 Floater/TuneData and Black-H physics

- Reconstructed all four fixed-width P5136 Floater request/reply pairs from the
  client consumers: socket creation, activation kit, protection spanner, and
  socket reset. Requests now distinguish `consumable_id` from the required
  category-3 kart type. Every failure returns the complete decoder shape, so a
  missing state or rejected request cannot leave the client UI busy forever.
- Corrected two retained C# incompatibilities instead of cloning them. The
  socket reply writes native `record_state=0` rather than duplicating the kart
  serial and sends the same `[Tune1,Tune2,Tune3,Slot1,Count1,Slot2,Count2]`
  state that is persisted. Reset atomically computes and stores the post-reset
  state, then returns that exact state so the client cache cannot diverge from
  the server. Protection result codes 2/3/4 retain native kart-unavailable,
  socket-missing, and already-protected meanings.
- Added bounded, BOM-tolerant, last-wins `(kart_id, normalized_serial)` loading
  for `TuneData.json`. Mutations use the existing profile-root lease, no-follow
  rider capability, temporary-file atomic replace, and explicit
  committed-but-directory-sync-uncertain outcome. Unknown JSON fields survive
  rewrites; malformed or duplicate tune codes isolate only the Tune stream at
  login but strictly reject a Tune mutation without replacing the file.
- The friend-server policy does not deduct Floater consumables. It still checks
  target kart ownership, category, selector, slot range, nonempty target slot,
  and duplicate protection. Activation selector 4 uses the recovered item pool,
  selector 6 the speed pool, and selector 5 installs the fixed Black set
  `[603,703,903]`. Protection counts and reset survival match native behavior.
- Tune preload is emitted before plant, kart-level, parts, and ordinary rider
  items using the exact 12-`i16` record and six-vector `LoRpGetRiderExcDataPacket`
  layout. Duplicate-kart serial allocation also reserves orphaned Tune records,
  preventing a newly granted copy from inheriting another copy's Floater state.
- Ported the exact C# server-physics contributions for codes 103 through 903.
  Black-H therefore contributes start-booster time `+800`, transform
  acceleration `+0.018`, and drift escape force `+210` to the matching
  `(kart_id,serial)` room physics snapshot. Item codes 10103 through 12003 remain
  valid persisted client semantics but deliberately add no invented server
  speed physics, matching the original C# path.
- Added codec golden/bounds tests, strict/lenient persistence tests, protection
  and reset transition tests, inventory-preload ordering tests, and a session
  workflow covering rejected category, missing/duplicate protection, socket →
  Black activation → protection → reset, main-profile non-consumption, reconnect
  preload, and final room-physics bytes.

## 2026-08-04 plant parts and legacy kart enhancement

- Reconstructed the stock `PqEquipTuningExPacket` as two five-`i16`
  descriptors: the newly equipped plant part and the displaced part. The
  retained shorter C# fixture remains accepted, but arbitrary trailing sizes
  fail closed. `PrEquipTuningPacket` failure now uses the native decoder's
  fixed `u8 + 5*i16` body instead of the truncated C# `i32` response.
- Plant changes are written through the profile-root lease and no-follow rider
  directory capability, then atomically published to `PlantData.json` before a
  success reply. The refreshed exception cache is bound to the authenticated
  session and room participant; an exact kart ID and normalized serial match is
  required before any contribution is applied.
- Ported all 91 recovered plant performance entries and their C# mode filters.
  Race participants now carry a game-type-by-S0..S7 physics matrix: game types
  1 and 3 use speed rules, while 2 and 4 use individual/team item rules. Loading
  and Running still retain their frozen race-start block.
- Added exact native codecs for probability, upgrade, point allocation, point
  clear, and special-slot requests/replies. The stock upgrade request's final
  `i32 cost`, all nine state shorts including `Effect`, and the three-short
  consumed-material descriptor are explicitly modeled. Startup inventory sends
  the C#-order plant -> level -> parts exception streams.
- Enhancement is intentionally friend-server behavior: both target and donor
  ownership are checked, probability is reported as 100, result is success,
  the request cost is parsed but not deducted, balances are echoed unchanged,
  and the donor descriptor is all zero. The donor `GrantedKarts` entry is never
  removed. First upgrade creates grade 5 with 35 free points; repeated upgrades
  preserve allocation. Point mutations are bounded per slot to 0..10 and to a
  total of 35 before `LevelData.json` is atomically replaced.
- Login, channel migration, `PqGetRider`, equipment changes, and time-attack/
  room physics all use the same cached plant/level sidecars. Invalid persisted
  levels are rejected before array lookup. Optional plant/level/global-parts/
  rider-parts streams now fail independently during login and `PqGetRider`: the
  damaged stream is empty and logged while the authenticated session and other
  valid streams survive. Mutations remain strict for the one sidecar they
  change, so a malformed unrelated Level/Parts file no longer turns a committed
  plant write into a false failure.
- Plant and level records are normalized and deduplicated last-wins by
  `(kart_id, serial)`, matching the retained C# compatibility loader and keeping
  preload identical to the record used for physics. Duplicate-kart serial
  allocation reserves orphaned `LevelData.json` records as well as plant and
  parts records. A stale grade-zero level placeholder is reset to a real grade-5
  35-point state; a repeated grade-5 upgrade preserves allocation and normalizes
  remaining points. Signed deltas support bounded point redistribution.
- Capability mutations distinguish pre-publish failure from a rename that was
  committed but whose final directory durability check failed. The latter is
  logged and treated as committed, including in additive point updates, so a
  client retry cannot double-apply a mutation that is already visible on disk.
- Added golden codec tests, sidecar validation tests, exact serial/mode physics
  tests, and a session-handler workflow proving 100% upgrade, donor
  non-consumption, point persistence, reconnect preload, and final physics
  composition. Fault tests cover isolated malformed-sidecar preload and
  post-rename durability uncertainty. Core/profile/server library suites pass;
  a stock-client GUI enhancement smoke test remains the final hardware E2E gate.

## 2026-08-04 eight-client UDP relay mock

- Added an ignored, duration-configurable eight-client UDP stress test. Its
  default duration is 120 seconds. Every burst concurrently schedules movement
  and `PqUdpEcho` requests with deterministic per-client latency plus jitter;
  the first request is delayed behind the second so the server must observe
  genuine ingress-order reversals. Movement payloads also include deliberately
  older sender-local ticks. Replies are checked as an unordered exact multiset,
  so arrival order is unconstrained while any missing, duplicate, wrong-account,
  stale-route, or byte-changed datagram fails the test.
- The full 120-second run passed on 2026-08-04: 8,870 movement requests, 4,246
  ping echoes, 2,257 deliberately stale ticks, 4,910 observed ingress-order
  reversals, and 62,090 exact seven-recipient movement relays. Per-sender
  movement counts were `[1084, 1056, 1110, 1149, 1120, 1131, 1075, 1145]`.
  Re-run it with:

  ```powershell
  $env:P5136_UDP_STRESS_SECONDS='120'
  cargo test -p p5136-server --test udp_runtime_standalone eight_clients_sustain_jittered_exact_relay_for_configured_duration --locked -- --ignored --exact --nocapture
  ```

- Added a real-socket standalone UDP test with eight independently bound
  localhost clients. Every client sends one movement `GameSlotPacket`; the
  runtime excludes the sender and publishes the exact body to the other seven,
  covering 56 relayed datagrams in total. The test also proves independent
  sender ticks, per-recipient account routing, no sender echo, and use of each
  recipient's latest observed route hash.
- Added an eight-human World race fixture. After all seven guests become ready
  and the room freezes its race roster, every possible sender resolves exactly
  the other seven identities in stable slot order. Player IDs 0 through 7 and
  the running-room boundary are asserted explicitly.
- This is executable server/mock coverage, not a claim that eight stock clients
  have completed the live LAN race and podium cycle. That remains a separate
  hardware E2E gate.

## 2026-08-04 deployment UI, random tracks, and room-title physics

- Rewrote README as the Korean deployment guide. It now leads with GUI startup,
  client-path discovery, LAN ports, logs, IP/domain constraints, item-table
  provenance, random tracks, and S0-S8 title physics instead of the historical
  implementation narrative.
- Localized the native GUI and added a LAN IPv4 discovery/apply control.
  Loopback, link-local, unspecified, and multicast addresses are excluded;
  physical adapters are preferred before RFC1918 subnet ranking. Common WSL,
  Hyper-V, VMware, VirtualBox, VPN, and Tailscale names are demoted. The chosen
  literal is applied to both bind and advertised address; unusable advertised
  values are rejected. Hostnames remain rejected because the client endpoint
  codec carries exactly four IPv4 bytes. Korean system-font discovery now
  covers Windows, macOS, and common Linux Noto/Nanum installations.
- Added an explicit macOS `Sikarugir` connector backend. GUI and CLI accept a
  preconfigured wrapper `.app`, launch it through `/usr/bin/open`, preserve the
  selected game directory for connector-managed PIN/XML files, and support an
  optional alternate game executable. Preflight requires both the game EXE and
  wrapper app directory to exist. `MACOS_SIKARUGIR.md` records the prefix
  symlink, Nexon `RootPath`, working-directory batch file, optional CoreAudio
  workaround, CLI invocation, and log verification steps without bundling
  Sikarugir, the game, or a wrapper.
- Automatic item probabilities are now verified on the GUI start boundary.
  The selected client Data directory is actually parsed, the individual/team
  row counts and source path are shown, and a parse failure prevents startup.
  The exact verified snapshot is passed to that server start attempt, avoiding
  a second mutable-file read between the GUI report and runtime application.
- Extended the bounded no-unsafe legacy RHO reader to `Rh layer spec 1.0`.
  Version 1.0 uses the data cipher for its header and the archived 32-byte XOR
  key for block metadata; 1.1 retains the existing header-info path. An opt-in
  smoke test successfully reads the real KR `Data/track_common.rho`.
- Ported the C# random-track catalog semantics for speed/item selectors
  0,1,3..8,23,30,33,40, including reverse/crazy/new/league/speed-only pools,
  validated GUI overrides, basic-AI preference with empty fallback, and
  actor-owned per-room used/last history. Selection uses process entropy like
  C# `Random.Shared`; the requested selector stays in room state while one
  concrete hash is frozen for the start transaction. Nonempty overrides now
  require a client Data catalog instead of being silently ignored.
- The random-track GUI now mirrors the C# checked-list workflow: every selected
  pool displays its effective client defaults immediately, individual checks
  create a manual override lazily, and select-all, clear-all, and restore-client-
  defaults actions are available. An empty manual selection remains visible for
  correction but is rejected before server startup.
- Ported the exact C# ASCII-boundary S0-S8 parser, protocol-byte mapping
  `[3,0,1,2,4,5,6,7,8]`, and all modern speed-base fields. S8 preserves the
  C# integrated-item `DragFactor=0.74` distinction from S7. Each participant
  carries nine prebuilt equipment-specific 235-byte physics blocks; World
  selects the title variant at race start and refreshes all nine variants
  after durable equipment changes. `TESTS1ROOM`, `S10`, and S9 do not trigger
  an override.

## Scope and source policy

- Rust repository:
  `C:\Users\drash\Documents\kartrider\kartrider_p5136_rust`
- C# behavioral reference:
  `C:\Users\drash\Documents\kartrider\KartRider-P5136`
- C# audit:
  `C:\Users\drash\Documents\kartrider\KartRider-P5136\P5136_STABILITY_AUDIT.md`
- `PhysicsSim`, captures, scratch output, and historical analysis are separate
  projects/artifacts and must not be copied into this repository.
- The C# repository is read-only for the rest of this port. A defect found in
  C# is a Rust requirement or a capture question; it is not authorization to
  edit C#.
- C# is a protocol and product-intent reference, not a bug-for-bug
  specification. Preserve client-visible wire behavior, but do not reproduce
  data loss, races, spoofing, partial publication, or unsafe failure handling.
- Rust and C# have independent Git histories. Never copy a dirty tree from one
  repository into the other.

Branch: `main`

State: retained-corpus implementation complete, independently reviewed, and
release-validated. Server startup against the real stock catalog/Data tree is
now smoke-validated. The next live action is a stock-client login followed by
the two-machine LAN multiplayer E2E, including a speed-team start that emits
`PqStartCollectRecord` and two or more defended impacts during one timed Angel
activation. Protocol coverage is not presented as proof of those live client
gates.

Current implementation checkpoint:
uncommitted retained-corpus completion work on `main` +
`406a9e9 Handle captured Koin queries and scenarios` +
`1cdb3aa Complete login initialization query coverage` +
`4c806af Complete post-rider startup query replies` +
`533df45 Port lease-bound favorite sidecar migration` +
`bb84027 Harden favorite sidecar import bounds` +
`9b26159 Add GUI server controls` +
`c67426c Document LAN E2E setup` +
`8be036e Add bounded packet diagnostics`

## Current Rust checkpoint

### Native client consumer census (2026-08-03)

A reproducible IDAPython scan now inventories the client from its typed packet
consumers rather than deriving expected packets from the Rust or C# server.
The scan follows the generated `Packet -> T` cast adapters to their callers;
without that second hop, normal login, room, and ceremony consumers disappear
from a direct-RTTI-xref report.  The external analysis artifact contains 2,886
RTTI-backed serialized classes, 534 consumer-reachable classes, 749 adapter
callers, 212 direct cast sites, and 88 reachable nested `Gop*` classes.

The LAN multiplayer completion scope is now explicit in
`CLIENT_CONSUMER_AUDIT.md`: 63 outer consumers across bootstrap/rider state,
channel/room state, Game/P2P topology, speed/item race lifecycle, and
race-affecting loadout mutation, plus all 80 strictly decoded type-12 item
operation schemas.  Event, social, commerce, test-server, and unrelated
minigame consumers remain in the raw census but are not completion gates;
specialized gameplay modes are parked and can be promoted later.

Lucci world objects, bonus-item world objects, and team flags are now an
explicit product-scope exclusion. Their `GameSlot` types 4/5/6/7/8 retain
strict bounded parsing and diagnostics, but actor-owned spawn/collection/flag
state, relay release, and stock-client E2E are not port-completion gates.

Strictly decoded type-12 item operations now pass through a bounded
race-epoch object registry before relay. The registry binds object ID to the
operation/base class and original installer generation, but no longer treats
state 1/2/3 as a protocol-wide install/hit/remove enum. A class-specific
semantic decoder drives installation, impact, resolution, retargeting,
respawn, and terminal admission. Duplicate impacts compare the exact state,
transition token and target, so a valid later phase on the same object is not
suppressed. The retained barricade trace is respected: player 1 may report a
state-2 impact while nested owner 0 remains the installer; state 3 is a
post-impact transition and state 4 is terminal. Every outbound permit is
reserved before the registry commit, so backpressure cannot publish or commit
a partial authoritative transition.

### Type-12 item source/target/state semantics (2026-08-03)

The pinned IDB was re-probed through each `GoItem*` vtable offset `+0x24` and
joined to both the native producer and `Gop*` writer. The resulting ledger is
`analysis/P5136_ITEM_OPERATION_SEMANTICS.md`; the reproducible probe and full
output are `analysis/ida_5136_type12_semantics_probe.py` and
`analysis/ida_5136_type12_semantics_probe_v5.log`.

Rust now has class-specific field decoders for 79 of the 80 direct-writer
classes. The original fifteen are Barricade, Banana, Mine, Rocket, CokeRocket,
GoldRocket, DinoClawRocket, TigerRocket, Snowman, SuperMag, Waterbomb,
Waterfly, InfectedWaterfly, SnowWaterfly, and WaterbombFly. The next passes add
Cokebomb, Snowbomb, InfectedBomb, RollingCokebomb, RollingInfectedbomb,
Rollingbomb, WaterMine, TimeMine, ItemTimeFlybomb, TimeCokebomb,
TimeInfectedBomb, TimeSnowbomb, Timebomb, BigTimebomb, AreaUfo, MovingUfo, Ufo,
Shield, SpecialShield, LockdownRocket, Thunderbolt, ForceZone, Oil, Silence,
Siren, SirenShield, SpecialSmall, Cloud, Cloud2, Magnet, SpeedDown, Devil,
MqDevil, and NewDevil. The fourth pass adds Angel, Emp, Ghost, Icefly,
Scanning, SlotLock, SpecialSiren, SpaceCraft, and StraightRocket. The fifth
pass adds Balloon, HeadBand, Dynamite, Hammer, Press, RobotBeam, TombStone,
Block, BoundWall, Cube, CubeForBoss, EventObject, GiantTalisman,
WitchUnionMagic, and TargetKart. The sixth pass adds BossPrison, BoundRoad,
Course, Falling, and Piratebomb. Seventy-five classes yield at least one named
lifecycle meaning plus native phase, source object, target object,
transition token, variant, and evidence grade. BigTimebomb exposes its exact
`variant:u8@12 | native phase:u32@13 | token@17 | target@21 | source@25`
consumer binding, conditional on both actor lookups succeeding. Producer call
sites constrain its native phase to activation 0, ordinary/team-routed impact
2/3, and SpecialShield resolution 4.
Angel state 0 is a proven team-effect activation. A targeted IDB follow-up
closed state 2 as a repeatable, non-terminal defense-impact branch: its 28-byte
body is `token@16, source@20, target@24`, and the consumer binds both actors and
advances native phase 2. The source-only producer does not explicitly overwrite
the target member, and the consumer normalizes object member +40 but passes the
stale state-0 member +28 as the phase argument. Those native quirks are now
documented without discarding the proven wire roles. A second lifetime audit
shows that the shared resolver's `sub_4E83E0` is a container-insertion wrapper:
it records the protected kart in the attack object's processed-target set and
does not remove Angel from the kart's timed active-effect collection. The same
evidence corrects ordinary Shield state 2 from removal to defense impact.
StraightRocket state 1 is a proven phase-1 launch. Its concrete consumer
accepts writer-supported states 2/3 but performs no class binding, phase call,
or helper action, so both are explicit `NoClientAction` branches rather than
unknown side effects. RobotBeam/TombStone state 2
and Block state 2 likewise read a source member which their writer does not
serialize; Rust records only the token/target/native phase present on the
wire. EventObject and Course raw 12 are object IDs rather than lifecycle
states. CubeForBoss's exact writer lengths are 77/69, not 73/65: the earlier
census omitted its four-byte class dword.
The sixth pass used the corrected primary-vtable RTTI scan in
`analysis/ida_5136_five_unknown_occurrence_probe.py`. BossPrison is emitted by
GoBossKart target selection; BoundRoad by BombRobot/MechanicBall timer and lane
patterns; Falling by PetitMeteor/SpaceBombing lane patterns; and Piratebomb by
controller branch 12 after per-entry target filtering. Course carries a
subject ID, counted UTF-16 `goal`/`Ev_*` event name, and trailing token; its
concrete peer consumer intentionally releases it without a gameplay action.
This establishes client occurrence and FSM meaning, not authority for Rust to
simulate boss AI, controller timers, or course collision physics.

### Ordinary `_R/_I` course-gimmick census (2026-08-04)

The stock client's complete ordinary-track archive set was scanned directly:
218 `_Rnn/_Inn` archives (94 speed, 124 item), 534 `.1s` scenes, and no archive
read failures. The resulting external ledger is
`analysis/P5136_RI_TRACK_GIMMICK_AUDIT.md`.

Base scenes explicitly place `banana`, `itemCube`, `mine`, `waterMine`,
`event`, `obstacle`, and `dummy` object types. The first four instantiate the
same GoItem classes already covered by exact `GopBanana`, `GopCube`,
`GopMine`, and `GopWaterMine` validation/semantics. `event` instantiates
`GoItemEventObject` and confirms real ordinary-track occurrence, but its
fixed notification body still is not a lifecycle FSM. `obstacle` and `dummy`
are local scene objects. The course dispatcher independently treats
warp/snow/rain/rail/lens-flare/flash/shake/wave/pet controls as local client
state; only `event*` sends `GopCourse`, whose peer consumer is a no-op.

Therefore this census does not add a server-side track-physics simulator or
new permissive packet class. It confirms that the current exact shared-object
schemas cover the ordinary static network effects and that BossPrison,
BoundRoad, Falling, and Piratebomb remain special-controller scope.

The semantics decoder is now crate-private and accepts a private-field
`ValidatedItemOperation` capability created only by exact schema validation.
This makes the length/state precondition a Rust type boundary rather than a
public-API documentation convention.
Important corrections include
SuperMag raw offsets 16/20/24 as token/source/target, compact Waterbomb offsets
16/20/25 as token/target/source, Mine state 3 as an explicit client no-op and
state 6 as respawn, Rocket states 4/5 as retarget/removal rather than a
generic numeric transition, TimeMine's conditional target flag, and timed
bomb state-3/4 same-ID source/target binding. The only fully unresolved exact
schema is the deliberately scope-excluded `GopLucci`; it may relay through the
existing bounded path but does not create an authoritative object record
merely because its state number is 0 or 1. The registry preserves Barricade
initialize -> place and Mine removal ->
respawn, keeps unresolved/no-op packets from overwriting authoritative hit
fingerprints, suppresses unsupported post-terminal updates, and refuses to
mint unseen tombstones from consumer-only terminal evidence. WaterbombFly
state 6 records its phase-5-or-reset branch as conditional. A 15-class table
now uses IDB-ledger literal pairs, class names, state offsets, and field
offsets to exercise exact outer GameSlot parsing, distinct semantic fields,
and registry admission together. A separate real Mine wire sequence covers
state 1 -> 5 -> 6 -> 2 through parse, commit, reactivation, and later impact.
An additional 29-state literal table hard-codes pair, raw length, state offset,
and actor/variant offsets without consulting the production schema, then runs
each frame through outer GameSlot parsing and registry admission. A World test
also proves that `StartRoom` replaces `RaceProgress` and releases prior-race
registry capacity rather than accumulating the 1,024-object bound across the
server lifetime. The RTTI/caller recovery is reproducible via
`analysis/ida_5136_type12_runtime_consumer_probe.py` and its `.log` output.
The shield/UFO/Lockdown/Thunderbolt pass adds a separate 23-state literal
wire-to-registry table. Thunderbolt's counted target vector uses a validated
raw-range descriptor and is decoded on demand; it is never reduced to a
single arbitrary target.
The ordinary-effect pass adds a separate 27-state literal wire-to-registry
table. Oil's zero success byte selects teardown, while ForceZone's zero branch
remains nonterminal because a client-runtime flag selects phase 4 or teardown;
Silence state 2 remains an explicit client
no-op; and SpecialSmall state 2 is modeled as a class-local runtime-flag update
rather than a lifecycle transition. Cloud and Cloud2 keep raw 16 as an
installation token in state 1 but reinterpret the compact state-2 raw 16 as
the affected target; Magnet binds distinct raw-20 source/raw-24 target actors;
SpeedDown state 2 is the phase-2 terminal teardown. Devil/MqDevil bind a
secondary target only when discriminator 5 selects it; NewDevil retains the
same source/token activation without serializing that target.
`GopGoldShield` is now promoted from the C# compatibility-only set to the
80-class exact manifest. Its state-0 28-byte body activates a timed defense;
its state-2 34-byte body records a non-terminal successful block. Kind 0 maps
to Gold Shield item 36, kind 3 maps to Protect Shield item 81, and trailing
`u16=106` on state 2 is the Siren Shield override. The independent oracle and
FSM retain the activation while accepting repeated impact objects; unknown
kinds relay without authoritative registry mutation.
The normal-dependency-free `p5136-client-oracle::item_operation` module repeats
the recovered expansion's pair table, state locations, lengths, conditional
branches, counted targets, and actor offsets without importing core. Its
differential tests run all 166 recovered external branches and distinct actor
values. The independent oracle rejects every truncated prefix and extra suffix;
the production outer GameSlot parser likewise refuses to promote those shapes
to strict `ItemOperation` (while retaining its bounded opaque fallback policy).
Lucci, BonusItem and TeamFlag remain scope-excluded.

`p5136-client-oracle::item_client_fsm` now executes the original 149 consumer
fixtures as state transitions rather than decode-only assertions. The pinned
outcome census is 74 `LocalOnly`, 70 `DeferredOutbound`, zero
`ImmediateOutbound`, and 5 `UnknownSideEffect`. Angel state 0 is deferred
because its defense-hit continuation is proven; state 2 records a local
defense impact while retaining the timed Angel effect. The 14 later
BossPrison/BoundRoad/Falling/Piratebomb branches and the variable-length Course
consumer plus the two GoldShield branches share the same executor, so all 166
recovered branches are accepted.
The five unknown effects are pinned by exact class/state as `GopEventObject`
(`0x71110001`), `GopGiantTalisman` state 3, `GopRobotBeam` state 2,
`GopTombStone` state 2, and `GopWitchUnionMagic` state 4. They are runtime
lifecycle/effect-label gaps, not unresolved gameplay-page item/class joins.
Deferred results enqueue only a class/object/state scheduler marker: no next
state, timer expiration, collision, or packet byte is invented, and a newer
known lifecycle transition for the same object cancels the stale marker.
Known lifecycle state is stored by class/object key, `Remove` clears it, and
unknown or explicit no-op consumers leave it and any pending marker unchanged.
Strict decode failure is transactional across object state, deferred markers,
and counters.

The original 149 fixtures now also cross an executable production-server
boundary. `audit_game_slot_item_operation` uses the real outer `GameSlot`
decoder, validates the synthetic frozen-roster reporter/mask, invokes the same
item-to-registry operation mapping as `World::relay_game_slot`, commits any
planned mutation in an isolated race registry, and returns the owned relay
bytes. The pinned census is 88 `PublishTracked`, 61 `PublishUntracked`, and
zero `SuppressDuplicate`; all 149 relay byte-for-byte unchanged. This composes
with the existing actor/outbound-queue integration tests, but it deliberately
does not claim live execution of every rare stock-client visual, timer, boss,
or course-controller side effect.

The fourth-pass literal tests additionally cross Angel, Emp, Ghost, Icefly,
Scanning, SlotLock, SpecialSiren, SpaceCraft, and StraightRocket through exact
writer-shape validation, the independent client oracle, production parsing,
and race-object registry admission. Known activations/impacts are tracked;
Angel state 2 now produces a tracked non-terminal hit observation, while
StraightRocket states 2/3 are explicit client no-actions, publish untracked,
and leave the existing object fingerprint unchanged.

### Complete gameplay-page item catalog (2026-08-03)

The user-supplied Korean item page is now represented by
`p5136_core::item_gameplay_catalog`. Its 54 heading entries are a complete,
uniqueness-tested catalog across acceleration (6), attack (22), defense (7),
placement (11), status (5), and utility (3). Each entry records a stable slug,
Korean name, target scope, effect hints, a concise Korean summary, established
P5136 symbol/ID pairs, and evidence-graded `Gop*` links. Forty-one numeric
name/ID pairs are currently anchored as 19 retained fallback-table pairs, 20
Korean-executable initializer pairs, and two verified profile supplements
(`siren=24`, `superMagnet=103`). Exact literal tests pin all 54 per-heading
category/target/effect tuples and the ordered 53-pair evidence manifest.

Target semantics now distinguish the immediately preceding opponent, every
opponent ahead, nearby other karts, nearby karts including a possible source,
a fixed track area, and non-allied karts. This prevents the waterfly family
from collapsing into Thunderbolt semantics, captures bomb-waterfly self-hit
risk, and keeps Doctor R's allied-team exclusion explicit. Documented mode
availability is stored separately from target scope, and `None` explicitly
means unrecorded rather than unrestricted. Native operation-class evidence
and modern heading-association evidence are also separate axes; an unresolved
rolling waterbomb variant can no longer be promoted merely because both
candidate classes have recovered native writers.

The attached page was last edited in 2026, so it is never used to synthesize
P5136 offsets, states, durations, probabilities, or defense rules. Direct RHO
resources and executable producers now close every active ambiguous join:
Guide Rocket 33 uses `GopRocket`; StraightRocket 73 is Giant Missile;
Timebomb 13 is distinct from the Abyss special BigTimebomb 122; SnowWaterfly
118 is distinct from Kefi-special Icefly 80; Super Shield 18 uses `GopShield`
while `GopSpecialShield` is item 40; and both Cloud discriminator maps are
exact. `GopShield` state 1 is corrected to `item_id:u16@16, token@18,
source@22`. Rolling Waterbomb, Jiangshi, and first-place Devil are explicitly
`DeferredByUser`. Net and modern Random Missile remain page-only because no
P5136 join is proven, rather than because two packet classes compete.
GoldShield remains the exact Gold/Protect/Siren defense envelope. The full
ledger and source hash are in
[ITEM_GAMEPLAY_COVERAGE.md](ITEM_GAMEPLAY_COVERAGE.md).

The current independent oracle represents 12 of those 63 outer consumers and
does so at deliberately mixed evidence grades.  Fifty-one core outer
consumers, `GameSlotPacket` outer-type coverage, and the emitted nested item
schemas therefore remain open semantic-oracle work even where production
serializers and server-side integration tests already exist.  The new census
is an inventory and prioritization proof, not a claim of wire-semantic
completion.

### Start-collect-record codec and speed-team compatibility (2026-08-04)

The speed-team disconnect in runtime log
`p5136-1785851850557-11152.log` is now tied to the four-byte logical hash
`0x529107F4`, which is `PqStartCollectRecord`. Rust previously classified that
identity-bound hash as unsupported and actively ended the TCP session. The
retained C# dispatcher has only the packet-name enum entry and falls through
without a reply, which explains why the older deployed C# build did not fail
at the same point.

The installed P5136 executable establishes both native classes exactly. The
request uses the 16-byte base-only vtable at `0x01064E78`, so its wire shape is
only the hash. `PrStartCollectRecord` is a 20-byte object with vtable
`0x01064E9C`; native readers `0x00593260`/`0x00593590` write one raw byte to
object offset `0x10`, and writers `0x005938C0`/`0x00593BF0` emit that member
through the raw one-byte primitive at `0x00520660`. Its complete wire shape is
therefore `F5 07 A4 52 <flag>`, with no encoding or suffix.

Common `GameStage` consumer `0x00AD59F0` casts through `0x00AE8310`, verifies
only that the typed packet exists, and calls `0x00AE69A0`. That routine passes
`flag == 0` to `0x00AE6A00` and then stores the unnormalized flag in its owned
race state. The client accepts every nonzero byte as true, while the Rust
writer deliberately emits only canonical 0/1. A normal-dependency-free oracle
decoder pins the hash, exact five-byte completion boundary, truthiness, inverse
collector-gate argument, wrong-hash rejection, every truncation boundary, and
suffix rejection. The high-level FSM accepts the side effect in Loading,
Racing, or Settling without changing scenes and resets it at race/room boundaries.

The reply policy is now tied to the same equipment condition used by the stock
client. During race-state construction, `0x00B4A07C` calls
`sub_8E0970(12)` and treats any nonzero category-12 item ID as the condition
for retaining/creating `KartRecorder` at race-state offset `0x8C`.
`KartCatalog.xml` category 12 contains the replay recording cameras (IDs 1 and
5, plus client-defined variants). In the retained 65-byte equipment block this
is the ninth `u16`, persisted under the legacy C# JSON name `Set_HeadPhone`;
the Rust profile model keeps that serialized name but exposes a semantic
`replay_recording_camera_id()` accessor.

Production dispatch strictly parses the request, re-authorizes the exact
session generation, reads only its bound profile, and returns canonical
`PrStartCollectRecord(false)` when category 12 is empty or
`PrStartCollectRecord(true)` when it is nonzero. This does not synthesize or
trust a client flag. The client's independent special/forced-recorder guard
still prevents a false reply from deleting protected recorder modes. Focused
authenticated dispatch tests pin both five-byte replies and prove that a
request with a spurious suffix fails as a typed race-protocol error.

### Recorded-race finish disconnect and unknown-packet policy (2026-08-04)

Runtime log `p5136-1785855133322-26316.log` did not reach server settlement or
podium serialization for the local rider. At the speed-team finish the client
sent the exact 24-byte logical packet
`BC0AF494D294010000000000670000005F00000039010000`, hash `0x94F40ABC`.
Rust returned `UnsupportedIdentityPacket` and terminated that TCP session;
the later 367-byte room snapshot to the peer was cleanup after this server-side
disconnect, not the initiating crash.

The hash is `PcReportUserCollectedRecord`. The pinned IDB establishes a
36-byte object, five consecutive wire dwords after the base hash, and native
readers/writers at `0x00728430`/`0x0072B780` and
`0x0072E4A0`/`0x00730F30`. Producer `0x00A84930` identifies the first value
as elapsed collection time; the captured fields decode to
`103634, 0, 103, 95, 313`. The other four metrics remain diagnostic rather
than being given invented gameplay semantics. Neighbor
`PqReportGameCollectedRecord` is proven base-only/hash-only. Both retained C#
requests fall through without a response, and Rust now exactly parses,
re-authorizes, logs, and consumes them without mutating authoritative results
or emitting a fabricated reply.

The broader authenticated fallback now follows the requested compatibility
policy. Classified packets still use strict complete-consumption codecs and a
malformed known packet remains an error. A genuinely unknown hash is checked
against the admitted identity generation and bound profile, then produces a
`p5136_packet` warning and no response while the session stays alive. Its raw
payload is already preserved by the immediately preceding bounded logical
receive record. This replaces the former arbitrary-unknown fail-closed policy
without weakening typed handlers.

### Protocol-visible client FSM reconstruction (2026-08-03)

`CLIENT_PROTOCOL_FSM.md` separates the native packet-consumer graph from
the compatibility-safe semantic order. RTTI and decompilation establish the
delegation chain `ChannelStage -> SessionStage -> SessionReadyStage`, with
`GameReadyStage` and `GameFinalStage` applying their own branches.

The ordinary speed/item solo/team stages all delegate to the common
`GameStage` consumer and then add mode-specific `GameSlot`, leave, race-time,
and control effects. `sub_A847F0` switches exactly on server `GameControl`
states 1, 3, and 4 and invokes virtual slots 97, 98, and 99. The state-4
speed/item callbacks move the internal mode phase from 2 to 3 before invoking
the final UI/effect helpers. This upgrades the 1/3/4 dispatch from a generic
partial-consumer claim to a native state-effect anchor without inventing
names for every internal field.

The independent `protocol_fsm` module models transport and scene separately:
server-first login, rider bootstrap, normal channel reconnect, same-socket
club UI hand-off, room admission/snapshots, Loading, Racing, Settling, and the
three ceremony phases. `GrCommandStartPacket` is correctly treated as
self-contained because `sub_CF3F30` reapplies its nested session and slot
packets; prior standalone room snapshots are not an artificial start guard.
UDP time-sync is recorded as readiness evidence but not required for server
state 1 because the bounded timeout path is intentional.

The compatibility FSM enforces the deployed ceremony sequence
`GameControl(4) -> GameNextStage -> GameResult`, rejects standalone lobby
snapshots after Loading, permits leave-room across all room/race phases, and
preserves only an established migration epoch across reconnect. Ten focused
transition tests cover success, timeout, rejection, rollback, and cross-scene
failure paths.

The podium scheduler follow-up is now statically closed for the standard
individual and team stages. `GameFinalIndiStage::update` calls `sub_B42500`;
`GameFinalTeamStage::update` calls `sub_B507D0`. Their final phases copy the
RTTI-checked saved `GameFinalIndiParam`/`GameFinalParam` into virtual slot 103
(`sub_B49BB0`), which calls `sub_BED050 -> sub_BED1D0` and replaces the stage
with `GameReadyStage` or a game-mode-specific ready stage. Global flag `0x40`
selects `ObserverReadyStage` and the longer individual observer presentation.

The ordinary individual fixed delays are strict `>1000`, `>100`, and `>5000`
milliseconds around an offset-2132 animation-completion gate. The ordinary
team path uses strict `>1000`, `>100`, and `>7000` millisecond gates. Team mode
flag `0x80` stops after the last delay until local action 13 advances its phase
4 to phase 5. Exact final-stage RHO extraction found UI resources but no
duration or Room callback, confirming that these conditions live in the
executable. Four independent `final_stage_scheduler` tests now pin timer
boundaries, animation gates, observer stage selection, and the manual team
confirmation path. The high-level transition is named
`ClientPodiumSchedulerCompleted`; no server packet is fabricated for the
local room installation.

### Independent client-semantics oracle (2026-08-03)

The workspace now contains `p5136-client-oracle`, a separate non-published
crate that reconstructs selected client-side packet readers without importing
the production `PacketReader`, Adler hash function, wire constants, encoded
scalar table, or serializer models. `p5136-core` is a dev-dependency only for
integration-test input; `cargo tree -p p5136-client-oracle --edges normal`
shows no normal dependency at all. This prevents a server writer and its test
reader from sharing the same offset or hash bug.

The primary native-evidence tests do not obtain their input from the server
serializer. Checked-in synthetic raw fixtures transcribed from the recovered
IDB layouts cover one team human plus one DNF AI `GameResult` and the complete
`GameNextStage` body. Serializer-produced packets are retained only as
differential coverage against those independent readers.

The oracle records evidence grade per packet instead of treating every exact
byte test as native semantic proof:

- `GameResultPacket` is `IdbLayoutExactPartialSemantics`, based on
  `sub_726CC0`, human reader `sub_71BF00`, and AI reader `sub_71BAD0`. The
  independent reader consumes the full 212-byte human and 22-byte AI records,
  recovers and tests every exposed identity/result/team/points/character/club
  field, checks exact completion, rejects every truncated prefix and extra
  suffix, and explicitly rejects the old malformed 217-byte C# result record.
  Unnamed record spans and the 34-byte outer result state remain opaque and are
  length-checked without assigning invented semantics.
- `GameNextStagePacket` is `IdbCodecAndConsumerExact`: the oracle independently
  checks its 13-byte codec and stage fields. The ceremony state machine then
  enforces the deployed acceptance order
  `GameControl(state=4) -> GameNextStage -> GameResult`; `GameControl` state
  dispatch for states 1/3/4 now has a native common-handler anchor, while the
  full meaning of every callback and internal field remains only partially
  reconstructed.
- `PrStartCollectRecord` is `IdbCodecAndConsumerExact`: a hard-coded-hash
  oracle consumes its single raw flag byte, models the common-`GameStage`
  inverse gate argument and stored truthiness, and rejects every truncated
  prefix and extra suffix independently of the production serializer.
- Room list/create/join, `GrSessionData`/`GrSlotData`, authentication/login/
  migration, and the initial room snapshot have independent structural readers
  with distinctive nonzero/UTF-16/endpoint fixtures and exhaustive truncation
  on the highest-risk variable packets. Their evidence remains
  `CSharpGoldenPlusLiveTrace` because a retained native consumer decoder has
  not yet been recovered.
- The five read-only club replies additionally model both sides of the
  evidenced stock consumer branches: absent/present membership, failed/empty/
  present pending join, every known create-condition status, zero/nonzero list
  count, and both `current < capacity` and full/unavailable admission.

The oracle integration suite passes. Its policy tests prevent silent
promotion of evidence grades and prevent `p5136-core` from becoming a normal
oracle dependency. The complete workspace
passes `cargo test --workspace --all-features`; full all-target/all-feature
Clippy passes with `-D warnings`; formatting and the no-normal-dependency check
also pass. This is the first high-risk semantic slice, not a claim that every
server-to-client serializer now has a reconstructed client consumer. The
remaining global oracle and stock-client LAN E2E gates stay open in
`PORTING.md`.

### Ceremony packet-order correction from deployed C# evidence (2026-08-03)

The two-machine Rust log
`target\p5136-finish-kart-abilities\release\logs\p5136-1785768755004-28608.log`
records an item-team settlement at `14:55:31.804Z`: Rust sent
`GameNextStage` (13 bytes), the old 486-byte `GameResult`, then final
`GameControl(state=4)` (85 bytes). The local client continued writing for
roughly 13 seconds and then reset TCP; the server-side Windows error 10054 is
the consequence of that client exit, not its initiating cause.

Static analysis confirms that the modern C#-derived human-result record is not
the P5136 layout. `sub_71BF00` consumes 212 bytes when all four bounded nested
vectors are empty. At wire offset 63 it reads the team as one byte, then reads
dwords at offsets 64, 68, and 72. The old writer instead put `team_points` at
63 and the team byte at 67, and appended five bytes beyond the decoded record.
For `team_points=10, team=1`, the next dword consequently became
`0x01000000`.

- The previously deployed C# server emitted the same malformed 269-byte
  one-human and 486-byte two-human layout. The exact two-human fixture is in
  `KartRider_5136\logs\packet-trace_20260717_210737_188_13040.log` at packet
  sequences 2173/2176. Every retained historical result has
  `winning_team=0`; those solo zeroes mask the team-field corruption, so the
  old packet bytes are not a valid team-result golden fixture.
- That working trace instead differs in packet order. It sends final
  `GameControl(state=4)` at sequences 2161/2164, `GameNextStage` at
  2167/2170, and `GameResult` at 2173/2176.
- Rust now follows that deployed order exactly:
  `GameControl(state=4) -> GameNextStage -> GameResult`. Its human codec now
  emits the decoder's 212-byte record, places `team` at 63 and `team_points`
  at 64, and no longer appends the five unconsumed bytes. The immutable result
  snapshot, complete DNF roster, final-stage tick, and atomic all-recipient
  publication are unchanged; the two-human/no-AI packet is now 476 bytes.
- The current read-only C# source contains a later Korean5136-specific branch
  that selects the opposite order. It is not treated as stronger evidence
  than the user's known-working deployed trace, and no C# file was changed.

The core codec test asserts the decoder offsets and fixed length. The focused
pending-fanout and DNF/retry tests assert all three packet hashes in the
deployed order. Core all-target tests pass 251 unit plus one frame-boundary
integration test. Server all-target tests pass 485 unit tests with two local
proprietary-data tests ignored plus nine UDP integration tests, and
all-target Clippy with `-D warnings` passes. A fresh two-machine ceremony run
remains the live acceptance gate.

### Post-ceremony stale lobby-snapshot crash fix (2026-07-31)

The two-machine log
`target\\p5136-finish-kart-abilities\\release\\logs\\p5136-1785542687403-33332.log`
shows that, at `00:08:56Z`, Rust sent the then-current `GameNextStage` (13
bytes), two-human `GameResult` (486 bytes), and final `GameControl` (85 bytes)
to both clients without an immediate reset. That delayed reset did not prove
the settlement order correct; the 2026-08-03 deployed-C# comparison above now
controls the order. The stale lobby-snapshot finding below remains an
independent cross-scene defect.

The only subsequent server-to-client application packet was a 367-byte
`GrSlotDataPacket` (`0x337C062D`) delivered to the peer at `00:09:09Z`, then
both clients reset their TCP connections. Its source was the other client's
`GrFirstRequestPacket` at `00:06:17Z`: Rust had added a duplicate peer lobby
snapshot while rehydrating that requester. The packet was stale by the time
the peer's TCP writer reached it and attempted to apply lobby state over the
post-race scene.

- Rust now treats `GrFirstRequestPacket` as requester-only scene
  rehydration: the requester gets the ordered `GrSessionDataPacket` plus
  `GrSlotDataPacket`; existing room peers receive nothing. They already
  receive authoritative slot snapshots for actual join/leave/ready/team/
  track changes.
- This intentionally differs from the C# redundant peer broadcast. It removes
  a cross-scene stale-state hazard without weakening authoritative room-change
  fanout.
- World and real encrypted-TCP integration tests cover the requester-only
  behavior, including a partially received peer frame. A fresh LAN run is
  still required to confirm the stock client returns from ceremony to the
  room without a reset.
- The tag-driven GitHub Release workflow packages from the same bounded
  `target/p5136-finish-kart-abilities/release` directory configured for local
  builds, so its Windows/macOS/Linux assets cannot silently look in Cargo's
  default target path.

### C# opaque-packet audit remediation (2026-07-31)

The read-only static audit
`C:\Users\drash\Documents\kartrider\analysis\P5136_CSHARP_OPAQUE_PACKET_AUDIT.md`
was compared against the Rust codecs and retained packet fixtures. The C# tree
was not modified.

- `PcRideSwithInfoPacket` (`0x5815082A`) is no longer admitted as an arbitrary
  1,024-byte opaque body. Rust now consumes its `f32` elapsed value, bounded
  UTF-16 nickname-to-`u32` map (at most 8 names × 64 units), bounded 8-byte
  sample vector (at most 64), and exactly eight aggregate words. The captured
  56-byte retire, 72-byte finish, and 88-byte post-goal forms are regression
  fixtures for that one structured codec.
- `GameAiReportPacket` is decoded as eight diagnostic metric words;
  `PcGameClientFramePacket` as three neutral metrics; `GameBoosterAddPacket`
  as an exact empty signal; and `PcReportStateInGame` as the exact four-word
  sequence/tick heartbeat. `GameReportPacket` is fully consumed with neutral
  diagnostic labels. None can affect race results or item authority.
- The normal 406-byte `GameControlPacket(state=2)` now has a typed diagnostic
  decoder for the complete 393-byte snapshot: follow-on values, seven-word
  session envelope, 54-byte result subobject, 235-byte effective KartSpec,
  a validated `length=22` shared-object payload, participant slots, global
  metric, local result, and terminal state. World settlement still trusts only
  its server-owned state transition and the existing finish-time policy.
- `LoRqUseItemPacket` is corrected to three unsigned words:
  SlotItem category, SlotItem ID, and remaining quantity. GameSlot type 10/16
  now exposes uninitialized raw bytes 13/18, producer-supplied byte 19, the
  full status word, and the post-consume ancillary effect code rather than
  misleading boolean/trailing-word labels; type 16 remains a shared effect
  packet, not a barricade alias.
- `PcStartMatching` and `PcCancelMatching` are now exact authenticated
  codecs. A start receives the complete seven-byte empty/create
  `PcMatchingFound` variant (`hash | 00 00 00`) rather than C#'s truncated
  two-byte body. Rust deliberately does not choose a random existing room;
  actor-owned pairing and matching-room migration remain a separate,
  two-client-capture-gated feature.
- `PcGameRequestRelay` now retains both dwords as desired-peer and requester
  slot diagnostics, but still does not invent a directional
  `GameRelayBroadcastingPacket`: consumer analysis has not fixed the output
  second-word transformation. A broad room broadcast or hard-coded zero would
  be less correct than the current no-reply behavior.

### Local Club UI hand-off from the live P5136 crash (2026-07-31)

The stock-client run in
`target\\p5136-finish-kart-abilities\\release\\logs\\p5136-1785523302246-26424.log`
ended at the first `PqClubChannelSwitch` request, hash `0x48770772`, because
Rust treated the previously unclassified identity-bound packet as terminal.
The exact 29-byte decoded request is a channel-switch-shaped envelope:
`hash | opaque_length=14 | opaque[14] | game_type=52 | channel=0 |
reserved_zero[4]`. It is not an ordinary `PqChannelSwitch`: C# replies with
the local Club UI variant of `PrChannelSwitch` rather than creating a TCP
migration permit.

- `p5136-core::channel` now exposes a dedicated parser that bounds the opaque
  block before copying, verifies the `PqClubChannelSwitch` hash, and requires
  exactly four zero reserved bytes. Wrong hashes, every truncated prefix, and
  nonzero/resized reserved suffixes are typed errors.
- Rust now returns C#'s exact 18-byte local hand-off:
  `PrChannelSwitch | mode:i32=1 | channel:u16=13 | token:u16=0 |
  endpoint=0.0.0.0:0`. It does **not** call normal migration admission, so
  this special response cannot revoke or freeze the current authenticated
  login generation.
- Session coverage verifies the captured packet, the exact response,
  malformed-suffix rejection, required bound profile, stable identity, and a
  successful same-connection `PqServerTime` afterward. This is deliberately a
  safe C#-level Club entry point, not a claim that a global Club repository,
  membership namespace, join flow, or club economy exists. Those remain the
  explicitly deferred actor-owned design slice below.

### P5136 ceremony/DNF diagnostic checkpoint (2026-07-31)

The item-team race in
`target\\p5136-finish-kart-abilities\\release\\logs\\p5136-1785523302246-26424.log`
ended with one human and four AI entries. One AI did not finish; it remains a
normal result participant with `finish_time = u32::MAX`, its assigned rank,
kart, team, and team points. Rust must not remove or synthesize away that
entry merely to avoid a client crash.

- Static comparison and the deployed C# packet traces confirm the result
  roster retains all four AI entries, including the DNF entry. Removing the
  old human record's five unconsumed bytes changes this one-human/four-AI
  packet from 357 to 352 bytes without removing a racer.
  The deployed P5136 settlement order is final `GameControl(type=4)`,
  `GameNextStage`, then `GameResult`; Rust now uses that captured order rather
  than the later source-only Korean5136 branch.
- C# excludes the first human finisher from the earlier
  `GameControl(type=3)` broadcast. Rust preserves that recipient exclusion;
  adding it back would be a speculative incompatibility change.
- The server now emits a structured, packet-derived ceremony snapshot before
  serialization, listing every human and AI tuple including DNF time/rank and
  the final packet length. It supplements the existing raw TX log without
  changing wire behavior, so a subsequent client crash can be correlated to
  the exact frozen result roster.

### Two-client item race follow-up (2026-07-31)

The live run in
`target\\p5136-finish-kart-abilities\\release\\logs\\p5136-1785526711472-27784.log`
identified two concrete protocol gaps and one verified delivery path.

- The client sent the exact 88-byte `PcRideSwithInfoPacket` (`0x5815082A`)
  no-reply report immediately after `goalin`. The original seven-length
  admission was an overly narrow Rust policy, not C# behavior. The later
  opaque-packet audit recovered its bounded elapsed/map/vector/eight-aggregate
  container, which Rust now consumes completely and keeps non-mutating.
- The two-client trace contains `GopMine/GoItemMine`, state 2, with a 29-byte
  payload and a peer mask that exactly names the other frozen racer. It remains
  a typed diagnostic fixture, but routing is now derived from the validated
  type-12 envelope and known operation pair rather than this one observed
  state/length. Other known `Gop*`/`GoItem*` bodies receive the same
  sender-excluded, peer-mask-checked, non-mutating relay treatment.
- Player ID 1 submitted 34 valid type-1/type-2 item-box pickups. Each was
  synthesized once and sent to both frozen recipients with C#-matching bytes:
  the original 73-byte request, final item ID at offsets 38..39, success byte
  40, and untouched context/blob tail. This log contains no missing server
  award or sender-exclusion for that player; a remaining client-side display
  symptom needs a new reproduction after the terminal post-goal disconnect is
  removed, rather than a second speculative award path.

### Authoritative item-box pickup and probability controls (2026-07-30)

The stock-client run in
`target\p5136-gameslot-static\release\logs\p5136-1785453355779-31260.log`
reached item individual gameplay and sent three valid 73-byte type-1
`GameSlotPacket` item-box requests at 23:18:36 and 23:18:46. All three passed
the strict hash, sender, all-bits mask, finite-position, blob-length, and
`GopCube`/`GoItemCube` checks. The World actor then returned the explicit
`ItemPickupSynthesis` evidence-pending outcome, so no packet was sent. This
was the complete cause of the missing item acquisition; there was no new
transport or client disconnect error in that interval.

- Parsed type 1/2 requests now mint a `SynthesizeItemPickup` capability.
  Before minting it, the parser requires the captured pre-award state and the
  repeated object/tick/owner fields to agree. Type 1 requires its
  `0xF00000xx` object and `tick/tick+1500` relation; type 2 requires its
  `0x00FFFFFF` sentinel and captured current-item state. A reflected server
  award is therefore not accepted as another request.
  `ParsedGameSlotPacket::into_item_pickup_award` consumes that capability,
  requires a positive item ID, preserves the validated request, replaces
  offsets 38..39 with the chosen `i16`, and writes success byte 40. The exact
  73-byte tail, declared 24-byte blob, and `GopCube` payload remain unchanged.
- The single-writer World actor permits pickup only for the exact frozen human
  sender during the existing active-race window and only in item individual
  game type 2 or item team game type 4. Per race/player it rejects an exact
  `(operation tick, kind, object)` replay and a wrapping-monotonic tick
  regression while allowing the captured same-tick/different-object burst.
  A six-token bucket refilling at four awards per second bounds fabricated
  increasing ticks; the retained 65 pickups peak at three events in 100 ms,
  500 ms, and one second, so the complete corpus remains admitted. Token,
  replay, and rate state commit only after every recipient queue is reserved.
  The actor then broadcasts the synthesized packet to the whole room including
  the sender. Any full queue rejects the complete pickup without consuming
  admission state.
- `ItemProbabilityConfiguration` owns separate individual/team immutable
  tables and `Live`, `Top`, `High`, `Middle`, `Low`, or `Combined` selection.
  The explicit `ItemProbabilityRankPolicy` makes the trust boundary visible.
  For the current LAN/friends model it defaults to `TrustClientReported`, so
  `Live` uses the C# mapping (eight racers:
  `Top, High, High, Middle, Middle, Low, Low, Low`). The GUI checkbox or
  `--trust-client-item-rank false` selects `CombinedFallback` without changing
  explicit Top/High/Middle/Low/Combined choices. IDs must be positive and
  unique, tables are capped at 512 rows, names at 64 characters, each weight
  at 1,000,000, and every active rank total must be nonzero. Checked `u64`
  totals and an explicit `[0,total)` roll avoid overflow and modulo bias.
- The installed P5136 data keeps these tables in the 28.9MB legacy
  `Data\item.rho`, not the RHO5 packs. The new no-`unsafe`, read-only
  Rh-layer-1.1 reader bounds archive/block/directory/name sizes, validates the
  decrypted header and nonzero block checksums, rejects duplicate/out-of-range
  metadata, and extracts only
  `slot/itemProb_indi@zz.bml` and `slot/itemProb_team@zz.bml`. Its bounded
  UTF-16 BML path reproduces the C# result exactly: 14 individual rows with
  combined sum 400 and 18 team rows with sum 410. Equivalent RHO5 entries are
  supported when no legacy `item.rho` exists.
- A portable XML override uses
  `<itemProbabilities rankBand="..."><individual>...` and `<team>...` with
  stock `item` attributes `idx`, `name`, `toprank`, `highrank`, `midrank`,
  and `lowrank`. It is read through one bounded handle and requires one exact
  root, one instance of each section, complete closure, no DTD/text payload,
  bounded rows, and no trailing partial document. CLI uses
  `--item-probability-xml PATH`.
- The native Server GUI now has the C#-equivalent rank selector, individual
  and team tables, read-only IDs/names, bounded editable weights, and buttons
  for client `item.rho`/RHO5 values, portable XML, automatic client reload,
  and the safe 14/18-item fallback. As with the existing GUI controls, edits
  apply at the next server start and are not persisted. Automatic tables are
  intentionally non-editable until `Load client item.rho/RHO5 values` pins the
  actual resolved rows, preventing a displayed fallback table from being
  mistaken for the stock distribution. `Trust client-reported live rank
  (LAN/friends)` is checked by default and can be cleared independently of the
  selected probability table.
- Kart-specific item acquisition remapping is now authoritative and separate
  from actor-owned three-slot consumption/use effects. The bounded catalog
  parser retains resolved `Abilities/TransformByKart` rules, and the World
  actor applies at most one matching `no_flag` rule to the base probability
  award using the frozen race kart snapshot. This covers the RHO-backed
  Gigantes V1 mappings `magnet(5) -> superMagnet(103)` and
  `rocket/randomRocket(7/127) -> tigerRocket(99)` without copying C#'s
  possible double-transformation path. The monotonic token and bounded rate
  gate do not yet prove track-box
  spawn/collision ownership or clock freshness; those require actor-owned
  track state or a verified UDP client-clock binding. Type 4/6/16 routing and
  type-12 source/target/object effect semantics remain evidence-gated; known
  type-12 client events already use bounded relay-only routing.
- The user confirmed the preceding item-individual room-mode fix and V1 parts
  equip fix both work in the stock client. The next stock run should verify
  visible item acquisition; a two-machine LAN run remains necessary for
  sender-inclusive peer delivery.
- Live item probability bands now use the stock fixed 2-8-racer matrix rather
  than an inferred even three-way split. The count includes frozen AI racers
  (but not observers), so client-reported placements among mixed human/AI
  fields remain valid. In particular, two-player second place is `Middle`,
  and the corrected matrix also changes the old 4-, 5-, and 8-racer boundary
  mistakes.
- The same server-owned pickup transformation path covers the stock Sebek V1
  (`kartId=1395`) rules: UFO, water fly, magnet, booster, rocket, water bomb,
  EMP, and time bomb each independently become `goldShield(36)` on a 25%
  `no_flag` roll. The authoritative type-1/type-2 award must carry the final
  item ID; this is not delegated to an unverified second client-side roll.

### Live MyRoom/X-parts/room-mode fixes (2026-07-30)

The stock-client run recorded in
`target\p5136-gameslot-static\release\logs\p5136-1785449807325-35392.log`
separated one intentional EOF from two actionable failures and one room-mode
policy gap.

- MyRoom search sent no follow-up request before the client reset its TCP
  connection. Static catalog comparison found five named kart entries without
  a matching exported spec; one is kart `814`, `monster13`, which is included
  by a `몬스터` search. Catalog grants now exclude only
  named-but-unresolved karts when name/spec metadata exists. Inventory-only
  legacy catalogs retain their old behavior, while profile duplicate-kart
  grants use the same filtered grant iterator. A persisted equipped kart that
  is no longer client-safe is atomically reset to the unequipped state during
  `PqGetRider`, and plant/parts exception records for such karts are omitted
  from the outbound preload.
- The captured V1 engine request
  `(kart=1454, serial=1, category=63, item=2, grade=2, value=1150)` was an
  exact record generated and sent by the Rust inventory builder. The old
  equipment path incorrectly checked it as an ordinary XML catalog grant,
  making every generated category-63 through category-66 part impossible to
  equip. Publication and authorization now share one typed series table and
  validate category, item, grade, value step/range, and the profile-specific
  inventory amount. A rejected value or sidecar persistence error returns an
  inferred `PrEquipXPartsItem` result-1 failure and leaves durable state
  unchanged instead of terminating the TCP session. Only result 0 is present
  in retained traffic, so result 1 is documented as a compatibility inference,
  not a captured golden. A real encrypted-TCP malformed-sidecar test verifies
  the failure frame is followed by a successful request on the same session.
- Public channel-to-room mapping now includes item individual
  `65 -> 2`, item team `66 -> 4`, and the corresponding item/speed newbie
  channels `8 -> 2`, `7 -> 4`, `14 -> 1`, and `13 -> 3`. Special battle,
  club, and matchmaking channels remain closed rather than being silently
  treated as ordinary rooms.
- Regression coverage proves named unresolved kart grants, persisted rider
  selection, and equipment sidecars are filtered; observed V1 values are
  accepted from the same generated series; invalid values and sidecar failures
  are non-terminal and non-mutating; and both item channels create matching
  rooms.

### Retained 19,496-record corpus completion (2026-07-30)

- The external
  `C:\Users\drash\Documents\kartrider\KartRider_5136\logs` corpus is now an
  executable opt-in test boundary rather than a prose-only inventory.
  `P5136_PACKET_TRACE_DIR` drives a read-only parser that verifies all 19,496
  incoming records, 100 distinct hashes, and 97 TCP hashes. Every TCP hash is
  owned by a composed Rust dispatch domain, every actual packet in the former
  28-hash gap is fully consumed by its strict codec, and every Game/P2P UDP
  record passes the routed UDP codec.
- The five room-control gaps use the existing actor boundary. Track/room-data,
  bounded basic AI, closed slots, room talk, and macro chat are parsed before
  an actor command; authorization/state checks and all-recipient queue
  reservation precede atomic fanout. Macro text comes from the bound profile
  and preserves team filtering.
- X-parts now load and publish `PartsData.json` through the run lease and
  no-follow profile capability. The file is atomically replaced and synced
  before the exact success reply is possible. Windows directory-handle
  `sync_all` incompatibility is handled by syncing the file and rename, then
  verifying the directory capability; Unix retains directory `fsync`.
  Nonregular, malformed, and oversized sidecars fail closed without mutation.
  Only category 63 appears in this retained corpus; broader X-parts evidence
  remains an explicit non-corpus feature gap.
- Locked items share the bounded ordered-set value abstraction with favorites
  but remain a distinct canonical profile field. Get/Update use one canonical
  profile lane and immutable durability receipt. An unresolved C#
  `Locked.json` is captured exactly once through the same lease-bound,
  no-follow importer; import plus the first update share one revision.
  Nonregular, malformed, and oversized sidecars leave canonical state
  unresolved and cannot produce success.
- The single-player/time-attack codec consumes the complete captured shapes,
  including producer bytes ignored by C#. Kart physics is built for the
  requested kart and speed from the catalog with explicit fallback reasons
  for contributions not represented by current authoritative data. Time
  attack start atomically charges the checked entry fee and stores
  mode/track state before reply. Finish requires one active start, uses checked
  Lucci arithmetic, atomically stores time/RP/Lucci before reply, and consumes
  the active-run capability so a replay cannot reward twice.
  A duplicate start while that capability is active is rejected before profile
  I/O or fee deduction; its regression test proves revision, Lucci, and the
  exact active-run state remain unchanged.
- Career state and UDP reconnect are typed client events. Native writer
  analysis and the barricade-hit runtime capture prove that career state is
  `career:i32 | state:i32 | count:i32 | count * (item:i16, i32, i32)`, not a
  fixed one-entry body. Rust accepts at most 64 entries, requires exact
  consumption, and treats the result as diagnostic no-reply input. Reconnect
  authorizes only the same identity
  generation and clears both Game/P2P route bindings behind the UDP arrival
  epoch fence, preventing a pre-reset datagram from becoming the replacement
  endpoint.
- Game AI, game report, frame, relay, booster, heartbeat, ride-event, and
  ride-path telemetry all have bounded complete-consumption codecs.
  `0x5815082A` is the named `PcRideSwithInfoPacket`, not an unknown-hash
  exception: its map/vector/aggregate containers are parsed for each retained
  56/64/68/72/76/80/88-byte form. The 56-byte compact-retire fixture comes
  from `p5136-1785514451491-14604.log`, the 72-byte finish-adjacent fixture
  from `p5136-1785511619293-33416.log`, and the 88-byte post-goal fixture from
  `p5136-1785526711472-27784.log`. At that checkpoint arbitrary unknown hashes
  still failed closed; the recorded-race finish correction above supersedes
  that fallback with authenticated logged no-reply consumption. Rust does not
  copy C# client-authoritative anti-cheat mutation or fabricate the disabled
  relay branch.
- Strict Windows gates pass, with three ignored local opt-in tests. The opt-in
  set covers both proprietary client-data extractors and the complete external
  packet corpus. Workspace
  all-target/all-feature Clippy with `-D warnings`, formatting, and
  `git diff --check` pass. Production Rust remains under workspace
  `unsafe_code = "forbid"`; the legacy RHO crypto and parsing path also uses no
  `unsafe`.
- The independent GameSlot re-review found no P0. Its native-zero reserved
  word and stale-frozen-recipient findings were corrected and revalidated.
  The then-current 73 manifest pairs and count formulas were independently matched against
  the external matrix. Automatic future synchronization with that external
  analysis artifact and authoritative object ownership remain documented
  evidence/tooling gaps rather than guessed runtime policy.
- The current CLI/GUI E2E build is
  `target\p5136-finish-kart-abilities\release\p5136.exe` (18,356,736 bytes,
  SHA-256
  `4293B8F1245CB28277677A0E6263356FC8917626B65686A42649B2A378576376`).
  `--version` reports `p5136 0.1.5`; the release target is the single fixed
  Cargo output directory. The latest stock-data loader test used the user's
  real `KartRider_5136\Data` directly, loaded 493 catalog transforms, classified
  1,282 automatic and 14 quarantined karts, and required no generated
  `Profile/KartCatalog.xml`. The earlier bounded smoke started all four
  transports and passed messenger reachability. The installed
  legacy `item.rho` opt-in test separately confirmed 14/18 probability rows
  and combined weights 400/410. The server smoke log is
  `target\p5136-finish-kart-abilities\release\logs\p5136-1785851010010-9888.log`.
  Launch with no arguments for the Server/Connector GUI or use the documented
  `server`/`connect` commands.
- No C# files were modified. C# remains protocol/product evidence; checked
  arithmetic, persist-before-success, replay denial, actor state ownership,
  strict known-packet codecs, and authenticated logged unknown consumption
  intentionally improve unsafe C# behavior.
- Local Cargo output is fixed to the same
  `target\p5136-finish-kart-abilities` directory through
  `.cargo/config.toml`; older diagnostic build trees were removed after this
  checkpoint.
- The retire/flying-pet checkpoint was rebuilt after the previous runtime
  process closed. Debug test artifacts were removed with
  `cargo clean --profile dev`; only the fixed release tree and its reusable
  release dependency cache remain.

### Lobby kart-physics refresh and V1 speed abilities (2026-07-31)

- A successful `LoRqSetRiderItemOnPacket` now reloads the durable selection,
  builds a fresh 235-byte kart-physics block, and asks the World actor to
  replace only that member's cached block while the protocol room is in the
  `Lobby` phase. Thus a kart changed after room entry is reflected in the next
  `GrCommandStartPacket`; the cached block becomes immutable once Loading
  begins, so an equipment packet cannot alter an active race.
- This corrects the observed `rollerBrushX_gold (1362) -> whiteKnightV1
  (1440)` switch, where the start packet previously retained the old
  `StartBoosterTimeSpeed=1000`, dual-booster `40..60`/`1.10`, and base instant
  acceleration values instead of White Knight V1's catalog physics. The
  catalog parser already serializes the V1 dual-booster and instant-accel
  fields; the stale room snapshot was the missing link.
- The regression starts a room with one physics block, publishes an equipment
  snapshot, refreshes it through the admitted identity capability, and proves
  that the start command contains only the new block. The actor command is
  generation-authorized, returns `false` outside a Lobby, and does not fan out
  a synthetic wire packet.
- The later-version V2 Exceed code is deliberately excluded rather than a
  missing P5136 extractor: `Parts12Data.json` and `Level12Data.json` are
  per-account C# server persistence files, not stock client assets. The C#
  audit documents that Korean P5136 emits only the Tune/Plant/Level/Parts four
  streams; adding the later byte-gated Level12/Parts12 stream desynchronizes
  its decoder. Its exported catalog also supplies no nonzero V2 default type.
  Normal P5136 V1 speed/dual-booster/instant-accel fields remain data-backed
  from the catalog and are now refreshed correctly.

The current checkpoint closes the direct-request disposition ledger at 40 of
40 by adding strict item-state boundaries for `LoRqDeleteItemPacket`,
`PqUnLockedItem`, `PqFavoriteItemGet`, and `PqFavoriteItemUpdate`. The later
favorite migration checkpoint resolves an absent Rust marker with a bounded,
lease-bound C# `Favorite.json` import, then commits that import and the
incoming Get/Update projection in the same immutable revision. Delete and
unlock remain explicit, authenticated, read-only no-reply outcomes: Rust does
not clone C# success acknowledgements that would claim deletion or unlock
without an authoritative durable transition. The C# repository remains
unchanged and is evidence only.

### Race completion GameControl tail (2026-07-31)

- The runtime log `p5136-1785522788375-25004.log` isolated a client disconnect
  to its normal 406-byte `GameControlPacket` finish report: the 13-byte prefix
  is followed by a 393-byte result snapshot. Rust's former 256-byte
  compatibility cap rejected it before the World actor could record the
  finish, making the typed protocol error terminal for the TCP session.
- Rust now decodes the statically confirmed snapshot boundaries and logs only
  non-authoritative diagnostics; it does not trust client result, physics, or
  session-attestation fields to decide settlement. The 512-byte cap remains
  for other versioned control extensions and a 513-byte tail is rejected.
  The corrected internal boundary is 235-byte KartSpec plus a four-byte
  `22` length prefix and 22-byte shared object; the former 243+18 split had the
  same total length but shifted every nested boundary by eight bytes. Parser
  and session regression coverage preserve the exact 406-byte shape, the
  shared-object prefix, the typed fields, and the cap boundary.

### Desktop server and connector GUI

- `p5136` remains one native binary by design. `p5136 server` and
  `p5136 connect` remain the scriptable CLI surfaces; launching with no
  arguments opens the desktop Server/Connector GUI.
- The Server tab maps directly to the public CLI server configuration: bind
  address, advertised IPv4 address, configured base port, profile root,
  required client root/`Profile`/`Data` location, remote profile creation, and
  the advanced first-message/session timeout and login-limit values. Inputs are
  validated before a bind and apply only to the next start. The GUI persists a
  bounded versioned snapshot of server and connector input fields, including
  paths, nickname, runner configuration, probability edits, and random-track
  overrides. Runtime state, log/catalog/search results, and profile contents
  are not part of that snapshot.
- Server ownership never crosses into the GUI thread. A named worker owns the
  `ServerHandle`; it reports the four bound endpoints, receives explicit
  graceful/force commands, and is joined after it reports a terminal result.
  Graceful shutdown remains interruptible by Force while the runtime drains
  wire/profile work. A retained reward recovery error is shown as a blocked
  state and requires a deliberate force-stop click.
- Closing a live-server window is an operator shutdown: the GUI cancels the
  immediate close, requests graceful shutdown, waits five seconds, requests
  force shutdown if still live, joins the worker, and only then permits the
  process to exit. The `Drop` fallback also force-signals and joins the worker;
  it does not silently detach a live `ServerHandle`.
- The Server tab can copy the advertised endpoint and base port into Connector
  settings. Connector preparation/launch retains its existing cancellation
  and atomic-file behavior.
- The GUI layer adds no `unsafe`: the workspace remains
  `unsafe_code = "forbid"`. `cargo test --workspace -q`,
  `cargo clippy --workspace --all-targets -- -D warnings`, and release CLI
  build pass at this checkpoint (817 regular tests, 2 local opt-in ignored).

### File sink and complete packet diagnostics (2026-07-30)

- Every CLI or GUI process reserves a new local log at
  `<executable directory>\logs\p5136-<timestamp>-<pid>.log`; the GUI displays
  the exact path. `P5136_LOG_DIR` overrides the directory for a test run.
  Creation/open failure is a startup error, never a silent console-only
  fallback. Files are intentionally not rotated or deleted automatically so a
  just-crashed client run remains available for inspection.
- The file sink independently enables `p5136_packet=debug`, so it captures
  packet records even with the normal `info` terminal level. Its records have
  direction, peer, full length, captured length, and first-word little-endian
  value (the packet hash for logical TCP/Messenger frames),
  and uppercase raw hexadecimal bytes. Login TCP is recorded after decryption
  and before request admission (and after a successful write on output);
  Messenger is likewise logical-frame payload. UDP records the complete
  encrypted wire datagram before decode and after a successful send, including
  malformed ingress that the server drops. Partial/malformed login and
  Messenger wire frames are separately captured before their decoders return.
- Packet diagnostics are bounded by design: payload text is capped at the
  first 4 KiB per record and a process-wide 512-record/second raw-record budget
  prevents remote disk/CPU amplification. A rate-limited interval reports its
  suppressed count before the next retained record. The file sink itself uses
  a bounded, lossy background queue, so a stalled disk never blocks a network
  runtime; a saturated queue may drop newest diagnostic records. Normal client
  traffic retains every packet, but overload output is explicitly not a
  complete capture. Every retained record retains its true byte count and
  explicit `truncated = true` marker. OS-level receive errors that never
  produce a datagram have no payload to record and remain ordinary structured
  errors.
- Raw wire/logical bytes can contain credentials, nicknames, and chat. These
  files are local sensitive diagnostics; they are Git-ignored by virtue of
  being created beside the executable and must not be attached or committed
  without review. Preserve the matching log when repeating the present client
  crash, then delete or archive it by operator choice.
- The first stock-client rerun should keep the server log and the client-side
  crash evidence together. Compare the final `direction=received` login TCP
  record (or absence of one) with the client crash time before changing
  compatibility behavior.
- The 2026-07-30 stock-client crash was traced to the exact four-byte
  `PqGetRiderTaskContext` request (`0x5870084F`). Rust now validates that
  hash-only request and returns the C#-evidenced
  `PrGetRiderTaskContext | i32(0)` reply (`0x58840850`) without mutating the
  profile; repeat the stock-client startup test before treating later traffic
  as a new compatibility issue.
- The next stock-client run passed that response and terminated at the exact
  four-byte `PqRankerInfoPacket` request (`0x41C60708`). Rust now returns the
  profile-backed `PrRankerInfoPacket` body (`status 0 | ranker:u8 | f32 100.0 |
  u32 0`). The adjacent C# startup queries `PqVersusModeRankOnePacket` and
  `PqRiderSchoolExpiredCheck` are also implemented as strict hash-only,
  read-only queries with their exact default replies so the next run does not
  have to discover those two omissions one at a time.

### Audited login and menu initialization path (2026-07-30)

- The next retained run is
  `target\p5136-startup-queries\release\logs\p5136-1785435892564-5036.log`.
  It proves that login, the complete catalog inventory stream, rider
  completion, endpoint reports, and the earlier post-rider replies all
  succeeded. The terminal request was the exact four-byte
  `SpRqGetMaxGiftIdPacket` (`0x5EB4085B`), rejected as an unsupported
  identity-bound packet.
- The C# receive path was audited from `ClientSession.OnPacket` rather than
  treating source order as client order. It reads the hash under the session
  lock, establishes or acquires the identity-generation operation, invokes
  `Korean5136Protocol.TryHandle` first, then the general packet dispatcher,
  and only then its large fallback handler. The P5136 path is therefore:
  server `PcFirstMessage`; client `PqLogin` and server `PrLogin`; client
  `PqGetRider`; complete `LoRpGetRiderItemPacket` inventory stream followed by
  `PrGetRider`; then client-driven post-rider/menu queries. `PqCnAuthenLogin`
  and `PqChannelMovein` are supported identity-establishing alternatives, but
  this retained local run did not emit either one.
- The actual post-rider request order in that run was:
  endpoint reports; `LoPingRequestPacket`; `PqGetRiderTaskContext`;
  `PqGetRiderQuestUX2ndData`; `PqAddTimeEventInitPacket`; countdown/preset
  no-ops; `PqGetGameOption`; `PqRiderSchoolDataPacket`;
  `LoRqGetRiderItemPacket`; time-shop/countdown no-ops;
  `PqRankerInfoPacket`; preset no-op; `ChRequestChStaticRequestPacket`;
  `PqRequestExtradata`; `PqDynamicCommand`; and finally
  `SpRqGetMaxGiftIdPacket`. This order comes from the encrypted-TCP logical
  log, not from the order of C# `if` statements.
- Rust now answers `SpRqGetMaxGiftIdPacket` with the C#-evidenced
  `SpRpGetMaxGiftIdPacket | i32(0)` response (`0x5EA1085A`). The request is
  strict hash-only and remains behind the authenticated identity/profile
  boundary.
- To avoid repeating the same one-packet discovery cycle, the adjacent
  read-only menu/store queries in the C# fallback were audited and implemented
  together:

  | Request | Reply body | Rust state source |
  | --- | --- | --- |
  | `SpRqKoinBalance` | `koin:u32 | 0:u32` | bound profile |
  | `PqFavoriteTrackMapGet` | `theme_count:i32 = 0` | honest empty projection |
  | `SpRqGetCashInventoryPacket` | `count:i32 = 0 | terminal:u8 = 0` | terminal empty |
  | `SpRqRemainCashPacket` | `0:u32 | cash:u32` | bound profile |
  | `SpRqRemainTcCashPacket` | `99:u32 | tc_cash:u32` | bound profile |

  The TC Cash prefix `99` appears in both the stock-era and current C#
  handlers, so it is retained as an established wire constant rather than
  interpreted as mutable state. Favorite tracks are deliberately empty:
  importing the mutable C# favorite-track sidecar without a lease would copy
  the same race that the favorite-item importer was designed to remove.
- All six requests reject truncation, wrong hashes, and trailing bytes. Their
  exact hashes and response bytes have codec goldens, authenticated dispatch
  coverage, and a no-profile-revision assertion. No mutation or economy
  success is implied.
- Evidence boundary: the C# server is a request dispatcher and does not encode
  one deterministic client send order. The retained runtime log is
  authoritative for the sequence above; C# establishes the response shapes
  and the wider set of possible on-demand queries. Gift actions, purchases,
  coupon/exchange pages, and competitive-mode requests remain separate
  feature paths and remain unimplemented/logged no-reply unless already
  classified. The next
  stock-client run must prove progress beyond `SpRqGetMaxGiftIdPacket`; this
  audit does not claim that every later UI path is complete.
- Independent reviewer audit found no P1/P2 issue in the C# wire comparison,
  Rust abstraction boundary, authentication/identity fencing, read-only
  behavior, error propagation, tests, or `unsafe` policy. Workspace
  all-feature validation passes with 819 regular tests and 2 local-data
  opt-in tests ignored; formatting, `git diff --check`, and workspace
  all-target/all-feature Clippy with `-D warnings` also pass.
- The fresh release is
  `target\p5136-initialization\release\p5136.exe` (15,473,152 bytes, SHA-256
  `C01B47262FE5FC258EE7B1B063FD57EE6331A2D084E62647A5349A046E083745`).
  It was built in a distinct target directory so a running older executable
  could not mask the new implementation.
- The C# repository remains unmodified.

### Captured Koin fix and retained-log coverage audit (2026-07-30)

- The next Rust run,
  `target\p5136-initialization\release\logs\p5136-1785436881093-30272.log`,
  passed the gift, cash-inventory, and adjacent menu queries. It terminated on
  a five-byte `SpRqKoinBalance` request:
  `BD 05 4C 2D 01`. The old Rust parser incorrectly treated Koin as a
  hash-only request and rejected the terminal `01` as one trailing byte.
- All 56 Koin requests in 28 retained packet traces have that exact five-byte
  form. Rust now requires the request hash, the observed mode byte `1`, and
  exact exhaustion. Truncation, a different mode byte, or additional bytes
  remain typed protocol errors. The response remains the profile-backed
  `SpRpKoinBalance | koin:u32 | 0:u32`.
- The common captured post-Koin sequence is `PqFavoriteItemGet`,
  `PqLockedItemGet`, `PqFavoriteTrackMapGet`, `PqGetFavoriteChannel`,
  `PqAddTimeEventTimerPacket`, `PqCheckMyClubStatePacket`,
  `LoRqSetRiderItemOnPacket`, and `PqChannelSwitch`. Every one was already
  owned by a Rust item-state/startup/club/equipment/channel domain. The new
  crash was therefore one exact wire-shape error rather than evidence that the
  remaining baseline initialization path was absent.
- The entire external client log directory was audited to avoid another
  one-packet-at-a-time search. The 49 packet-trace files contain 19,496
  incoming records and 100 distinct incoming hashes. Seventy-two were already
  classified by existing Rust transports/domains; the remaining 28 are all
  TCP requests. Their request lengths, C# reply/state requirements, and Rust
  status are recorded in
  [CAPTURED_PACKET_COVERAGE.md](CAPTURED_PACKET_COVERAGE.md).
- Five of the 28 are bounded read-only queries with complete capture/C#
  evidence. Rust now implements exact terminal replies for current
  competition, challenger info, four-ID event-buy counts, training missions,
  and the empty new-career list. Unlike C#, the event-buy query uses no
  mutable process-global request state. All five remain behind the exact
  identity/profile fence and are proven not to change the profile revision.
- `PqStartScenario` and `PqCompleteScenarioSingle` are also fully ported rather
  than treated as compatibility no-ops. Start validates
  `hash | scenario_type:i32`, acquires the identity-bound canonical profile
  lane, durably stores `scenario_type`, reauthorizes the generation, refreshes
  the session snapshot, and only then returns the exact nine-byte reply.
  Completion consumes exactly the captured 22-byte opaque-body length (the C#
  handler does not interpret its values) and returns the bound durable
  `scenario_type` without mutation.
- An early implementation attempt admitted all 28 request hashes and captured
  lengths as successful no-reply compatibility packets. Independent review
  rejected it: room track/AI/slot/chat, X-parts, kart physics, and time-attack
  requests require replies, broadcasts, or durable state changes. That broad
  fallback was removed before commit or release. At that historical
  checkpoint the remaining 21 gaps deliberately returned
  `UnsupportedIdentityPacket` rather than letting state diverge; the current
  retained-corpus checkpoint above supersedes that state and implements all
  21 in their owning domains.
- Independent final review found no P1 issue after that correction. Its P2
  recommendations were applied: scenario identity/profile validation now
  precedes profile-lane admission, opaque completion documentation says
  length-only validation, malformed 21/23-byte and wrong-hash completion tests
  were added, and the then-pending hashes were asserted fail-closed. Those
  assertions were removed only as each owning implementation and regression
  test replaced them.
- Workspace all-feature validation passes with 839 regular tests and two local
  opt-in tests ignored. Formatting, `git diff --check`, workspace
  all-target/all-feature Clippy with `-D warnings`, and the workspace
  `unsafe_code = "forbid"` policy also pass.
- The fresh release is
  `target\p5136-corpus-audit\release\p5136.exe` (15,488,000 bytes, SHA-256
  `FAC9124B4C3640965FCF3BFCC15F1620A0CFA7DCA0A955D2CB322D5139ED3E36`).
  It was built in a distinct target directory so an older running executable
  could not mask the new implementation.
- The C# repository remains unmodified.

### Correct stock P5136 and LAN E2E setup (2026-07-30)

- The local stock P5136 installation is
  `C:\Users\drash\Documents\kartrider\KartRider_5136`, not the
  `HF_20051214_Factory` client copies. Its `KartRider.exe` SHA-256 is the
  connector's exact supported P5136 hash:
  `629F084E2A12C6FA1FF0EA603B90F8768454D13A1BC2DF6A8504F8AA06FD6194`.
  The Factory client copies hash differently and are not valid P5136 E2E
  targets.
- The same installation supplies both required local runtime data paths:
  `Profile\KartCatalog.xml` for `--catalog` and `Data` for
  `--client-data-dir`. Release startup with both paths was smoke-tested on
  `127.0.0.1:49311` through a real messenger probe. The copied Factory Data
  path failed closed because it did not contain the required KR emblem entry.
- One P5136 installation does not support concurrent multi-client launch.
  The first real two-player test must use a second machine on the local LAN,
  with its own supported P5136 installation and a copied release connector.
  Do not race connector patching or launcher-profile writes inside a shared
  game directory.
- At this checkpoint the server host's physical LAN interface is Wi-Fi
  `192.168.1.10/24` (the profile is currently Windows `Public`; re-check this
  value immediately before testing). The current local client XML already
  identifies login as `192.168.1.10:39312`, so use configured base port
  `39311` unless another test requires a different port.
- GUI LAN server values for the first test:

  ```text
  Bind address:                   0.0.0.0
  Advertised IPv4:                 192.168.1.10
  Configured port:                 39311
  Profile root:                    <rust repo>\Profile
  KartCatalog.xml:                 <KartRider_5136>\Profile\KartCatalog.xml
  Client Data directory:           <KartRider_5136>\Data
  Allow new remote nicknames:      checked
  ```

  This binds game UDP `39311`, login TCP and P2P UDP `39312`, and messenger
  TCP `39313`. Before connecting the remote client, authorize those exact
  inbound protocols/ports for the active Windows firewall network profile. No
  firewall rule was created by this port checkpoint.
- On the remote machine, run its release `p5136.exe` GUI only for the
  Connector tab: point Game directory to that machine's P5136 installation,
  set a unique nickname, Server IPv4 to `192.168.1.10`, configured port to
  `39311`, and use its native runner. The remote server must create the new
  profile, so the server-side remote-creation checkbox above is required for
  a fresh remote nickname.

### Safe item-state checkpoint

- Exact request hashes are `LoRqDeleteItemPacket` `0x4F4E07B8`,
  `PqUnLockedItem` `0x27C00565`, `PqFavoriteItemGet` `0x3BAD06B0`, and
  `PqFavoriteItemUpdate` `0x527807F3`. The evidenced reply hashes are
  `LoRpDeleteItemPacket` `0x4F3D07B7`,
  `PrUnLockedItem` `0x27CD0566`, and `PrFavoriteItemGet` `0x3BBD06B1`;
  Favorite Update is one-way.
- Delete and unlock share the observed
  `auth_scalar:u32 | credential_count:u32` prefix. Rust accepts only the stock
  producer's zero scalar and empty credential list before reading any
  credential string. Delete then consumes four `u16` values; unlock consumes
  one terminal zero byte. Every request requires exact exhaustion.
- Rust exposes no delete/unlock success serializer. The stock client treats
  either reply as a capability to perform a local transition, while the C#
  server acknowledges deletion without durable server deletion and unlock
  accepts a reply even when its authentication/state semantics are not
  enforced. Valid requests therefore remain connected but unanswered; invalid
  requests return typed protocol errors.
- Favorite Update is exactly
  `hash | scope:u8=1 | count:u32 | count * (category:u16 | item_id:u16 |
  serial:u16 | operation:u8)`. One stock batch is capped at 200 records before
  allocation. Operations 1/2 are Add/Remove. Repeated keys are legal and are
  applied sequentially in wire order; stock evidence does not establish a
  uniqueness invariant.
- Favorite Get is exact hash-only. Its reply is
  `hash | count:u32 | count * (category:u16 | item_id:u16 | serial:u16 |
  state:u8=0)`. The aggregate collection cap is independent of the 200-record
  update cap and is derived from the configured login payload. At the default
  1 MiB payload it is 149,795 records.
- `FavoriteItems` is a private-vector, stable-order, unique persisted
  abstraction. Whole batches are applied purely in O(N+B), with idempotent
  Add/Remove semantics, final-result cap validation, and no partial mutation.
  Persistence uses one optimistic profile transaction and exact immutable
  durability confirmation. A repeated successful request reuses its revision.
- `ProfileStore::transaction_with_context` is an additive, read-only snapshot
  abstraction that exposes only the optional source revision. It lets an
  already-populated legacy `Launcher.json` be published as immutable revision
  1 even when an idempotent update changes no fields. The original
  `transaction` API and shared CAS loop remain intact.
- The bound session cache now patches only the favorite projection and exact
  revision after the post-write identity fence; unrelated cached projections
  are preserved. Favorite persistence errors remain concrete and typed at the
  public session boundary rather than being erased behind `dyn Error`.
- `Profile.favorite_items: Option<FavoriteItems>` is the migration marker:
  `None` means the external C# sidecar decision is unresolved, while
  `Some(empty/list)` is canonical Rust state. When it is `None`, the importer
  captures `Favorite.json` exactly once under the in-process profile lock,
  parses the strict ordered `{ItemCatID, ItemID, ItemSN}` array (optional UTF-8
  BOM accepted), applies the incoming batch, and seals `Some(result)` in the
  same immutable revision. A missing sidecar alone means empty; null, malformed,
  duplicate, oversized, non-regular, symlink/reparse, or byte-cap-exceeding
  sidecars fail closed and leave the marker unresolved.
- `RaceRunLease` now retains a `cap_std::fs::Dir` for the canonical root.
  Import opens the profile directory with `open_dir_nofollow`, probes its final
  sidecar entry without following links, then reopens it with final-component
  no-follow plus nonblocking mode and validates the opened handle is a regular
  file. Reads are capped at `ProfileStore`'s configured byte maximum and use a
  sentinel byte to detect growth. No unsafe code is added. The diagnostic
  `PathBuf` is never used as an I/O authority.
- The initial sidecar candidate is held across optimistic CAS retries. A retry
  with no marker reuses that same candidate; a competing canonical marker wins
  over it. Imported state must fit the current configured favorite reply cap
  before Rust seals it, while already-canonical over-cap state keeps the
  existing shrink-only recovery rule.
- The sidecar is preserved and never read after a Rust marker exists. C# and
  any other external profile writer must be stopped during the one-time
  migration: they do not honor the Rust lease. The remaining documented local
  filesystem assumption is a stable, operator-owned profile root; defending a
  hostile Unix root rename/replace race requires converting the whole legacy
  profile load/CAS backend to capability-relative I/O, beyond this slice.
- Real encrypted TCP now covers login -> one-way Update -> Get on the same
  connection -> disconnect/identity release -> reconnect/relogin -> identical
  Get. It independently parses the reply hash/count/items/state/EOF and checks
  immutable revision one, imported order, and the first Update's no-reply IV
  behavior.
- Checkpoint gates passed on Windows: 815 regular tests, both opt-in local-data
  tests, workspace/all-target/all-feature Clippy with `-D warnings`, formatting,
  and `git diff --check`. Workspace `unsafe_code = "forbid"` remains active;
  the new production path has no `unsafe`, panic, `unwrap`, or `expect`.
  Independent abstraction/error/durability reviews found no commit-blocking
  P0/P1 issue after the fail-closed marker and cache fixes.

### Strict read-only club-query boundary

- The exact request/reply name-hash pairs are:
  `PqCheckMyClubStatePacket` `0x71740944` /
  `PrCheckMyClubStatePacket` `0x718B0945`;
  `PqGetUserWaitingJoinClubPacket` `0xB4C50BC1` /
  `PrGetUserWaitingJoinClubPacket` `0xB4E20BC2`;
  `PqCheckCreateClubConditionPacket` `0xC9790C78` /
  `PrCheckCreateClubConditionPacket` `0xC9980C79`;
  `PqGetClubListCountPacket` `0x72C90964` /
  `PrGetClubListCountPacket` `0x72E00965`; and
  `PqGetClubWaitingCrewCountPacket` `0xBF5E0C2C` /
  `PrGetClubWaitingCrewCountPacket` `0xBF7C0C2D`.
- The first three requests are exactly hash-only. Club-list count is
  `hash | club-name-filter UTF-16 | club-master-filter UTF-16`, for
  `12 + 2 * (name_units + master_units)` bytes. Waiting-crew count is exactly
  `hash | club_code:u32`, for eight bytes. Rust rejects club code zero because
  the stock producer does not send the request without a selected club, while
  preserving every nonzero 32-bit bit pattern with `NonZeroU32`.
- Club-list filters are bounded before allocation at 64 club-name UTF-16 units
  and 32 master-nickname UTF-16 units. Those values reuse existing Rust domain
  invariants as a conservative resource policy; they are not presented as a
  static stock serializer limit. Parsed fields and constructors remain
  private, and the parsed request omits `Debug` so user-entered filters are not
  accidentally logged.
- `PrCheckMyClubStatePacket` is the complete 31-byte
  `club_code:u32 | club_name UTF-16 | logo:u32 | line:u32 | grade:u16 |
  nickname UTF-16 | member:u32 | level:u8` layout. Rust emits code zero and
  correctly typed empty/default trailing fields. The stock consumer treats
  code zero as no membership and ignores or resets the remaining fields.
- `PrGetUserWaitingJoinClubPacket` is
  `lookup_success:u32 | pending_club_code:u32 | club_name UTF-16`. Rust emits
  `(1, 0, "")`: the query succeeded and there is no pending join. Emitting
  status zero would instead report a lookup failure and prematurely stop the
  client flow. The empty final value is modeled as a UTF-16 string even though
  its zero length is byte-identical to the C# handler's integer zero.
- Create-condition emits status 3. Status zero would enter creation, one and
  two claim specific RP/Lucci shortages, and four follows a refresh path;
  status 3 is the consumer-evidenced generic unavailable result. Club-list
  count emits `(0, 0)` rather than C#'s fabricated total. The stock client
  applies a local page-count fallback when the first count is zero, so this is
  safe but does not yet prove a literal zero-club UI; the future empty
  `PqSearchClubListPacket` flow and stock E2E remain validation gaps.
  Waiting-crew count emits `(0, 0)`, making `current < capacity` false and
  preventing a join without inventing a pseudo-capacity.
- Full reply SHA-256 values are
  `30ff57e681453da377357a5f9012f12bd956546224299e7e101927b7c38eeaf1`
  (no club),
  `1c405d2e5e0488e12e76810dc28755cbd3a2121e9e13757f5fb945fabf779263`
  (no pending join),
  `156e6d86242ad83c166b934584a40a977785e90c6372450ed836be0c657c2615`
  (create unavailable),
  `4803dd15f50145a10f181d859e9d60074c56374f84a1bea4f265eb4d0983780d`
  (empty list count), and
  `7d6833ca5f580bbc6c1796a4e066782ed44822dd72d8be1d1c24a5cdfb188b39`
  (waiting capacity unavailable).
- The C# handlers ignore bodies and trailing data, fabricate default club
  membership and list/capacity counts, report create success without a club
  subsystem, and can mutate/persist `ClubMark_LOGO` during a waiting-state
  query. None of those behaviors are cloned. The stability audit has no
  packet-specific club finding, but its state-ownership, validation, and
  failure-handling rules support this stricter Rust boundary.
- Tests pin all ten hashes, complete request and reply bytes/digests, every
  truncated prefix, wrong and cross-kind hashes, negative string lengths,
  trailing data, UTF-16 unit limits including surrogate pairs, zero and full
  nonzero club-code domains, unbound-profile error ordering, in-memory and
  durable profile immutability, identity/session continuity, a live follow-up
  request, stale migration ownership, and quiesce priority. Two independent
  read-only reviews found no P0-P3 issue in the protocol, abstraction,
  ordering, error propagation, or `unsafe` policy.
- The direct P5136 request-disposition ledger is now 40 of 40 explicit after
  the later item-state checkpoint. The deliberate compatibility no-op table
  remains 25 of 25 explicit. Successful club membership, creation, join,
  search, and rename remain deferred until an actor-owned repository and
  atomic namespace, membership, authorization, and durability rules are
  designed.

### Fail-closed rider-info lookup

- `PqGetRiderInfo` is `0x27770563`; `PrGetRiderInfo` is `0x27840564`.
  The ordinary stock producer writes
  `u32 scalar 0 | empty reserved UTF-16 string | bounded target UTF-16 string |
  raw u8 mode`, for `17 + 2 * target_utf16_units` total bytes.
- Rust accepts only scalar zero and an exactly empty reserved string, bounds
  the target before allocation, preserves the complete `u8` mode domain, and
  requires complete consumption. Nonzero scalar, negative/nonempty reserved
  length, truncation, invalid or oversized target encoding, wrong hash, and
  trailing bytes remain distinct typed errors.
- The parsed request has private fields and getter-only access. It deliberately
  does not implement `Debug`, and the session handler neither reads nor logs
  the target nickname or mode.
- The only exposed reply is the exact five-byte failure
  `[64 05 84 27 00]`. It clears the stock client's pending lookup and reports
  an unknown rider without disclosing whether a local or offline profile
  exists. The handler has no profile-coordinator parameter and performs no
  target-profile read, creation, persistence, request-specific World command,
  mutation, retry, or fanout.
- A successful response is intentionally deferred. The available response
  schema has 44 fields, while the current C# compatibility serializer omits
  ten tail bytes. Its nonzero-scalar branch corresponds to the derived couple
  request rather than the ordinary producer. Rust does not expose either
  malformed success projection until public-field, privacy, offline lookup,
  and repository policy are explicit.
- Global admission precedes parsing. For a live admitted identity, exact
  parsing precedes `AuthorizeIdentity` and the bound-profile fence; malformed
  plus unbound therefore returns `RiderInfoProtocolError`. Stale ownership and
  quiesce remain outer errors and return `StaleSession` or
  `OutboundProductionClosed` before this parser runs.
- Tests pin both hashes, the exact stock request and failure bytes, every
  truncated prefix, scalar/reserved invariants, UTF-16 unit bounds including
  surrogate pairs, raw mode boundary values, trailing rejection, authenticated
  fail-closed dispatch, in-memory and durable profile immutability, follow-up
  session liveness, and stale/quiesce priority.

### Canonical rider-school start

- `PqStartRiderSchool` is `0x4327072D`; `PrStartRiderSchool` is
  `0x4338072E`. The stock producer writes exactly one encoded `u8` after the
  request hash, so the request is exactly five bytes. Rust decodes and
  preserves the full `u8` domain without inventing an undocumented business
  range, then rejects every truncated or trailing shape the C# handler
  ignored.
- The reply is exactly `hash | raw status 1 | 235-byte P5136 kart-physics
  block`, for 240 total bytes. Serialization is fallible and
  `KartPhysicsBuildError` propagates through `LoginSessionError`; no panic or
  default-on-error path can emit a partial reply.
- Rust reuses `build_p5136_kart_physics_block` with the validated S7 baseline.
  The normal formula yields `2304.0` and `3745.587890625` at physics offsets
  138 and 142. The compatibility shortcut instead hardcodes `2305.0` and
  `3745.0`; that isolated drift and the mutable global `SpeedPatch` dependency
  are deliberately not cloned. The canonical full-packet SHA-256 is
  `52f16bc897e349ad220b226f3563653cb02718a2a2827076249ece194104ad9e`.
- The opaque request byte currently selects no server state because neither
  producer nor consumer evidence establishes its meaning. After exact parsing,
  the request passes identity/profile fences and returns the canonical direct
  reply without profile I/O, revision change, request-specific World command,
  mutation, retry, or fanout.
- Tests cover both hashes, exhaustive encoded-byte decoding, all truncations,
  wrong hash and trailing rejection, exact response length/status/physics
  fields/digest, unbound-profile ordering, authenticated direct dispatch,
  profile immutability, follow-up liveness, stale migration ownership, and
  quiesce.
- Room and single-player kart physics now resolve the Korean P5136 flying-pet
  table (80 immutable IDs) before building the 235-byte block, matching the C#
  `FlyingPetSpec` additions. Normal-pet defense remains client-authoritative:
  Rust persists and broadcasts `Set_Pet` at rider-item offset 26 and relays the
  client's type-11 reaction/mask without rolling `itemTable@kr.xml` a second
  time. Unknown flying-pet IDs retain the C# zero-spec behavior and are logged
  as a typed fallback.
- The direct P5136 request-disposition ledger is now 40 of 40 explicit after
  the later item-state checkpoint. The deliberate compatibility no-op table
  remains 25 of 25 explicit. Stock-client school-start E2E remains an open
  validation gate.

### Strict stateless compatibility replies

- `PqRequestExtradata` is `0x44660748`; its producer constructs only the base
  packet, so the logical request is exactly four bytes. `PrRequestExtradata` is
  `0x44770749` and its safe fixed body is exactly `u8 code 0 | u8 optional
  absent`, for six total bytes.
- The stock reply codec can represent an optional value for another code, and
  the client compares the code with 99. The server-side business meaning and
  value policy are not established, so Rust exposes only the evidenced code-0
  absent-value reply and does not invent a success API or optional string.
- `PqWebEventCompleteCheckPacket` is `0xA8140B50` and is likewise an exact
  four-byte base-only request. `PrWebEventCompleteCheckPacket` is
  `0xA8300B51` with no body, also exactly four bytes. The client consumes it to
  clear its pending state and advance the corresponding local state machine.
- Both request producers allocate the base packet and send it without a field
  write. Rust therefore uses the shared strict hash-only parser and complete
  consumption. Every 0–3-byte prefix, wrong hash, and trailing byte is a typed
  protocol error; the C# handlers' acceptance of a suffix is not cloned.
- Global identity-operation admission is outermost. For a live admitted
  identity, strict parsing precedes `AuthorizeIdentity` and the bound-profile
  fence, so malformed plus unbound returns the protocol error. A stale owner
  or quiesced producer is rejected during global admission before the
  packet-specific parser, preserving `StaleSession` or
  `OutboundProductionClosed`.
- Valid requests then pass `AuthorizeIdentity` and the bound-profile fence and
  return one direct reply. They create no request-specific World command,
  profile-store I/O, profile revision, shared-state mutation, persistence,
  retry queue, or peer publication.
- Tests pin all four hashes, both exact request/reply byte sequences, strict
  truncation/wrong-hash/trailing rejection, both authenticated dispatch paths,
  profile and revision immutability, identity continuity, a live follow-up
  request, unbound profile behavior, stale migration ownership, quiesce, and
  the malformed/error-priority combinations.
- These were tracked as additional shared startup-handler gaps rather than
  members of the 40 direct P5136 request-disposition set. That direct ledger
  was therefore unchanged by this slice. After the later rider-info,
  rider-school, club-query, and item-state work, the current ledger is 40 of
  40 explicit; the deliberate no-op table remains 25 of 25 explicit.
- Stock analysis artifacts remain outside this repository. Stock-client E2E
  behavior and the successful extra-data value policy remain open evidence
  gaps.

### Fail-closed P5136 shop buys

- `SpReqNormalShopBuyItemPacket` is `0x9E700B05` with the exact body
  `stock_id:i32 | unknown:i32 | mode:u8`, for 9 body bytes and 13 total bytes.
  `SpReqItemPresetShopBuyItemPacket` is `0xCE5F0C9E` and appends one trailing
  `preset_or_slot:u16`, for 11 body bytes and 15 total bytes.
- The stock executable's two request producers and serializers establish those
  widths and ordering. Rust preserves the full scalar domains and does not
  invent allowed ranges or business meaning for `unknown`, `mode`, or the
  trailing `u16`. No proprietary executable, capture, or external analysis
  artifact is copied into this repository.
- The same hash classifier gates dispatch and is reused inside the
  exact-consumption parser. Its parsed representation has private fields and a
  private tagged variant, so callers cannot construct a normal request with a
  preset value or a preset request without one. Every truncated prefix,
  cross-kind length mismatch, unknown hash, and trailing byte is rejected.
- Both aliases return `SpRepBuyItemPacket` (`0x415B0701`) with the exact body
  `u8 1 | 24 zero bytes`, for 29 total bytes. The server does not echo parsed
  fields or raw request data and does not create a shop-specific World command,
  mutate economy/profile state, persist, or publish to peers.
- Shared identity-operation admission, `AuthorizeIdentity`, and the bound
  profile fence run before parsing and the direct response. Once those fences
  succeed, wire truncation and trailing bytes produce only a bounded metadata
  log and no reply, then the session can handle another request. An impossible
  classifier/parser hash disagreement remains a typed fatal invariant path.
  Stale generation, unbound profile, quiesce, actor termination, and other
  system errors are never downgraded.
- Tests cover exact hashes, both golden request layouts and decoded boundary
  values, every truncated prefix, cross-kind drift, trailing input, the exact
  failure bytes, both authenticated dispatch aliases, session liveness after
  malformed input, identity continuity, stale migration ownership, and
  quiesce rejection. Stock-client purchase UI/E2E behavior and the meanings of
  the unknown fields remain open evidence gaps.
- These two aliases contributed to the earlier 30-of-40 checkpoint. The
  current direct P5136 request-disposition audit is 40 of 40 explicit after
  rider-info, rider-school, club-query, and item-state integration. The
  separate deliberate compatibility no-op table remains 25 of 25 explicit.
- `LoRqDeleteItemPacket` and `PqUnLockedItem` are now explicit safe no-reply
  boundaries, so Rust does not copy C# success-without-state-transition
  behavior. `PqFavoriteItemUpdate` is strictly parsed and atomically persisted
  after either canonical-state loading or the bounded, lease-bound sidecar
  migration; invalid unresolved sidecars fail closed without sealing a marker.

### Authenticated legacy server time

- `PqServerTime` is `0x1E9204C7`; `PrServerTime` is `0x1E9D04C8`. The reply is
  exactly `days_since_1900:u16 | quarter_seconds:u16` after the packet hash,
  for a total of eight bytes.
- C# writes both values as signed `short`, but its unchecked date overflow has
  the same little-endian bit pattern as Rust's `u16` day value reduced modulo
  65,536. Quarter-seconds are floor-seconds divided by four and remain in
  `0..21_600`.
- Current, stock-era, and legacy C# evidence agrees on the reply shape. The
  request handlers dispatch from the already-read hash without consuming a
  body. No checked-in producer or capture proves hash-only exhaustion, so Rust
  accepts an unused suffix only within the existing bounded login frame and
  does not copy it.
- The packet is identity-bound like every authenticated Rust request. Exact
  generation authorization and the bound profile fence run before the direct
  requester reply. The shared identity-operation admission and
  `AuthorizeIdentity` actor commands are its only World interactions; there is
  no ServerTime-specific command, shared-state mutation, profile-store I/O,
  peer publication, or retry queue.
- Tests pin both hashes, the exact little-endian body for fixed time values,
  hash-only and unused-suffix dispatch, the eight-byte response shape, clock
  range, and continued identity ownership. Stock-client request-body and E2E
  evidence remain open rather than being inferred from C#'s non-consumption.

### Bounded TCP GameSlot relay

- `GameSlotPacket` is `0x27C00574` and its TCP common envelope is
  `hash:u32 | claimed_player_id:i32 | item_or_mask:u32 | type:u8`. Rust caps
  the complete logical TCP packet at 1013 bytes and every nested blob at 960
  bytes before copying it. This codec is independent of the opaque UDP relay
  envelope that happens to use the same packet name; the TCP cap is never
  applied to UDP movement traffic.
- The accepted Korean P5136 wire types are `1`, `2`, `4`, `5`, `6`, `7`,
  `8`, `9`, `10`, `11`, `12`, `13`, `16`, and `17`. Claimed player IDs must
  be in `0..=15`;
  type-specific masks, declared lengths, complete consumption, item-vector
  counts, object IDs, known operation hashes, and finite coordinates are
  checked before an actor command exists. Unsupported types are nonfatal
  no-side-effect drops.
- Type 12 first validates the complete bounded envelope, claimed player ID,
  low-16 mask, zero native-reserved outer word, and exact declared payload
  length. It then admits a known operation/base pair from the 80 native writer
  schemas or five additional C# `PacketName` enum-derived `Gop*`/`GoItem*`
  pairs. Unknown pairs, malformed envelopes, invalid masks, and bodies above
  960 bytes fail closed.
- A body that fits one of the 80 recovered writer shapes retains typed object,
  state, and retained/static/default evidence for logs and tests. Shape drift
  and C#-only named pairs become an explicit `BoundedItemOperation`: opaque,
  bounded, byte-preserving, and relay-only. This replaces the prior
  capture-by-capture `EvidencePending` routing gate. Only strictly decoded
  bodies enter the common object registry; fallback bodies do not create
  Rust-side lifecycle state.
- `GopBarricade` remains deliberately stricter because it creates or updates a
  server-observable world object. State 1 validates object ID, owner, reserved
  field, and all twelve finite transform floats. States 2 and 3 validate the
  nested owner as an active frozen racer but do not require it to equal the
  outer sender: retained traffic proves that a remote victim reports state 2
  while the nested owner remains the installer. State 3 is the native phase-4
  post-impact transition; state 4 carries the terminal phase-5/6 variant. The
  shorter transitions retain their exact bytes.
- Types 4 and 6 preserve the fixed collection envelope and nested
  `GopLucci`/`GopBonusItem` body, including matching nonmissing object IDs,
  state 1, bounded collector ID, ticks, finite position, variant, and exact
  25-byte body. Types 5/7/8 likewise validate the exact `GopTeamFlag` state-1
  attach, state-3 drop/position, and state-4 return/position bodies. These five
  world-object transitions deliberately remain non-mutating
  `EvidencePending` diagnostics: Lucci/bonus-item world objects and team flags
  are outside the supported gameplay scope, so no spawn/ownership registry is
  planned unless that scope is explicitly reopened.
- Type 13 accepts only the exact empty nested body emitted by `sub_9E33E0`.
  Because no client receiver consumes it, the World actor authenticates and
  consumes the notification without reply, relay, or mutation. Type 16 shares
  the full type-10 codec but keeps its original type and never runs type-10
  held-slot behavior.
- Type 17 validates nested `GameKartPacket` (`0x27250564`) and
  `GameKartQuadPacket` (`0x406006EF`) with their native flag-derived length
  formula. Full precision is `116 + optional 16/4/4`; quad is
  `76 + optional 4/2/2`. Accepted movement fallback frames are relayed
  byte-for-byte only to the sender-excluding low-16 peer mask.
- `ParsedGameSlotPacket` is a parser-minted, move-only capability. Its raw
  bytes, actor action, body, claimed ID, and mask are private; read-only
  accessors expose the validated facts and `into_raw(self)` consumes the
  capability. Allocation of the owned raw packet occurs only after all wire
  checks succeed. This prevents another crate from changing a pickup into a
  relay action or accidentally cloning one accepted command into two actor
  publications.
- The World actor reauthorizes the admitted identity, finds the exact frozen
  generation, requires a human racer source, and compares the claimed player
  ID with the actor-owned frozen slot. Observers remain receive-only. Lobby
  and Loading reject item traffic; Running accepts it, and Settling accepts it
  only while the deadline is open and finalization is still
  `AwaitingDeadline`. The open Settling path is required because Rust enters
  Settling at the first finish while other racers may still send late item
  events.
- Strict type-12 operations use `(race_epoch, object_id)` as the registry key
  and retain the operation/base class, original owner identity generation,
  last reporter, semantic meaning, source, target, transition token, phase,
  and last native state. There is no protocol-wide numeric lifecycle: the
  recovered class contract decides initialize/place/launch/impact/resolve/
  retarget/remove/rebind/respawn. Barricade 0 -> 1 and Mine 5 -> 6 are explicit
  valid transitions. Repeated exact impacts and removals are normal logged
  suppressions with no fanout; unsupported post-terminal updates are also
  suppressed except for proven respawn. Producer-backed orphan terminal
  transitions may create bounded tombstones, but consumer-only terminal
  evidence cannot mint an unseen record. Class or explicit-owner rebinding
  fails closed, and the registry is capped at 1,024 entries per race progress
  instance.
- Unknown semantics, explicit client no-op branches, and
  `BoundedItemOperation` fallback bodies remain byte-exact relay-only. They do
  not create or mutate authoritative records and therefore cannot erase an
  impact fingerprint or inherit meaning from the same numeric state in a
  different class.
- Type 9 requires its sender-inclusive held-item synchronization mask. Types
  10 and 16 permit the empty solo audience or a low-16 remote-peer mask but
  reject the sender bit; type 11 requires a nonempty remote-peer mask. All
  three relay only to masked peers and never echo the sender. Type 10 preserves
  its full `u16` status and other producer fields. For every admitted
  type-12 form, the World actor derives the expected mask
  from all other active exact-generation non-observer racers and rejects any
  omission or extra bit; accepted bytes go to every active exact-generation
  peer except the sender, including observers. Missing, migrated, released,
  or replacement generations are never substituted.
- The older static `SendGameSlotBroadcast` design note describes a
  sender-inclusive all-active-player mask. The retained encrypted two-client
  traces are stronger runtime evidence for the implemented type-12 path:
  player 0 sends mask `0x2`, player 1 sends `0x1`, and a solo sender sends
  zero. Rust therefore requires the peer-racer mask and excludes the sender.
  Observer delivery follows the room-observation fanout, but an
  observer-present stock trace is still an explicit verification gap.
- File-correlated runtime evidence prevents treating a zero type-16 mask as a
  room-global broadcast: all 86 type-16 captures came from two solo logs,
  while multi-client type-10 logs used the opposite player's bit. The newest
  log adds 174 type-17 quad frames, all 96 bytes outer / 76 bytes nested, from
  player 0 to mask `0x2`; all satisfy the recovered native length formula.
- All recipient queue permits are reserved before the first publication. One
  full queue drops the whole time-sensitive event, releases earlier permits,
  leaves race state including the object registry unchanged, and does not
  enqueue a heartbeat retry. For a tracked type-12 transition, registry commit
  happens only after all permits are held and before their infallible publish.
  An
  empty audience is a valid zero-recipient outcome. Quiesce continues to block
  the enclosing `WorldCommand::Race` before publication.
- Valid type-1/type-2 pickup frames are not relayed. The parser mints a
  synthesis-only capability because the request's live-rank field occupies the
  response item-ID field. Captured pre-award state plus repeated object, tick,
  state, and owner fields must all agree. In game types 2/4, the actor enforces
  per-player monotonic replay tokens and a capture-sized rate bucket, draws
  from its immutable probability snapshot, consumes the capability to patch
  offsets 38..40, and atomically broadcasts the exact 73-byte success packet
  to all active frozen recipients including the sender. `Live` follows the
  explicit rank trust policy: client-reported C# mapping for the current
  LAN/friends default, or Combined fallback when disabled. Speed rooms reject
  synthesis.
- Rust still omits the C# type-10/type-11 kart side effects, bonus-item
  synthesis, and actor-owned held-slot/use and reaction effects. Bonus-item world-object
  gameplay is explicitly out of scope. Kart-specific pickup
  remapping is now derived from the bounded catalog and is applied exactly
  once after base selection. The stability audit identifies double
  transformation and synthetic packet behavior as failure risks; the
  remaining effects stay separate rather than reproducing those defects.
- GameSlot wire errors, unsupported P5136 types, wrong phase, spoofing,
  observer source, inactive frozen membership, closed settlement, and outbound
  saturation are structured nonfatal drops. Stale global identity ownership,
  actor termination, invariant failures, quiesce closure, and an impossible
  command/outcome mismatch still propagate. Ordinary runtime events include
  bounded metadata and typed reasons; the dedicated local `p5136_packet` file
  sink additionally retains bounded raw packet diagnostics as documented
  above.
- The opt-in external audit sends the older 1,471 retained TCP GameSlot RX
  records through the strict parser: type 1=`43`, type 2=`22`, type
  9=`1,337`, type 10=`38`, type 11=`1`, and type 12=`30`. The external
  capture directory remains uncommitted. The separate newest-log audit adds
  174 type-17 frames. This proves parser compatibility with those corpora. The
  79 class-specific type-12 field contracts instead come from the pinned
  producer/writer/consumer IDB join (75 have named lifecycle meanings); the
  scope-excluded Lucci semantics, kart side effects,
  visible stock-client pickup E2E, and stock-client multiplayer E2E are still
  open.

### Canonical protected-item state

- `PqLockedItemGet` (`0x2D8105C2`) is treated as a strict hash-only request.
  The retained stock-client corpus confirms the exact four-byte producer.
- `PrLockedItemGet` (`0x2D8F05C3`) begins with a signed `i32 count`. Both C#
  handlers default to count zero; Rust now serializes the same bounded
  seven-byte record projection used by the client when canonical locked items
  are nonempty.
- Truncation, the wrong hash, or trailing bytes produce a typed protocol error.
  Get/Update then pass the normal identity-operation and exact profile-binding
  fences and use one canonical profile lane.
- The retained nine-byte `PqLockedItemUpdate` shape is
  `hash | scope:u8=1 | count:u32=0`; the shared strict batch codec also
  supports bounded Add/Remove records. Updates are atomic, idempotent ordered
  set transitions and publish session cache state only after immutable
  durability confirmation.
- `Profile.locked_items: Option<LockedItems>` is the migration marker. `None`
  captures a missing or strict bounded C# `Locked.json` exactly once through
  the run lease and no-follow profile capability. Import plus the incoming
  batch are sealed in one revision; malformed, oversized, nonregular, or
  linked candidates fail closed and leave the marker unresolved.

### Generation-bound client P2P reports

- `ChClientP2pAddrPacket` and `ChClientUdpAddrPacket` are exact ten-byte
  packets: the four-byte packet-name hash, four client-claimed IPv4 bytes, and
  a little-endian `u16` port. Truncation at every boundary, trailing data, an
  unknown hash, or the other report kind's hash fails before profile admission
  or actor mutation.
- The codec consumes but does not expose the claimed IPv4 bytes. Rust derives
  the wire address only from the authenticated TCP peer. Direct IPv4 and
  IPv4-mapped IPv6 peers retain that IPv4 address; native IPv6 peers are
  deliberately unadvertised as `(0.0.0.0, 0)` because P5136 room packets
  cannot represent IPv6. A native IPv6 peer must never become the false
  capability `0.0.0.0:<nonzero>`.
- The Game-UDP report is strict validation-only and has no reply. Observed UDP
  ingress remains the authority for the separate game transport table; a TCP
  self-report is not proof of NAT reachability and cannot overwrite the first
  UDP bind.
- An accepted P2P UDP datagram also supplies a runtime-only P2P source port
  for its exact active identity. Admission has already checked the source IP,
  transport kind, logical arrival epoch, and generation, so that observed
  `u16` takes precedence in live room/MyRoom slot projections when a stock
  client omits `ChClientP2pAddrPacket`. Rust combines that authenticated UDP
  source port with the authenticated TCP IPv4 address; it never trusts the
  datagram payload or the client-claimed IPv4 bytes.
- The P2P report persists only the `u16` port through the canonical profile
  lane, `ProfileStore::transaction`, exact immutable-receipt confirmation, and
  a pre-reserved actor completion slot. Once submitted, the write and terminal
  publication survive requester cancellation. An identical value reuses the
  existing revision; port zero is an absolute durable clear. A durable report
  remains a presentation fallback and cannot overwrite an authenticated
  runtime P2P observation.
- Runtime endpoint authority is separate from the historical profile field.
  Every login generation and same-channel replacement starts with runtime port
  zero, regardless of the value loaded from disk. The observed P2P source is
  cleared with that identity on migration, release, and reconnect. Later
  profile/equipment, reward, and `PqGetRider` refreshes preserve only the
  exact live generation's runtime value and cannot resurrect a stored port.
- After durability, World revalidates the exact identity binding. Only an
  actively owned exact generation updates runtime state. Ownerless,
  superseded, or released outcomes remain durable but do not revive a cache or
  receive a stale success path.
- In one actor turn, an accepted active report updates the ordinary protocol
  room projection, including observers, and every current MyRoom owner/visitor
  role for that identity. Other presentation fields remain role-specific.
  An absent-owner tombstone stays unadvertised, and an exact duplicate causes
  neither a topology change nor a MyRoom revision increment.
- The report has no client ACK and emits no invented peer fanout. Publication
  does not reserve an outbound queue, so saturation cannot prevent the durable
  cache update. A peer that already received an older room snapshot may retain
  it until a later normal snapshot; capture evidence is required before adding
  a live endpoint-refresh packet.
- Same-channel replacement retains lobby/MyRoom membership for the next
  lobby, clears its advertised endpoint, and does not inherit a frozen
  Loading/Running race generation or result authority. The old exact race
  generation becomes inactive and follows the existing abort/DNF settlement
  policy.
- The C# direct-P2P/Game-UDP shortcut trusts client-reported endpoint data and
  may skip relay without proving reachability. Rust does not reproduce that
  behavior: it keeps relay routing transport-local, uses an authenticated P2P
  source port only for the exact live room-slot presentation, and never
  persists a transient NAT binding.

### LAN relay readiness (2026-08-01)

- Two-machine capture `p5136-1785562566890-30896.log` confirms the server
  forwards the real 112-byte GameSlot UDP envelope from `192.168.1.10:63367`
  to `192.168.1.15:61002`; the relay is not a missing P2P implementation.
  The inverse direction exposed two compatibility gaps instead: `.15` sent
  P2P UDP from `61003` without a TCP P2P-port report, and after start it
  echoed UDP time-sync but did not send its own 24-byte time-sync request.
- `GameControl(state=0)` only arms the frozen-roster readiness handshake, as
  in C#. A successful `PrUdpTimeSync` send marks that exact participant ready;
  all-ready schedules the normal one-second-delayed `RaceStart`, while the
  bounded 30-second fallback remains for a client that never originates its
  own time-sync request.
- Capture `p5136-1785767798854-25476.log` disproved the temporary eager-ready
  experiment: `GrCommandStart` reached both clients at `14:43:11.023`, their
  state-0 controls arrived at `14:43:12.815` and `14:43:15.147`, and Rust sent
  state 1 at `14:43:16.156` before UDP preparation completed. Both clients
  subsequently disconnected. The state-0 readiness shortcut was therefore
  removed; duplicate state-0 controls remain idempotent and cannot advance the
  ready set.
- Regressions cover authenticated P2P-source publication without a TCP report
  (including migration cleanup and durable-report precedence), plus the C#
  loading gate where the ready set stays empty until an exact successful UDP
  time-sync outcome.

### MyRoom emblem catalog and main selection

- `RmRequestEmblemsPacket` (`0x63F508C5`) is an exact hash-only request.
  `RmOwnerEmblemPacket` (`0x49AF0774`) is `i32 1`, `i32 1`, an `i32` count,
  then that many source-ordered `i16` IDs. Count, packet size, XML size,
  attributes, duplicates, and catalog entries are bounded.
- Catalog definitions are immutable after startup. A source-ordered
  `Vec<i16>` is retained for the wire response and a separate `HashSet<i16>`
  supplies constant-time selection validation. Definition IDs must be
  positive; zero is reserved solely as the client's empty-selection sentinel.
  Missing catalog data yields an exact empty response and permits only
  `0,0,0` updates.
- Owners and visitors to a public item surface may request the list. A
  protected visitor needs the exact kind-1 one-shot grant scoped to requester,
  owner, resource, and room-info revision. A matching attempt consumes the
  grant in the actor turn before response reservation, including queue-full
  failure. A grant for another resource is preserved; outsiders and stale
  plans receive no packet. The shared owner-resource policy retains access for
  a public owner-Secede tombstone, while a protected owner tombstone is denied
  after its grants are revoked.
- The C# response exposes its process-global definition list as though it were
  the current owner's emblem inventory. Rust currently preserves that
  client-visible list policy behind stricter authorization because no separate
  owned-emblem profile model exists. Definition validation and authorization
  are separate abstractions so an evidence-backed ownership model can replace
  the list source without weakening either boundary.
- `RmRqUpdateMainEmblemPacket` (`0x867F0A14`) is exactly three `i16` values.
  Stock codec `0x0076E120` serializes object offsets `+0x10`, `+0x12`, and
  `+0x14`; its producer also fills and validates three UI selections.
  Four-byte bodies are truncated and eight-byte bodies have trailing data.
  Rust rejects both before authorization, catalog work, or persistence.
- Every proposed slot must be zero or a known positive catalog ID. Validation
  is all-or-nothing; one unknown value returns the exact failure body
  `[0,0]` and changes no profile, revision, or actor cache. Duplicate selected
  IDs remain legal because the wire describes three presentation slots, not a
  set.
- Only the exact present owner may update. Before any worker submission the
  actor rechecks identity, profile subject, membership/role, quiesce state,
  conflicting profile writes, and reserves the requester's success queue.
  Queue saturation therefore starts no worker and mutates no disk state.
- The write uses a move-only prepared/registered capability, the canonical
  profile lane, `ProfileStore::transaction`, and an actor-owned pre-reserved
  completion slot. It mutates only `Rider.Emblem1..3`, preserves flattened
  future fields, avoids a new revision for an identical immutable snapshot,
  and confirms an exact revision after a durability-uncertain commit.
  Cancelling the request cannot discard an accepted outcome.
- Success `[1,0]` is published only after durability and exact active
  owner-generation revalidation. The ordinary protocol-room `RoomPlayer`
  cache is silently refreshed for later slot/start projections; no invented
  MyRoom peer fanout is sent. A released, superseded, or role-changed
  generation keeps the durable result but receives no stale success packet.
  `Emblem3` uses `serde(default)` so legacy two-field profiles load as zero
  without losing unknown fields.
- Direct inspection of the installed client data found
  `etc_/emblem/emblem@kr.xml` once in `DataPack1_00000.rho5`: 586 unique
  positive IDs, minimum 1 and maximum 8803. This proprietary XML and its
  archive are evidence only and are not copied into Git.
- The safe `p5136-rho5` crate scans the configured stock-client `Data`
  directory without writing to it. It uses checked offsets/ranges, bounded
  archive/table/path/file counts and declared-size totals, KR key goldens,
  header and entry checksums, exact first-`0x400` double decryption, exact zlib
  consumption and decompressed length, and plaintext MD5 authentication.
  Duplicate or missing normalized target paths fail closed. The workspace
  forbids `unsafe`, and the reader contains no `unsafe` syntax.
- The production directory-entry cap is 4096 because the installed fixture has
  1559 direct entries, mostly legacy non-RHO5 files. Independent archive,
  per-archive/total-file, table, path, archive-byte, per-entry-byte, and
  declared-total-byte caps continue to bound retained and extracted data.
- `--client-data-dir DATA_DIR` loads the exact KR entry on a blocking worker
  before any listener is bound, parses its bounded UTF-16/UTF-8
  `<kartEmblem>` document in memory, and makes that immutable catalog
  authoritative. The local proprietary ignored integration test exercises the
  complete RHO5 -> XML -> `EmblemCatalog` path and confirms all 586 IDs in
  source order, minimum 1 and maximum 8803.
- The existing C# `KartCatalog.xml` exporter does not include emblems.
  Therefore the optional `<Emblems>` format-3 extension is usable for tests or
  an externally augmented portable runtime catalog and is the fallback when
  `--client-data-dir` is omitted. RHO5 definitions take precedence when both
  sources exist. Without either source, emblem behavior remains
  fail-closed/empty. Native extraction is complete, but do not claim stock
  emblem E2E until a Wine/native client run is verified.

### MyRoom direct protected entry

- The stock `ChRqEnterMyRoomPacket` codec is exactly two UTF-16 strings:
  requested owner nickname, then room password. Runtime traces also contain
  owner plus an empty second string. The C# handler reads the first string and
  then consumes the empty password length as an unused integer, so it silently
  ignores every nonempty room password. C# remains read-only; Rust fixes the
  behavior.
- Rust requires both bounded fields and exact packet exhaustion. The password
  is held in a dedicated type whose `Debug` form is always redacted; it is
  moved into the actor command and never copied into logs, a profile, or a
  reusable session flag.
- A public present owner admits the visitor normally. A protected present
  owner with an empty request receives the exact
  `ChCmdPwEnterMyRoomPacket(owner)` prompt. A nonempty mismatch returns
  `ChRpEnterMyRoomPacket` status `4`, which the stock client maps to
  `cannotEnterMissPassword`; an exact nonempty match enters the room.
- A protected room whose stored password is empty fails closed: empty input
  prompts and no nonempty input can match. Inactive, untracked, or
  owner-absent targets remain unavailable. Successful visitor replies still
  clear both stored password strings.
- Owner resolution, comparison, transition construction, serialization,
  exact-generation queue reservation, and commit occur in one actor turn.
  Empty prompts and mismatch replies mutate no topology; successful protected
  entry uses the same all-recipient atomic commit as public entry.

### MyRoom item-password check and one-shot follow-up

- `ChRqMyroomCheckPassEtcPacket` is now parsed as the stock codec actually
  emits it: signed `i32 kind` followed by a bounded UTF-16 password. Truncated,
  oversized, or trailing input fails before actor work. Its response is the
  same kind followed by a typed `i32` status: `0` unsupported/no-op, `1`
  success, `2` prompt for a password, and `3` wrong password.
- Client static analysis maps kinds `0..=3` to Garage, Emblem, Career, and
  Item Dictionary. On an uncached protected visitor open, success sends one
  matching follow-up request: kinds `0` and `3` send
  `RmRequestItemsPacket`, kind `1` sends `RmRequestEmblemsPacket`, and kind `2`
  sends `RmRequestCareerListPacket`. A cached client path may instead open its
  local UI without another network request.
- Applying the current owner's `UseItemPwd`/`ItemPwd` to all four shared
  visitor checks is an explicit Rust product-policy inference from the client
  flow and the sole item-password fields. There is no original-server runtime
  capture for this exchange, so this inference is documented rather than
  presented as captured server behavior.
- The actor requires an exact current membership and a present exact owner.
  Owners and visitors to an unprotected item surface receive success. For a
  protected visitor, empty input returns `2`, an exact match returns `1`, and
  a nonempty mismatch returns `3`. Invalid kinds, nonmembers, and
  owner-unavailable rooms return `0`.
- A successful protected check stores no password. It mints one move-only,
  actor-owned grant scoped to the exact requester binding, exact owner
  binding, protected resource, and the owner's room-info revision. Garage and
  Item Dictionary grants are consumed by the next `RequestItems` prepare;
  the Emblem grant is consumed only by `RequestEmblems`; the Career grant is
  consumed only by `RequestCareerList`. None can authorize another resource.
- The uncached stock-client path sends at most one matching follow-up after a
  successful check, so grants are deliberately one-shot. An unused grant may
  remain until it is consumed or revoked; successful entry/reentry, room
  movement, Secede, identity migration/release, a new password check, or an
  owner-info/password-policy revision revokes or invalidates it. Consumption
  happens before profile I/O; a cancelled, stale, failed, or backpressured
  request does not turn it into reusable authority.
- The C# path parses only the kind, returns a kind-0-only placeholder, and
  serves owner items without checking the item password. Rust deliberately
  does not reproduce that bypass.

### Terminal MyRoom Career list

- The bundled C# server exposes the four Career packet-name hashes but has no
  usable handler. It remains unchanged and cannot define the missing behavior.
  Stock-client static analysis supplies the exact layouts used here; this is
  not presented as captured original-server behavior.
- `RmRequestCareerListPacket` (`0x801309EE`) is an exact hash-only request.
  `RmOwnerCareerListPacket` (`0x6B740910`) starts with signed `i32 marker_b`,
  `i32 marker_a`, and `i32 count`. Each nonempty entry is exactly 17 bytes:
  `i32 field0`, `i32 field1`, `u8 field2`, `i32 field3`, `i16 field4`, and
  `i16 field5`. The client treats `marker_a == marker_b` as terminal, making
  `0,0,0` the conservative complete empty-list response. Rust now strictly
  accepts only the four-byte request and serializes exactly the 16-byte
  response hash plus those three zero `i32` values.
- The session parses and fully consumes the request before any actor mutation
  or grant consumption. Publication is requester-only through the actor-owned
  outbound queue; there is no direct session reply, extra ACK, peer broadcast,
  profile-store read, Career data lookup, or persistence side effect. The
  already-bound in-memory profile identity is still revalidated.
- Owners and public visitors may request the terminal list repeatedly. A
  protected visitor needs the exact kind-2 one-shot grant. Kind-0/3 owner-item
  and kind-1 emblem grants are preserved, while kind-2 is consumed when the
  actor mints a move-only Career plan, before queue reservation.
- The shared private owner-resource plan performs the common membership,
  owner, policy-revision, and generation checks, but distinct move-only
  Emblem and Career wrapper types prevent one resource capability from being
  published through the other path. Requester and owner generations are
  revalidated again at publication.
- Queue saturation is a logged packet drop rather than a session failure.
  Queue-full, policy-stale, owner-generation-stale, requester-generation-stale,
  and quiesced paths publish nothing. None of those outcomes, nor requester
  cancellation after prepare, restores a consumed kind-2 grant. Outsiders
  receive no packet. The established shared policy permits the terminal list
  for a retained public owner-Secede tombstone and denies a protected
  tombstone after revocation.
- The second request (`0xB7D40BE9`) is signed `i32 requester_no` plus a
  bounded UTF-16 nickname. Its response (`0x6A500900`) begins with
  `u8 present`; when present it continues with UTF-16 nickname, `i32 level`,
  `u8 rank`, and `i32 career_points`.
  That owner-info exchange is not implemented. Nonempty entry ownership and
  field meanings, marker/pagination behavior beyond terminal equality,
  owner-info lookup/authorization/data policy, and any Career persistence,
  reward, or progression semantics remain evidence-limited and must not be
  invented.

### MyRoom Reenter and random public entry

- `ChReRqEnterMyRoomPacket` and `ChRqEnterRandomMyRoomPacket` are now separate,
  explicit session paths. Both hash-only packets are parsed strictly; trailing
  bytes fail before profile projection, actor mutation, RNG use, or outbound
  publication.
- Both paths reuse the already generation-bound in-memory profile presentation
  and the same opaque `MyRoomEntryInput`/World command as direct entry. They add
  no profile job, profile lane, async plan, retry loop, or target-interleaving
  window.
- `Reenter` first resolves the requester's exact current Hub membership and
  republishes that authoritative room. It therefore keeps a visitor in the
  room they actually occupy, permits an already-authorized member to restore a
  protected room, and remains valid while the owner is temporarily absent
  during a retained topology window. Only when no membership exists does it
  return to an actor-tracked owned room or bootstrap the requester's own room
  from the bound profile.
- `Reenter` never treats a stale session copy as authoritative room state.
  Owners receive full room info; visitors receive the same policy/BGM/kart
  fields with both stored password strings cleared.
- A legacy profile's invalid self-room wire fields are retained as a deferred
  fallback result. Exact current membership or an authoritative tracked owned
  room does not consume that unrelated error; self bootstrap surfaces it as a
  typed request error without stopping the World actor.
- Random entry intersects current active identity generations with Hub state.
  A candidate must own an actor-tracked room, occupy its own slot zero with an
  exact reverse membership, be public, and have a visitor vacancy. The
  requester, the requester's current room, untracked identities, protected
  rooms, absent owners, and full rooms are excluded before selection.
- Eligible owners are sorted by stable `UserNo`, then a bounded choice source
  draws exactly once. Tests inject a fixed source; production uses
  `random_range`. No candidate consumes no randomness and returns
  `NoAvailableRoom(5)`. A choice source violating its bound is a typed internal
  error rather than a client status or modulo fallback.
- Selection, Hub transition construction, serialization, exact-generation
  queue reservation, and commit remain within one actor turn. Random entry
  therefore cannot select a room and then race a disconnect, migration, policy
  change, or capacity change. Queue backpressure leaves topology and revision
  unchanged; the actor operation can be retried after the queues drain, while
  the current session propagates the typed error and fails closed.
- Non-blocking optimization debt: the bounded random-candidate scan currently
  clones active identity bindings and eligible owner presentation before one
  owner is selected. This is capped by World admission and does not affect
  correctness or system design; a later cleanup may collect lightweight
  user/generation markers and clone only the selected authoritative owner.
  The availability enum also retains an obsolete production `dead_code`
  allowance; removing it is lint hygiene only.
- The C# source aliases both request names to one random selection over every
  online nickname. It can create an unowned target room, choose a full or
  protected/non-room identity while a valid public room exists, and disclose
  both plaintext passwords. Rust deliberately does none of those things.
- The current-room-else-self `Reenter` meaning is a conservative Rust product
  policy, not a capture-proven stock-client semantic. Future packet evidence
  may refine that choice, but must preserve the exact-generation capability
  and atomic publication boundaries.

### MyRoom direct entry

- `ChRqEnterMyRoomPacket` is an explicit session path. Its required owner and
  password strings use bounded UTF-16 codecs and exact exhaustion; the C#
  implementation's apparent optional dword was the empty password string's
  length, not a reserved field.
- No extra profile job or profile lane is introduced. The session projects its
  already generation-bound in-memory profile into an opaque
  `MyRoomEntryInput`; malformed presentation or self-room info fails before the
  World command is queued.
- The World actor reauthorizes the exact requester binding, resolves the target
  through the active identity registry, and validates the target against Hub
  owner topology in one actor turn. This removes the need for an async
  prepare/retry plan and leaves no target-generation or room-policy race between
  authorization and commit.
- Self entry creates the requester's room from its bound profile when no owned
  room exists. Re-entry into an existing owned room preserves the actor's
  authoritative owner presentation and info rather than overwriting them with
  a stale session copy.
- Visitor entry requires the exact target to own an actor-tracked room, occupy
  its slot zero, and remain the active generation. Public rooms admit
  immediately; protected rooms use the prompt/mismatch/match states described
  above. Inactive, untracked, and owner-absent targets return
  `OwnerUnavailable`; only an otherwise admissible room can reveal `Full`.
- Successful self replies contain the owner's full room info. Visitor replies
  preserve BGM, policy flags, `TalkLock`, and kart fields but clear both raw
  password strings. The reply always uses the canonical actor-resolved owner
  nickname.
- A move is one Hub transition containing old-room removal and destination
  admission. The requester response, old-room update, and destination snapshot
  are serialized and every exact-generation recipient queue permit is reserved
  before the transition commits. A full queue publishes nothing and changes no
  membership or revision. Backpressure remains a typed request error and is
  propagated to fail the session explicitly rather than leaving a stock client
  waiting forever for an entry reply; the actor operation itself is retryable
  from unchanged state after queues drain.
- The requester's destination batch is ordered as entry reply then current slot
  snapshot. Destination peers receive only that snapshot; remaining old-room
  members receive only the post-move old-room snapshot. A dropped ACK receiver
  cannot cancel accepted actor work, while quiesce rejects the command before
  publication.
- Room-entry passwords and item-surface passwords are intentionally separate.
  A `CheckPassword` success can never authorize entry, and a direct room
  password can never authorize owner-item reads.

### MyRoom CharacterPosition

- `RmCharPosPacket` is now parsed explicitly and strictly before actor work.
  The request must contain exactly one slot and six finite little-endian
  `f32` values; truncated, trailing, out-of-range, NaN, and infinity inputs are
  typed protocol errors and cannot fan out.
- The admitted World command retains an exact-generation identity-operation
  child while queued. The actor authorizes the source session and resolves the
  current room audience in one actor turn, so migration cannot reinterpret a
  stale source or recipient.
- The client slot is treated only as an assertion. It must equal the
  actor-owned membership slot, and the wire packet is serialized from that
  canonical slot. A mismatch and an authenticated nonmember are silent drops;
  the sender is excluded from the peer audience.
- Every current peer binding is revalidated against the active identity
  generation. All bounded recipient queue permits are reserved before any
  packet is published. One full recipient queue drops the complete ephemeral
  update without disconnecting the sender or leaking a partial fanout; the
  request is immediately retryable after queues drain.
- Position fanout is stateless: it does not change Hub topology or revision.
  Dropping the requester-side ACK future does not cancel an actor-accepted
  publication, and quiesce rejects new position production before publication.

### MyRoom RiderTalk

- The stock request is exactly one UTF-16 message. The echo is the
  authoritative signed `i32` room slot followed by that message. Runtime
  traces and both client codecs agree on this shape. The client receive handler
  indexes its roster directly from the slot, so Rust never accepts a
  client-supplied author or slot.
- Rust parses before actor admission side effects, requires exact packet
  exhaustion, and caps input at 256 UTF-16 code units. Negative lengths,
  truncation, invalid UTF-16, oversized messages, and trailing input are typed
  protocol errors with no fanout. Empty messages remain wire-compatible. The
  validated request field is private, move-only, and redacted from `Debug`.
- Client send- and receive-side static analysis independently establish the
  counterintuitive legacy flag direction: `TalkLock == 0` disables chat and
  every nonzero value enables it, with no owner exemption. The Hub projects
  this as a semantic boolean from the current room state; the World actor
  checks it in the same turn as membership and audience resolution. A disabled
  room or authenticated nonmember is a silent, non-disconnecting drop.
- The C# server ignores `TalkLock` and sends to recipients one at a time after
  releasing its room lock. Rust deliberately fixes both defects. It uses the
  actor-owned canonical sender slot, excludes the sender, revalidates every
  exact-generation peer, reserves all bounded queues, and only then publishes
  the complete fanout.
- Rider talk is ephemeral and never mutates Hub topology or revision. One full
  peer queue drops the whole update and makes it retryable after drain.
  Migration fences stale sources, dropping the requester ACK cannot cancel an
  accepted publication, and quiesce rejects production before publication.
  The 256-unit cap is an explicit Rust amplification bound; the C# parser has
  no comparable chat-specific limit.

### MyRoom RequestItems

- The hash-only request is classified explicitly and parsed strictly before
  actor or profile work. Trailing bytes are a typed protocol error and cannot
  publish a partial response.
- The World actor mints a minimal plan containing the exact requester
  generation, membership owner, exact owner generation, and item-visibility
  policy. Unrelated visitor churn does not invalidate or repeat a large
  profile read.
- The owner sidecars are read under the canonical owner profile lane and a
  retained child of the requester's exact identity operation. The disk result
  is itself typed to the planned owner binding; cancellation cannot let
  migration drain before the worker finishes.
- `TuneData.json`, `NewKart.json`, and `PartsData.json` are bounded by file,
  record, aggregate packet, and aggregate response-byte limits. The
  later-version-only `Parts12Data.json` is deliberately not read.
- The actor revalidates requester, membership, owner generation, and
  visibility after profile I/O, then reserves and publishes the full ordered
  response in one outbound batch. Stale authorization retries at most three
  times; queue backpressure is typed and cannot publish a prefix.
- Every ordered login response has one aggregate write deadline. A slow-drip
  peer cannot multiply the configured timeout by the RequestItems packet
  count while retaining request, identity, outbound, or shutdown guards.
- Missing owners receive only the zero-count enchant packet. An existing owner
  with no items receives the distinct explicit empty owner-item packet.
- The C# kart-empty early return loses valid parts. Rust deliberately fixes
  this data-loss defect: a parts-only inventory is serialized normally.
- A visitor may read a public owner inventory. When `use_item_password != 0`,
  Rust denies an ungranted visitor before disk I/O while still allowing the
  owner. A successful kind-0 or kind-3 item-password check authorizes exactly
  the next protected `RequestItems` plan; the grant is consumed before the
  bounded owner profile read and is revalidated against membership, owner
  generation/presence, and policy revision before publication. Integrated
  visitor info responses retain policy flags but redact raw passwords.

### Exact-generation operation ownership

- `IdentityOperationLease` is a non-`Clone`, non-`Copy`, actor-minted
  capability bound to one registry instance, identity, owner session, and
  generation.
- Each generation has an atomic admission gate. Migration freezes the exact
  source gate, rejects new work, and waits outside the World actor for every
  previously admitted child lease to retire.
- Queueing an admitted World command creates an actor-owned child lease.
  Cancelling the requester cannot release that queued work early.
- Accepted profile/equipment work retains its own child lease through disk
  mutation, World revalidation, publication, and terminal reply.
- The TCP session composes wire admission and identity admission so a frame
  cannot lose either lifetime during direct replies or actor reply flushing.
- Deferred disconnect/expiry release remains capacity-accounted until the
  exact generation drains. Graceful shutdown refuses to hide live leases;
  forced shutdown reports their count.
- A registry-instance token prevents a lease minted by one World actor from
  authorizing a different actor even when every public identity field
  collides. Ordinary commands and the specialized MyRoom/equipment durable
  paths enforce the same boundary; the actor also rejects a foreign envelope.

### Migration transaction

- Preflight freezes and validates the exact source generation before profile
  work begins.
- Candidate generation/session-index capacity, destination ACK queue space,
  peer publication permits, lifecycle storage, MyRoom state, protocol-room
  state, and identity state are prepared before the irreversible boundary.
- MyRoom, protocol-room, and identity transitions expose exclusive commit
  capabilities whose final `commit` methods are result-free.
- The destination migration ACK is inserted into its ordered outbound queue
  before owner publication. After that insertion, the contiguous actor turn
  contains no fallible transition.
- Peer backpressure aborts before ACK or owner publication and preserves the
  old owner, old gate, room topology, MyRoom topology, cancellation handle,
  and destination authentication state.
- Dropping or timing out an unsubmitted preflight reopens only its exact
  transfer freeze. Expiry wakes drain waiters.

### UDP and Messenger generation fences

- UDP socket `Ready` completion and identity activation increment one shared
  synchronized logical clock under the same short critical section. A datagram
  completed before activation is stale; the first datagram arriving after an
  already-pending receive crosses activation remains fresh.
- UDP source work enters the same identity-operation gate used by TCP. Routing
  uses actor-owned exact-generation room snapshots.
- Messenger uses bounded signed-length framing with exact header/body reads;
  fragmented and coalesced frames are covered over real TCP.
- Messenger Enter is checked against the World-published active identity.
  Hub mutations and per-connection writes are actor/single-writer serialized.
- A transport generation stamp brackets the socket poll that consumes the
  first byte, then is rechecked after the full frame and again at actor
  dispatch. A partial frame consumed under the old generation cannot execute
  as the replacement generation.

### Existing integrated gameplay and durability

- Login/authentication, startup replies, channel migration, room
  create/list/join/leave, ready/team/master/observer state, loading readiness,
  human race start, finish, ranking, settlement, team booster, deterministic
  fallback-track selection, and reward scheduling are actor-integrated. AI
  wire/state primitives exist, but the production AI roster/start flow remains
  an evidence-dependent gap.
- Race audiences and results are frozen by exact identity generation and
  process-global race epoch. Durable reward retries are idempotent and fenced
  by run/user/attempt identity.
- MyRoom actor state includes bounded entry topology and generation-aware
  migration/disconnect cleanup. Production dispatch currently integrates
  direct self/public entry, current-membership Reenter, bounded random public
  entry, direct protected entry, strict item-password checks and one-shot
  protected owner-item grants, fresh FirstState projection, owner-info
  durability, bounded RequestItems, exact-generation character-position
  and TalkLock-aware rider-talk fanout, and Secede.
- Rider equipment/plant persistence is cancellation-independent and refreshes
  the actor caches only after durable confirmation.
- Graceful shutdown drains wire admission, accepted durable work, completion
  mailboxes, actor producers, queued writes, and sessions in phases. Forced
  shutdown exposes pending actor-publication, migration, and
  identity-operation counts.
- Standalone World startup rejects a zero mailbox capacity with a typed error,
  and its join handle preserves terminal actor failures in release builds.
- Ready actor-output draining is snapshot-bounded, so a continuously producing
  peer cannot starve the session read loop.

## Corrections derived from the C# audit

This Rust checkpoint does not modify C# files. Rust intentionally differs in
these areas:

- Messenger does not treat one socket read as one packet, trust packet-supplied
  identity, mutate shared room lists without serialization, or issue
  concurrent writes on one connection.
- Migration does not publish several mutable indexes and then attempt
  best-effort rollback. It prepares capabilities first and crosses a
  result-free commit boundary.
- Protocol-room membership and public indexes commit within the World actor,
  eliminating the C# publication window.
- Team-booster state and all required recipients are reserved atomically;
  queue failure leaves the state unchanged.
- UDP source admission is generation-, owner-, IP-, room-, and logical-epoch
  fenced. Exact movement tick ordering is deliberately not guessed; see the
  capture-dependent gaps below.
- Bounds are enforced before variable-size allocation where Rust owns the
  decoder. The audit's broader C# "checksum before allocation" wording is not
  treated as a compatibility rule.
- Ordinary rider-info lookup does not copy the C# parser's nonzero-scalar
  branch from a derived couple-request schema or its ten-byte-short success
  serializer. Rust strictly accepts the stock ordinary request and returns
  only the non-disclosing failure until cross-profile public data policy is
  defined.
- Rider-school start does not copy the C# compatibility shortcut's two
  drifted acceleration constants or read mutable process-global `SpeedPatch`
  state. It uses the same validated canonical physics builder as normal Rust
  race data and propagates serialization failure.
- RequestItems never publishes Tune chunks before later sidecar validation,
  never reads unused Parts12 data, never uses the C# process-global
  `PreventItem` race, and never drops parts merely because the kart list is
  empty.
- Visitor-facing integrated MyRoom info does not disclose stored room/item
  passwords. Protected owner-item reads require a successful, move-only,
  one-shot item-password grant before disk access.
- Direct MyRoom entry does not ignore the room-password flag or serialize
  stored plaintext passwords to visitors. An exact present owner admits a
  public request or validates the packet-carried protected-room password
  inside the actor; all queue reservations precede the combined old/new
  topology commit, so the C# reply-then-best-effort fanout window is not
  reproduced.
- Reenter is not aliased to random-online-player selection. It preserves an
  exact current membership before falling back to the requester's own room.
  Random entry filters on actor-owned public/present/vacant room state before a
  stable bounded choice, so it cannot create a room for an arbitrary online
  nickname or return `Full` merely because it picked an ineligible room.
- Character-position input cannot spoof another member's slot or forward
  non-finite transforms. Rust resolves and revalidates the current-generation
  audience inside the actor and reserves every bounded peer queue before
  publishing, instead of using the C# lock-then-send snapshot with best-effort
  per-peer unbounded enqueue and possible stale or partial delivery.

## Non-negotiable invariants

- Keep workspace `unsafe_code = "forbid"`. Any real `unsafe` block is a
  stop-and-review condition.
- Do not make identity leases cloneable or reconstruct them from numeric
  session/generation fields.
- Preserve registry-instance validation at every admitted World-handle
  boundary.
- Freeze in the actor, wait for drain outside the actor, and acquire the
  profile lane only after drain.
- Actor/durable work must own a retained child lease independently of its
  request future.
- Keep migration ACK/peer queue permits and every transition reservation before
  the result-free commit boundary.
- Do not add a fallible send, allocation, serialization, or validation after
  the ordered ACK is published and before all migration state commits.
- Keep UDP source admission separate from recipient lookup. Freezing a source
  must not hide valid outbound recipients.
- Keep TCP GameSlot and the opaque UDP relay as separate codecs and policy
  domains. The TCP logical/blob caps must not constrain UDP relay bodies.
- Keep parsed TCP GameSlot commands move-only and parser-minted. Do not expose
  mutable action, audience, claimed-identity, or raw-wire fields.
- Reserve the complete exact-generation GameSlot audience before publishing
  any recipient. Queue-full drops are atomic and must not enter a retry queue.
- Do not replace pre-reserved completion permits with untracked spawned
  callbacks or best-effort `try_send`.
- Do not release a profile lane before World has revalidated and published the
  durable outcome.
- Reserve required outbound capacity before mutating state. Expected
  backpressure is a typed request error; impossible actor-state contradictions
  remain typed terminal errors.
- Keep global identity-operation admission outermost. After admission, parse
  before packet-specific mutation; any request that deliberately authorizes
  before parsing must document and test that error-priority boundary.
- Bounded mailboxes, collections, wire fields, and identity counts must remain
  bounded on all error paths.

### Nickname-scoped duplicate-kart inventory editor (2026-08-04)

The desktop server GUI now exposes an inventory editor for the concrete
multiple-enhancement use case. It loads only usable category-3 grants from the
selected stock `Profile/KartCatalog.xml`, searches the real Korean display
name with case/whitespace-insensitive substring ranking or an exact decimal
ID, and shows the name-to-ID resolution before mutation.

The persistence rule is owned by `p5136-profile`, not the GUI. Each addition
first acquires a short-lived offline lease on the same profile-root lock held
by a live server, captures and revalidates the durable profile-store identity,
then runs through the immutable-revision profile transaction. This prevents a
renamed/replaced root from mixing an old sidecar capability with a new ambient
profile CAS.
Each CAS evaluation loads the nickname's bounded plant/parts sidecars through
no-follow directory capabilities, recomputes the lowest free client-safe
serial, reserves current grants, the legacy equipped serial, and orphaned
sidecar serials, and appends one nickname-local `GrantedKart`. The base catalog
instance remains serial 1; duplicates start at serial 2 and are bounded to
4,096 records per profile. Different nicknames allocate independently.
Existing malformed or duplicate legacy grants are preserved on disk but
omitted from the operator view exactly as they are omitted from the runtime
inventory stream.

The resulting `(kart_id, serial)` is the same identity used by rider equipment
and the plant/parts sidecars, so two copies of one kart can retain different
enhancement state without cloning a global inventory. The GUI is disabled
while its server is active and explains that an already connected client must
reconnect. A committed-but-directory-sync-uncertain outcome is reported as a
distinct warning with a refresh instruction, avoiding an unsafe blind retry.
The loaded catalog's canonical path and bounded content fingerprint are
retained as part of the GUI snapshot; changing the client path invalidates the
catalog and selection, and addition re-reads and compares both before
mutation. Unit coverage fixes
Korean/ID search, serial 2/3 allocation, case-insensitive per-nickname
isolation, legacy equipped/orphan sidecar serial reservation, active-server
lease rejection, replaced-store identity rejection, catalog path/content
drift rejection, unknown kart rejection, and the profile grant bound. General
quantity/economy grants remain out of scope
because normal catalog items are already provided and purchase semantics
require separate currency, expiry, replay, and authorization rules.

## Validation snapshot

The current worktree passed on Windows:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
P5136_CLIENT_DATA_DIR=<stock Data> cargo test --workspace --all-features
P5136_KART_CATALOG=<stock KartCatalog.xml> cargo test -p p5136-profile inventory_editor::tests::stock_client_catalog_name_search_smoke -- --ignored --exact
P5136_CLIENT_DATA=<stock Data> cargo test -p p5136-server random_track::tests::stock_client_catalog_smoke -- --ignored --exact
# 1,031 regular tests and the real catalog-name data gate passed;
# 5 external-data-only tests remained ignored in the regular workspace run
git diff --check
```

The earlier deployment-tweak checkpoint build was
`target/p5136-finish-kart-abilities/release/p5136.exe` (16,822,272 bytes),
SHA-256
`EB918B7D345993545942F90A1FD8F97B56A459FF4B2573088CC92A3DF2CBB0C7`.

The remaining opt-in tests exercise local proprietary RHO5 metadata, the full
RHO5-to-`EmblemCatalog` runtime path, and all 19,496 retained inbound packet
records including the strict 1,471-record TCP GameSlot replay. They require
their explicitly configured client data or external trace directory. The
environment-selected item archive gate reads the installed legacy `item.rho`
and checks both row counts and combined weight totals; the new random-track
gate reads the installed legacy `track_common.rho` and resolves every default
pool.

Focused regressions cover:

- exact four-request item-state classification, hashes, stock-producer
  goldens, every truncated prefix, strict auth/scope/op/exhaustion checks,
  repeated-key wire order, independent batch/aggregate caps, exact Favorite
  Get replies, pure stable/idempotent batch application, atomic durable
  revision reuse, strict one-time sidecar migration (missing/BOM/malformed,
  byte/reply caps, post-marker ignore, CAS candidate reuse, and competing
  canonical-marker precedence), cancellation/migration fencing, selective
  bound session favorite/revision refresh, and encrypted TCP
  Update(no-reply) -> Get -> reconnect Get;
- exact five-request club-query classification and producer shapes, complete
  reply layouts and digests, fail-closed consumer meanings, private
  parser-minted fields, pre-allocation UTF-16 bounds, complete consumption,
  `NonZeroU32` club codes, authenticated read-only dispatch, profile/revision
  immutability, session continuity, and unbound/stale/quiesce ordering;
- exact ordinary rider-info scalar/reserved/target/mode parsing, private
  parser-minted representation, UTF-16 and complete-consumption bounds, exact
  five-byte non-disclosing failure, authenticated direct dispatch without
  remote profile access, profile/revision immutability, session continuity,
  and unbound/stale/quiesce error ordering;
- exact five-byte encoded rider-school request over the complete decoded `u8`
  domain; canonical 240-byte response fields and digest; fallible physics
  serialization; strict truncation/trailing rejection; profile immutability;
  follow-up liveness; and stale/quiesce priority;
- exact hash-only extra-data/web-event requests; all four packet hashes; exact
  six-/four-byte replies; truncation, wrong-hash, and trailing rejection;
  authenticated direct dispatch; profile/revision immutability; identity
  continuity; follow-up liveness; unbound, stale, and quiesced fences; and the
  malformed/error-priority combinations;
- exact normal/item-preset shop-buy hashes, 9/11-byte request bodies, decoded
  boundary values, all truncated prefixes, cross-kind/trailing rejection,
  exact common 29-byte failure response, authenticated alias dispatch,
  malformed-session liveness, identity continuity, stale-generation
  rejection, and quiesce propagation;
- exact `PqServerTime`/`PrServerTime` hashes and four-byte legacy clock body,
  bounded unused request suffix, authenticated dispatch, and identity
  continuity;
- requester cancellation with a queued actor-owned identity child;
- accepted profile work retaining its child through disk publication;
- migration drain, exact abort, expiry wake-up, and shutdown reporting;
- ACK and peer backpressure before owner publication;
- UDP receive/activation ordering without a stale reinterpretation or a
  first-fresh-datagram drop;
- Messenger generation changes during first-byte admission and later frame
  completion;
- cross-World capability rejection on ordinary and durable paths;
- typed zero-capacity World startup and observable actor termination;
- malformed room/race packets before mutation;
- TCP GameSlot hash classification; strict type
  1/2/4/5/6/7/8/9/10/11/12/13/16/17
  parsing;
  1013-byte logical and 960-byte blob limits; every supported-frame
  truncation; nonfatal malformed/unsupported dispatch followed by a live
  request; and no direct synthetic response;
- all 80 type-12 manifest pairs, five additional C# enum-derived operation
  pairs, every explicit/default/count-derived writer shape, uniqueness,
  truncation rejection, typed diagnostics, bounded fallback relay, and the
  retained stricter Barricade owner/transform boundaries;
- exact frozen-generation GameSlot routing for type 9/10/11/12/16/17,
  authenticated no-reply type-13 consumption, observer
  receive-only policy, claimed-ID spoof rejection, Loading and closed
  settlement rejection, open Settling relay, pickup/static-operation
  deferral, exact type-10/type-11 masks, server-derived type-12 peer masks,
  byte-exact sender inclusion/exclusion, masked observer delivery, stale
  replacement exclusion, native movement length validation, and all-recipient
  queue rollback/retry;
- exact MyRoom owner-item packets for owner, visitor, empty owner, and missing
  owner, plus strict malformed input;
- owner-item generation/topology/visibility revalidation, requester
  cancellation, dropped ACK, and atomic queue backpressure;
- aggregate multi-packet write timeout, guard drain, exact packet order, and
  IV progression;
- exact ten-byte P2P/Game-UDP report parsing, cross-kind rejection, claimed-IP
  discard, full `u16` port domain, and Game-UDP validation-only isolation;
- P2P durability cancellation survival, immutable-receipt confirmation,
  idempotent retry, and absolute port-zero clearing;
- exact active-generation ordinary-member/observer and MyRoom cache refresh,
  stale/ownerless/released publication suppression, same-channel endpoint
  revocation, duplicate Hub-revision stability, absent-owner tombstone
  preservation, no-ACK/no-fanout behavior, and IPv4/mapped/native-IPv6
  projection;
- direct-entry self bootstrap/re-entry, public visitor redaction, exact
  owner-plus-password parsing, protected empty prompt, status-4 mismatch,
  successful protected entry, untracked/owner-absent denial, room-full
  mapping, canonical nickname lookup, old/new move audiences, atomic
  backpressure rollback and typed session propagation, stale requester
  generation, dropped ACK, and quiesce rejection;
- strict Reenter/random empty-packet dispatch; current protected membership,
  owner-absent membership, owned-room fallback, and self bootstrap; stable
  eligible-owner filtering, no-candidate status, bounded-choice invariant
  failure, deferred invalid-self fallback, secret redaction, and atomic
  random-entry backpressure/retry;
- strict kind-plus-password parsing, exact status `0/1/2/3` replies, owner and
  public bypass, protected prompt/mismatch/success, invalid/nonmember/absent
  rejection, kind/resource separation, one-shot protected owner-item access,
  empty-stored-secret fail-closed behavior, policy-revision invalidation, and
  grant revocation on entry, Secede, migration, and release;
- exact empty `RequestEmblems`, bounded source-ordered catalog serialization,
  public/owner access, protected exact kind-1 grant consumption, wrong-resource
  preservation, stale-plan suppression, queue-full one-shot behavior, and
  malformed/duplicate/nonpositive XML rejection;
- exact hash-only `RequestCareerList` parsing and 16-byte terminal empty
  response; owner/public and protected exact kind-2 access; wrong-resource
  preservation; policy, owner-generation, and requester-generation stale-plan
  suppression; queue-full grant burning; public/protected owner-tombstone
  distinction; requester-only publication; and quiesce rejection;
- exact protected-item request/reply hashes, strict truncation/wrong-hash/
  trailing rejection, exact eight-byte terminal empty response, malformed
  parsing before profile lookup, and authenticated requester-only dispatch;
- exact three-slot main-emblem parsing, fail-closed catalog validation,
  present-owner preflight before profile admission, all-or-nothing
  transaction, durability-before-ACK, cancellation survival, stale-generation
  and role-change ACK suppression, completion backpressure, registered-ticket
  abort, accepted-outcome-loss terminal detection, and graceful/forced
  shutdown accounting;
- bounded synthetic RHO5 scan/extract goldens for offsets, both key layers,
  checksums, checked ranges, path normalization, exact zlib consumption,
  output limits, plaintext MD5, unique lookup, plus an opt-in local
  proprietary extraction-to-catalog integration;
- strict character-position input, canonical sender slots, nonmember and
  self-echo suppression, stale-source migration fencing, exact peer packets,
  atomic multi-peer backpressure/retry, dropped ACK, and quiesce rejection;
- strict bounded rider-talk input including UTF-16 surrogate boundaries,
  canonical sender echo, empty-message compatibility, sender/nonmember
  exclusion, live zero/nonzero `TalkLock` policy, stale-source fencing,
  migrated-recipient routing, atomic multi-peer backpressure/retry, dropped
  ACK, and quiesce rejection;
- bounded ready-output flushing.

Production Rust contains no `unsafe` syntax; the workspace also forbids it.

## Remaining work

These items prevent a "port complete" claim.

1. **Capture-dependent movement behavior**

   Opaque UDP/P2P room relay and generation fencing exist. Per-sender movement
   sequence state, exact tick wrap semantics, movement envelope fields, and the
   fallback used when data is missing still require stock-client captures.

2. **UDP first-bind capability**

   First bind validates active generation and source IP, but does not yet use a
   TCP-issued nonce/challenge. The synchronized logical clock closes the known
   task-preemption race, but cannot prove when a datagram entered an OS socket
   queue. Design and test nonce issuance, one-time consumption, expiry,
   Game/P2P separation, and NAT rebind policy.

3. **Remaining MyRoom/economy surface**

   The identity-bound dispatcher now logs and consumes an unclassified hash
   without a reply after exact-generation/profile authorization; the MyRoom
   match is exhaustive, so adding a classified request without a handler is a
   compile error. The retained-corpus deliberate no-reply ledger is complete;
   future packet families still require their own evidence and owning domain
   before they may mutate state or produce a response.

   The exact terminal empty `RmOwnerCareerListPacket` and matching kind-2
   grant path are implemented. Static client analysis establishes that safe
   terminal shape, but nonempty ownership and field meanings,
   marker/pagination semantics beyond equality, and owner-info
   lookup/authorization/data policy still need stronger evidence. The bundled
   C# code contains only the four packet-name hashes and silently drops the
   requests, so it is not a behavioral specification. The five read-only club
   queries now have strict producer-derived parsing and honest
   empty/unavailable replies, but the list-count zero case still needs the
   empty search flow and stock-client E2E. Successful club membership,
   creation, join, search, and rename require an actor-owned repository,
   global membership/name namespace, authorization, atomic persistence, and
   replay policy; they are a system-design slice rather than per-profile
   string writes. Finish kart tuning/upgrades and the remaining
   quest/attendance/progression surface.
   Canonical nonempty protected-item ownership, bounded Get/Update, and
   lease-bound `Locked.json` migration are implemented. Successful item
   deletion/unlock authentication and the broader economy rules remain
   separate gaps.
   Both shop-buy aliases now parse their exact producer-derived bodies and fail
   closed with the exact common response. Actual purchase authorization,
   pricing, inventory/currency transactions, replay/idempotency policy, and
   stock-client success behavior remain an economy design and evidence slice.
   Ordinary rider-info lookup now parses the exact stock request and returns a
   non-disclosing failure without target-profile access. A success path still
   requires a public profile DTO, field-level privacy rules, offline lookup
   authorization, and a corrected complete serializer; do not expose the
   current C# ten-byte-short projection or create profiles as a lookup side
   effect.
   Password request values are bounded and redacted but still use ordinary
   `String` storage and comparison, matching the existing plaintext profile
   fields; a later storage redesign should add zeroization/at-rest protection
   and per-requester attempt throttling without weakening actor admission
   bounds.

4. **Evidence-dependent packet behavior**

   TCP GameSlot now strictly parses every one of the 1,471 retained records
   and models all 80 statically recovered type-12 writer schemas plus the
   C# enum-derived named-pair compatibility set. A separate newest-log audit
   validates all 174 type-17 quad snapshots. Capture multiplayer type-16
   routing and finish the type-12 source/target/object-effect ledger beyond
   the 58 currently reconstructed contracts; known type-12 client events
   already relay after bounded envelope and peer-mask validation. Type-1/type-2 selection and synthesis now follow
   the C# writer and installed client probability data; capture a fresh visible
   pickup plus two-client sender-inclusive reply before calling that E2E
   complete. Catalog-backed acquisition remapping is now implemented exactly
   once from the frozen kart. Do not add the separate C# type-10/type-11 or
   item-use side effects until fixtures prove both their state transition and
   any extra wire packet without double transformation.

   Also add captures/fixtures for special observer-map master policy, AI
   roster/start and nonzero AI-master payloads, the real track-pool/control
   surface, stock-client rider-school start acceptance of the canonical
   physics reply, and any P5136-vs-modern packet difference still represented
   by a fallback.

5. **Race-wide crash atomicity**

   Individual reward writes are idempotent and durable, but a whole race is not
   yet one multi-profile journal transaction. Specify recovery if the process
   stops after only some racers commit.

6. **Compatibility and deployment gates**

   Run the existing Windows/macOS/Linux CI matrix to green and record the
   result. Add differential C#/Rust fixtures for the supported packet surface,
   native Windows client launch, Wine/CrossOver/Sikarugir launch, and a two-client login
   -> migrate -> room -> race -> persistence run.

7. **Explicit product-policy decisions**

   Resolve owner-disconnect tombstones, NAT rebinding, and special observer
   ownership from client-visible evidence. Keep safer deterministic Rust
   behavior unless a capture proves another behavior is required.

## Exact resume plan

1. Start the fresh release with the stock `KartRider_5136` client root (or its
   `Data` directory). The Rust server now rebuilds its immutable catalog
   directly from `kart.rho`, `item.rho`, and RHO5; `Profile\KartCatalog.xml`
   is not required. Launch
   the local stock client, and preserve the matching server/client logs.
   Verify that initialization reaches channel switch and then exercise the
   captured menu, scenario, time-attack, room, and race entry paths.
2. Use a second physical LAN machine for the first multiplayer run. Verify
   two distinct clients login, migrate, join one room, exchange ready/team/
   track/chat state, start, exchange UDP/P2P traffic, finish, persist, and
   shut down. Confirm the ceremony TX order is `GameControl(state=4)`,
   `GameNextStage`, then `GameResult`, and that both clients return to the
   room. One local installation is not a safe multi-client target.
3. Verify the new type-1/type-2 award in the stock client: collect the request,
   synthesized reply, selected item, effective rank band, and visible slot
   result. The first two-machine LAN run must confirm the same sender-inclusive
   73-byte reply reaches both exact generations. Separately collect a
   multiplayer type-16 mask and a two-client visible type-17 movement result;
   trace source, target, and
   `(base_hash, object_id)` ownership/effect semantics. Known bounded type-12
   pairs already relay. Never apply the TCP cap to the separate opaque UDP
   movement envelope.
4. Capture endpoint-report behavior with two stock clients and NAT-relevant
   topologies before adding a live peer-refresh/fanout packet or coupling the
   durable presentation port to observed UDP routing. Existing peers may
   retain an earlier serialized endpoint until a normal room snapshot.
5. Add TCP-issued UDP bind capabilities without weakening the existing
   generation/IP/logical-epoch fences.
6. Capture and implement movement sequence/tick behavior per sender and exact
   race generation; never copy the broken C# recipient-global predicate.
7. Close remaining economy and packet-fixture gaps, then design race-wide
   reward recovery.
8. Run the existing three-desktop CI matrix to green, record the run, and
   exercise the connector on Wine/CrossOver/Sikarugir.
9. Before every checkpoint run:

   ```text
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace --all-features
   cargo test --workspace --all-features -- --ignored
   git diff --check
   rg -n "\bunsafe\b" crates -g "*.rs"
   ```

## 2026-08-04 direct client RHO catalog

- Normal GUI/CLI startup no longer consumes the C#-generated
  `Profile/KartCatalog.xml`. Selecting the client root, `Profile`, or `Data`
  resolves one canonical `Data` directory and reads `kart.rho`, `item.rho`,
  and the KR RHO5 overlays directly.
- The bounded loader reproduces 1,456 kart names, 1,353 `BodyParam` specs,
  6,929 shop rows across 65 categories, 73 verified item symbols, all 493
  `TransformByKart` rules, and 133 merged `animalBooster` rules (626 runtime
  transforms total). It requires the 1450/1453 identity, slot-capacity,
  chicken-gold transform, and special-booster sentinels before publication.
- The opt-in stock-client test now verifies the direct-RHO cardinalities,
  sentinels, exact 1,287 automatic kart grants, and exact 9-ID quarantine set.
  The old XML remains a compatibility-only `ServerConfig` input, not a runtime
  prerequisite or generated artifact; blanket C# inventory publication is no
  longer treated as the desired semantic oracle.
- GUI inventory search retains the immutable direct-RHO `Arc<CatalogInventory>`
  and passes it into the next server start when the canonical `Data` path still
  matches, avoiding a second 112 MiB `kart.rho` parse.
- Direct catalog reconstruction publishes the 493 `TransformByKart` and 133
  `animalBooster` rules consumed by current Rust gameplay. The C#-exported 78
  `FiringToGain` and 150 `FiredToGain` audit rows intentionally remain outside
  `CatalogInventory`: the client owns those decisions, while the server only
  validates and relays the resulting GameSlot use/reaction reports.

## Definition of port complete

The port is complete only when every supported P5136 request has explicit
behavior and evidence, no classified request silently falls through, accepted
work is cancellation-safe and crash-diagnosable, normal/force shutdown is
tested, strict gates pass on Windows/macOS/Linux, and the stock client completes
a two-client login/channel/room/race/persistence flow through the Rust server
and connector.
