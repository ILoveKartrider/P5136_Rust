# Retained P5136 packet-trace coverage

Last updated: 2026-07-30

This ledger records the read-only audit of
`C:\Users\drash\Documents\kartrider\KartRider_5136\logs`. The capture files
remain external evidence and are not copied into this repository.

## Corpus boundary

- 49 `packet-trace_*.log` files were inspected; 35 contain packet records and
  14 contain headers only.
- Six `server-ui_*.log` files were also checked for corroborating server
  behavior.
- The packet traces contain 19,496 incoming records and 100 distinct incoming
  hashes: 97 on TCP, three on game UDP, and two on P2P UDP. The UDP sets
  overlap, so those counts do not add to 100.
- 72 incoming hashes were already classified by a Rust transport or protocol
  domain. The 28 TCP hashes below were the previously unclassified set.
- A `covered` row means Rust validates every retained producer shape for that
  hash and performs the stated response/state transition. All 28 rows are now
  covered; hashes outside the owned domains still fail closed as
  `UnsupportedIdentityPacket`.

## Previously unclassified TCP requests

| Request | Observed lengths | Status | Rust/C# disposition |
| --- | ---: | --- | --- |
| `ChGetCurrentCmpRequestPacket` | 4 | covered | Exact 17-byte empty competition reply |
| `GameAiReportPacket` | 36 | covered | Exact hash/length plus bounded 32-byte producer report; authenticated diagnostic no-reply |
| `GameReportPacket` | 361 | covered | Complete typed C# field decode plus exact 19-byte producer extension; diagnostic no-reply instead of trusting client anti-cheat counters |
| `GrChangeTrackPacket` | 44 | covered | Actor-owned track/room-data mutation and atomic room slot-data fanout |
| `GrRequestBasicAiPacket` | 9 | covered | Actor-owned bounded AI add/remove and exact AI/slot reply fanout |
| `GrRequestClosePacket` | 21 | covered | Room-master-authorized actor slot closure/opening and exact reply fanout |
| `GrRiderTalkPacket` | 14, 16, ..., 46 | covered | Bounded UTF-16 parse and actor-owned `GrRiderEchoPacket` room fanout |
| `LoRqStartSinglePacket` | 41 | covered | Exact start tick plus retained 33-byte producer proof; authenticated no-reply |
| `LoRqUseItemPacket` | 10 | covered | Exact typed item event; explicit non-mutating no-reply until authoritative effects exist |
| `PcGameClientFramePacket` | 16 | covered | Exact three-counter telemetry decode and authenticated no-reply |
| `PcGameRequestRelay` | 12 | covered | Exact value/route decode; no peer fanout because the C# relay branch is disabled |
| `PcRideEventReportPacket` | 23 lengths, 57..383 | covered | Bounded count, UTF-16 strings, transforms, IDs, flags, values, and ticks; exact exhaustion |
| `PcRidePathReportPacket` | 23 lengths, 35..1547 | covered | Bounded count and exact 27-byte sample-vector consumption |
| `PqChallengerInfoPacket` | 4 | covered | Exact 93-byte challenger-stage reply |
| `PqCompleteScenarioSingle` | 26 | covered | Exact opaque-body length validation and reply from bound durable `scenario_type` |
| `PqEquipXPartsItem` | 22 | covered | Lease-bound/no-follow sidecar load, synced temporary file plus atomic X-part publication, then exact 26-byte success/echo |
| `PqEventBuyCount` | 24 | covered | Bounded four-ID query and exact 40-byte zero-count reply; no C# process-global state |
| `PqFinishTimeAttack` | 33 | covered | One-shot active-run fence, checked reward arithmetic, atomic time/RP/Lucci persistence, then exact terminal reply |
| `PqGetTrainingMission` | 12 | covered | Exact 20-byte empty mission projection |
| `PqKartSpec` | 15 | covered | Catalog-backed requested kart/speed physics with explicit bounded fallbacks for unavailable pet/plant/patch/tune contributions |
| `PqLockedItemUpdate` | 9 | covered | Atomic ordered-set Add/Remove, durable canonical profile state, and lease-bound/no-follow one-time `Locked.json` import |
| `PqNewCareerItemStatePacket` | 26 | covered | Exact typed career-state telemetry; authenticated diagnostic no-reply |
| `PqNewCareerListPacket` | 4 | covered | Exact 24-byte empty career projection |
| `PqReportUdpReconnect` | 4 | covered | Exact-generation UDP rebind authorization clears stale Game/P2P routes behind an arrival-epoch fence |
| `PqSendMacroChat` | 13 | covered | Resolves the bound profile quick-message table and actor-fans out with team filtering |
| `PqStartScenario` | 8 | covered | Exact reply plus canonical-lane durable `scenario_type` update |
| `PqStartTimeAttack` | 39 | covered | Checked entry fee plus atomic mode/track state, requested physics build, active-run fence, then exact reply |
| unknown hash `0x5815082A` | 64, 68, 76, 80 | covered | Isolated bounded opaque driving report for only these four captured lengths; not a generic unknown-packet escape hatch |

## Initialization result

The failing Rust run ended on the exact five-byte request
`BD 05 4C 2D 01` (`SpRqKoinBalance`). The old parser accepted only the
four-byte hash and rejected the terminal `01` as trailing data. All 56 Koin
requests in 28 retained packet traces use that same five-byte shape.

The common captured flow immediately after Koin is:

1. `PqFavoriteItemGet`
2. `PqLockedItemGet`
3. `PqFavoriteTrackMapGet`
4. `PqGetFavoriteChannel`
5. `PqAddTimeEventTimerPacket`
6. `PqCheckMyClubStatePacket`
7. `LoRqSetRiderItemOnPacket`
8. `PqChannelSwitch`

Those requests were already owned by Rust domains before this audit. The 28
rows above first appear after additional menu, scenario, time-attack, room, or
race paths, but they are now covered as well.

## TCP GameSlot replay

- The retained RX corpus contains 1,471 TCP `GameSlotPacket` records: type
  1=`43`, type 2=`22`, type 9=`1,337`, type 10=`38`, type 11=`1`, and type
  12=`30`.
- The opt-in audit now passes every one of those records through
  `parse_game_slot_packet`; dispatcher hash ownership alone is no longer
  counted as GameSlot coverage.
- Type 1/2 frames are now parser-minted pickup capabilities rather than raw
  relay packets. The World actor accepts them only from the exact frozen human
  racer in item game types 2/4. All 65 retained requests also prove the strict
  pre-award state and repeated object/tick/state/owner relations. Actor-owned
  per-player monotonic tokens reject replay/stale tuples, while a six-token,
  four-per-second bucket preserves the observed maximum three-event burst and
  bounds fabricated increasing ticks. The explicit rank policy defaults to
  trusting client-reported rank for the current LAN/friends model and can be
  switched to Combined fallback. The actor replaces bytes 38..40 with the
  selected item and atomically broadcasts the exact 73-byte result to all
  active frozen recipients including the sender. C# source plus deterministic
  Rust fixtures prove the response layout; a fresh two-client response capture
  remains an E2E gate.
- The 30 type-12 records comprise Course state 0/1 with count 4, Banana state
  2, Rocket state 2, and Barricade state 1 shapes. Those exact shapes carry
  retained trace evidence. The other entries and Course counts in the
  67-class static writer manifest remain typed `EvidencePending` forms and
  cannot relay or mutate state.
- No retained RX record proves type 4, 6, or 16 routing, arbitrary
  static-writer state reachability, or object ownership. Those are explicit
  capture gaps rather than inferred compatibility claims.

## Safety decision

An early audit patch treated every captured length as a successful no-reply
compatibility request. Independent review rejected that policy because it
would discard required replies, broadcasts, and durable mutations while
making the session appear healthy. The replacement assigns each request to an
owning abstraction: terminal query, actor-owned lobby mutation, durable
profile transition, single-player state machine, client event, or bounded
telemetry. No blanket captured-hash fallback exists.

The C# implementation is not cloned when its behavior is unsafe or
non-authoritative. Rust uses checked Lucci arithmetic, denies a finish replay,
persists before emitting success, does not accept client anti-cheat telemetry
as authority, and does not enable the disabled C# relay branch.

## Verification

- The opt-in test
  `session::tests::external_retained_packet_corpus_matches_the_dispatch_domains`
  reads the external directory through `P5136_PACKET_TRACE_DIR`.
- It revalidated all 19,496 incoming records and 100 distinct hashes. All 97
  TCP hashes resolve through composed Rust dispatch domains; every actual
  packet belonging to the former 28-hash gap is fully parsed; every retained
  Game/P2P UDP packet is decoded by the routed UDP codec; and all 1,471 TCP
  GameSlot records cross the strict GameSlot parser.
- This is protocol/corpus coverage, not a claim that a fresh stock client has
  completed every UI and multiplayer path. A new stock-client startup run and
  the two-machine LAN multiplayer E2E remain separate gates.
