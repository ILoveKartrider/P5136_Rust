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
  hashes: 97 on TCP, three on game UDP, and two on P2P UDP (the UDP sets
  overlap, so these counts do not add to 100).
- 72 incoming hashes were already classified by an existing Rust transport or
  protocol domain. The 28 TCP hashes below were the previously unclassified
  set.
- A `covered` row means Rust validates the complete observed request shape and
  performs the stated response/state transition. A `pending` row remains
  fail-closed as `UnsupportedIdentityPacket`; it is not silently swallowed.

## Previously unclassified TCP requests

| Request | Observed lengths | Status | Rust/C# disposition |
| --- | ---: | --- | --- |
| `ChGetCurrentCmpRequestPacket` | 4 | covered | Exact 17-byte empty competition reply |
| `GameAiReportPacket` | 36 | pending | Race telemetry; no named C# handler or established semantics |
| `GameReportPacket` | 361 | pending | C# mutates per-session anti-cheat counters |
| `GrChangeTrackPacket` | 44 | pending | Must mutate actor-owned room track/data and broadcast `GrSlotDataPacket` |
| `GrRequestBasicAiPacket` | 9 | pending | Must mutate actor-owned AI slots and broadcast AI/slot replies |
| `GrRequestClosePacket` | 21 | pending | Must mutate closed room slots and broadcast `GrReplyClosePacket` |
| `GrRiderTalkPacket` | 14–46 | pending | Must broadcast `GrRiderEchoPacket`; commands may also change room state |
| `LoRqStartSinglePacket` | 41 | pending | Initializes C# single-player/anti-cheat counters |
| `LoRqUseItemPacket` | 10 | pending | C# currently parses only, but authoritative item semantics are not established |
| `PcGameClientFramePacket` | 16 | pending | Race telemetry with no named C# handler |
| `PcGameRequestRelay` | 12 | pending | C# parses a room value; its relay branch is disabled |
| `PcRideEventReportPacket` | 57–383 | pending | Race telemetry with no named C# handler |
| `PcRidePathReportPacket` | 35–1547 | pending | Race telemetry with no named C# handler |
| `PqChallengerInfoPacket` | 4 | covered | Exact 93-byte challenger-stage reply |
| `PqCompleteScenarioSingle` | 26 | covered | Exact opaque-body length validation and reply from bound durable `scenario_type` |
| `PqEquipXPartsItem` | 22 | pending | Must persist X-parts and return the 26-byte result/echo |
| `PqEventBuyCount` | 24 | covered | Bounded four-ID query and exact 40-byte zero-count reply; no C# process-global state |
| `PqFinishTimeAttack` | 33 | pending | Must atomically persist time/rewards/ranking before the terminal reply |
| `PqGetTrainingMission` | 12 | covered | Exact 20-byte empty mission projection |
| `PqKartSpec` | 15 | pending | Requires requested speed/kart/pet physics, not a generic baseline |
| `PqLockedItemUpdate` | 9, 16 | pending | Must durably add/delete locked-item state |
| `PqNewCareerItemStatePacket` | 26 | pending | No named current C# handler; semantics remain unidentified |
| `PqNewCareerListPacket` | 4 | covered | Exact 24-byte empty career projection |
| `PqReportUdpReconnect` | 4 | pending | No named current C# handler; reconnect semantics remain unidentified |
| `PqSendMacroChat` | 13 | pending | Must resolve profile quick-message text and broadcast with team filtering |
| `PqStartScenario` | 8 | covered | Exact reply plus canonical-lane durable `scenario_type` update |
| `PqStartTimeAttack` | 39 | pending | Must persist mode/track/economy state and build the requested physics reply |
| unknown hash `0x5815082A` | 64, 68, 76, 80 | pending | Repeated race-only packet; no symbolic C# handler or established semantics |

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

Those requests were already owned by Rust domains before this audit. None of
the 28 rows above belongs to the mandatory Koin-to-channel-switch baseline;
they first appear after the user opens additional menu, scenario,
time-attack, room, or race paths.

## Safety decision

An early audit patch treated every captured length as a successful no-reply
compatibility request. Independent review rejected that policy because it
would discard required replies, broadcasts, and durable mutations while
making the session appear healthy. The released implementation admits only
the five pure terminal queries and the two fully implemented scenario
transitions. The remaining 21 requests stay explicit and fail-closed until
their owning Rust domains are implemented.
