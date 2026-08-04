# P5136 protocol-visible client FSM

This document reconstructs the part of the Korean P5136 client's state
machine that constrains a compatible LAN server and a future mock client. It
does not attempt to reproduce UI-only pages, events, store flows, or social
features.

The executable oracle is
`crates/p5136-client-oracle/src/protocol_fsm.rs`. It has no normal dependency
on the production packet writers.

## Evidence boundary

Three different statements must not be conflated:

1. **Native consumer acceptance** means the stock executable has an RTTI cast
   and a reachable stage consumer for a packet.
2. **Native state effect** means decompilation also exposes the branch, state
   field write, or virtual callback selected by that packet.
3. **Compatibility-safe order** means a complete packet order is demonstrated
   by a known-working deployed trace. This can be stricter than mere native
   packet acceptance.

The FSM rejects packets that cross scene boundaries and enforces the deployed
ceremony order. It therefore models the compatibility-safe path, not every
syntactically accepted interleaving inside the executable.

Private evidence used for this pass:

- `analysis/ida_5136_protocol_fsm_probe.log`
- `analysis/ida_5136_protocol_fsm_transitions.log`
- `analysis/ida_5136_protocol_fsm_control.log`
- `analysis/ida_5136_protocol_fsm_control_callbacks.log`
- `analysis/ida_5136_gamefinal_derived.log`
- `analysis/ida_5136_next_stage_command_probe.log`
- `analysis/ida_5136_final_scheduler_types_probe.log`
- `analysis/p5136_server_packet_consumer_census.json`
- `analysis/physics_5136/KartRiderU.idb`
- unpacked exact-P5136 `stage/gameFinalIndi` and `stage/gameFinalTeam`
  resources under the private analysis tree
- the known-working deployed packet trace documented in `PORTING_STATUS.md`

No client binary or IDB is copied into this repository.

## Recovered consumer hierarchy

The following is a packet-consumer delegation graph. It is not a claim that
these are the only C++ base classes or all UI scenes.

```text
ChannelStage::consume                  0x00BEBF70
  handles channel switch/move-in and create/join admission
  |
  +-- SessionStage::consume            0x00CF5AB0
        handles leave, session data, and slot data
        |
        +-- SessionReadyStage::consume 0x00CF3D10
              handles GrCommandStartPacket
              |
              +-- GameReadyStage       0x00C3FB80
              |     room ready/team/AI/kick/equipment mutations
              |
              +-- GameFinalStage       0x00B47FB0
                    special start variants and session-ready fallback

GameStage::consume                     0x00AD59F0
  common relay, UDP-failure, next-stage, result, slot, and AI-master traffic
  |
  +-- SpeedIndiGameStage               0x00B0B5B0
  +-- SpeedTeamGameStage               0x00B0E5E0
  +-- ItemIndiGameStage                0x00B10DE0
  +-- ItemTeamGameStage                0x00B14960
        each adds GameSlot, leave, race-time, control, and mode effects
```

`GameFinalStage` above is a sibling consumer of `GameReadyStage` that can
construct the next game from normal or special `GrCommandStart*` packets. Its
name alone was not used as proof of the podium path. The derived
`GameFinalIndiStage` and `GameFinalTeamStage` update slots have now closed that
gap: they execute local phase schedulers and finally call common final-stage
virtual slot 103 to install the next ready stage.

The four ordinary race modes all delegate control packets to
`sub_A847F0`. That function switches on exactly states 1, 3, and 4 and calls
virtual slots 97, 98, and 99 respectively. For standard solo speed:

- state 1 callback: `0x00A850A0`;
- state 3 callback: `0x00B0BCB0`;
- state 4 callback: `0x00B0BCE0`.

The item-mode callbacks are separate but implement the same lifecycle split.
State 4 changes the mode's internal phase from 2 to 3 before invoking final
UI/effect helpers.

## Hierarchical state model

Transport and scene are orthogonal in the executable and in the oracle:

```text
Transport
  Disconnected
    -> AwaitingFirstMessage
    -> EncryptedUnauthenticated
    -> Authenticated

Scene
  Offline -> Login -> RiderBootstrap -> Menu
                                   Menu -> Migration -> Menu
                                   Menu -> RoomLobby
                                           -> Loading
                                           -> Racing
                                           -> Settling
                                           -> Ceremony
                                              AwaitingNextStage
                                              -> AwaitingResult
                                              -> Podium
                                              -> local final-stage scheduler
                                           -> RoomLobby (`GameReadyStage`)
```

### Login and migration

- A TCP connection waits for the server-first `PcFirstMessage` before the
  encrypted logical packet stream.
- Successful ordinary login enters rider bootstrap. The complete inventory
  preload precedes `PrGetRider`; `PrGetRider` is the FSM milestone used to
  enter the menu.
- A normal `PrChannelSwitch` creates a migration epoch. The client reconnects,
  consumes another first message, sends `PqChannelMovein`, and becomes
  authenticated again on `PrChannelMoveIn`.
- The local club UI variant (`PrChannelSwitch` mode 1) is deliberately modeled
  as a same-socket menu hand-off, not a migration.

The exact post-rider query list remains data-driven UI initialization, not a
linear FSM. Those requests can be emitted in different orders and do not each
deserve a scene state.

### Room admission and ready state

- Successful `ChCreateRoomReplyPacket` or `ChJoinRoomReplyPacket` enters the
  room consumer. Rejection remains in the menu/channel scene.
- `GrSessionDataPacket` and `GrSlotDataPacket` form the normal standalone room
  snapshot. Ready/team/AI/kick/slot/equipment packets mutate that scene.
- `GrCommandStartPacket` is self-contained. `sub_CF3F30` applies its nested
  session packet through virtual slot 86, its nested slot packet through slot
  88, copies the track at packet offset 300, and applies the remaining start
  structures. Therefore a prior standalone snapshot is useful evidence but
  is not a valid native start guard.
- Standalone lobby snapshots are rejected by the oracle after Loading begins.
  This captures the stale cross-scene `GrSlotDataPacket` failure already fixed
  in the server.

### Loading and UDP readiness

The TCP scene transition and UDP readiness gate are separate:

1. `GrCommandStartPacket` enters Loading.
2. The client sends `GameControl(state=0)`.
3. The client normally originates UDP time-sync and accepts its reply.
4. The server sends `GameControl(state=1)` to begin the race.

UDP synchronization is tracked but is not a client-side state-1 guard. The
working server intentionally has a bounded timeout for a client that never
originates the UDP request, so `GameControl(state=1)` remains legal without a
successful sync.

The stock client may also send the hash-only `PqStartCollectRecord` after
`GrCommandStartPacket`; that is the `0x529107F4` packet observed in the failed
speed-team run. Its exact `PrStartCollectRecord` counterpart is hash plus one
raw flag byte. The common `GameStage` consumer accepts the reply during
Loading, Racing, or Settling, passes the inverse truth value to its collector gate, and
stores the original flag without changing the scene. The independent FSM
therefore records `record_collection_flag: Option<bool>` as a guarded race
side effect, resets it at each room/race boundary, and rejects the reply in
RoomLobby or Ceremony. The Rust server now answers this query from the exact
authenticated profile generation: category-12 replay-camera equipment
(`Set_HeadPhone` in the legacy profile schema) yields flag 1, while an empty
slot yields flag 0. This mirrors the stock race-state builder's
`sub_8E0970(12) != 0` recorder gate; it does not add a scene transition or
depend on a client-supplied boolean.

After a recorded race finishes, the client can emit the exact 24-byte
`PcReportUserCollectedRecord` (`hash | elapsed_ms:u32 | four recorder-summary
u32 values`). This is local recorder telemetry, not a server scene command:
the server validates and records it without replying or changing race/result
state. `PqReportGameCollectedRecord` is a separate base-only, hash-only request
and has the same no-reply/no-transition compatibility behavior. Neither packet
can advance Settling or Ceremony; the ordinary `GameControl` and settlement
packets below remain the only modeled transition inputs.

### Race settlement and podium

The reconstructed compatibility sequence is:

```text
Loading
  -- server GameControl(1) --> Racing
  -- client GameControl(2) --> local finish reported (Racing sub-state)
  -- server GameControl(3) --> Settling
  -- server GameControl(4) --> Ceremony/AwaitingNextStage
  -- GameNextStage         --> Ceremony/AwaitingResult
  -- GameResult            --> Ceremony/Podium
  -- local scheduler       --> RoomLobby
```

`sub_A847F0` gives exact native control-state dispatch for states 1, 3, and 4.
The standard mode helpers consume race time and treat `GameResultPacket` as a
known packet, while `GameStage::consume` caches `GameNextStagePacket` and
`GameResultPacket`. The currently proven semantic order comes from the
known-working deployment:

```text
GameControl(state=4) -> GameNextStage -> GameResult
```

The earlier Rust order `GameNextStage -> GameResult -> GameControl(state=4)`
is not blessed merely because the native dispatcher recognizes all three
packet types.

### Podium scheduler and room installation

The ordinary podium-to-room transition is executable-side and automatic. It
is not a server packet and is not defined by the final-stage RHO resources.
The exact P5136 `stage.xml` files only select `GameFinalIndiStage` or
`GameFinalTeamStage` and their UI resources; they contain no duration or room
callback.

The static call path is:

```text
GameFinalIndiStage::update (0x00B42190)
  -> individual scheduler sub_B42500
GameFinalTeamStage::update (0x00B503A0)
  -> team scheduler sub_B507D0
both final phases
  -> virtual slot 103 / sub_B49BB0
  -> sub_BED050
  -> sub_BED1D0
  -> stage-manager replacement with GameReadyStage or a mode variant
```

On entry, RTTI-checked helpers recover and retain the stage command rather
than a raw packet:

- individual: `GameFinalIndiParam`, stored at stage offset `0x850`;
- team: `GameFinalParam`, stored at stage offset `0x84C`.

Both schedulers use offset `0x844` as their zero-sentinel phase timestamp.
Their ordinary paths are:

```text
Individual
  >1000 ms -> wait offset-2132 animation completion
  -> >100 ms -> >5000 ms -> slot 103

Team
  >1000 ms -> wait offset-2132 animation completion
  -> >100 ms -> >7000 ms -> slot 103
```

Every time comparison is strict `>`, not `>=`, and the animation gate makes
wall-clock duration longer than the fixed-delay sum. Team phase changes reset
the timestamp to zero, so the following update arms the next phase.

Global mode flag `0x40` is statically tied to the observer path: the individual
scheduler takes its extended observer-result phases, and `sub_BED1D0` selects
`ObserverReadyStage` instead of `GameReadyStage`. Global flag `0x80` selects a
special manual result path. In the team scheduler it stops after the 7-second
phase until local action 13 advances phase 4 to dispatch phase 5. Its broader
product-facing mode name has not been invented because RTTI/strings do not
establish one.

`crates/p5136-client-oracle/src/final_stage_scheduler.rs` encodes these native
guards independently of production server code. The high-level FSM's
`ClientPodiumSchedulerCompleted` event is the boundary at which virtual slot
103 has successfully installed the ready stage; it is no longer shorthand
for an arbitrary user dismissal.

Room leave is modeled as a cross-phase escape from lobby, loading, race,
settling, or ceremony back to Menu. An ordinary connection loss clears the
scene; only an established channel migration preserves its pending epoch
across reconnect.

### Nested item-operation consumer FSMs

The independent item-operation oracle now also models the last five in-scope
type-12 classes from their concrete primary-vtable consumers:

```text
BossPrison: state 1 launch/bind boss+target -> 2 apply prison
            -> 3 resolve -> 4 remove

BoundRoad:  state 1 place from BombRobot/MechanicBall lane pattern
            -> state 2 impact or remove(decision=0)
            -> state 3 resolve or remove(decision=0)

Falling:    state 1 launch from PetitMeteor/SpaceBombing lane pattern
            -> state 2 impact or remove(decision=0)
            -> state 3 resolve or remove(decision=0)

Piratebomb: state 1 attach/activate -> 2 detonate/apply
            -> 3 cancel/remove
            -> 4 SpecialShield-resolved

Course:     decode subject + UTF-16 `goal`/`Ev_*` + token -> release/no action
```

These are local packet-consumption transitions, not acknowledgement protocols:
the recovered consumer branches mutate or release client runtime objects and
do not prove an immediate TCP reply. Producer-side routines originate later
states when their native timers, collision tests, boss/controller logic, or
SpecialShield check fires. The mock oracle can therefore predict local phase
and actor bindings byte-for-byte, but it must not synthesize a network reply
unless a separate producer path proves one.

`p5136_client_oracle::item_client_fsm::ItemClientFsm` makes that boundary
executable. The original 149-branch corpus is a fixed regression gate; the 14
subsequently recovered boss/controller branches, the variable-length Course
consumer, and two exact `GopGoldShield` branches run through the same FSM, for
166 currently accepted branches. Every
accepted transition returns exactly one of:

- `LocalOnly`: local mutation/release with no proven later operation;
- `DeferredOutbound`: local state now has a recovered producer continuation,
  but its timer/collision/guard has not fired;
- `ImmediateOutbound`: a same-call send, currently zero branches;
- `UnknownSideEffect`: exact bytes but incomplete runtime effect.

The reviewed 149-branch census is 71 local, 69 deferred, zero immediate, and
9 unknown. Angel and `GopGoldShield` activation contribute deferred markers because the shared
defense resolver has a recovered state-2 producer; that state-2 branch records
a repeatable defense impact locally and does not remove the timed effect.
`GopGoldShield` resolves kind 0 to item 36, kind 3 to item 81, and the
state-2 trailing `u16=106` override to item 106. A
deferred result only queues a scheduler marker; it never invents
the next state or packet bytes, and a newer known lifecycle transition for the
same object cancels its stale marker. Known lifecycle observations are retained by
class/object key, `Remove` clears the object, and `Unknown`/explicit no-action
branches do not fabricate state. Decode failures are transactional across the
object map, deferred queue, and transition counter.

The same base fixtures are independently gated on the server side. For each
of the 149 branches, the production `GameSlot` decoder and the registry
mapping used by `World::relay_game_slot` admit the operation in a fresh race
registry and preserve the complete relay packet. The fixed disposition census
is 87 tracked, 62 relay-only/untracked, and zero suppressed. This validates
the protocol and server-policy composition; it does not substitute for live
client rendering or for firing every deferred native timer/collision guard.

## What is exact and what remains open

Recovered with strong static evidence:

- consumer delegation for channel, session, ready, ordinary race modes, and
  special final/start stage;
- `GrCommandStartPacket` as the room-to-loading trigger and its embedded
  snapshot application;
- ordinary-mode `GameControl` dispatch for states 1, 3, and 4;
- the exact hash-only `PqStartCollectRecord` request, five-byte
  `PrStartCollectRecord` reply, raw nonzero truthiness, and guarded
  Loading/Racing/Settling side effect;
- mode-independent consumption of `GameNextStagePacket` and
  `GameResultPacket`;
- the individual/team podium phase machines, exact strict timer thresholds,
  animation gates, retained `GameFinal*Param` types, virtual-slot-103 handoff,
  and default/observer ready-stage selection;
- scene-inappropriate standalone room snapshots as a compatibility hazard.

Recovered from runtime/deployed evidence:

- server-first encrypted login progression and normal channel reconnect;
- UDP time-sync readiness behavior, including the timeout path;
- the strict three-packet ceremony order and return-to-room acceptance target.

Still open:

- human-readable product names for every global mode bit, especially the
  flag-`0x80` manual result variant;
- exact local state names and meanings for every internal field, including
  the frequently toggled byte at stage offset 224;
- mode-specific mini-FSMs for non-core game types;
- a socket-driving mock client that waits for the deferred native guards and
  turns only proven producer transitions into outbound packet bytes. The
  current oracle deliberately returns markers rather than fabricating those
  packets.

## Executable regression surface

`cargo test -p p5136-client-oracle --test protocol_fsm` covers:

- cold login and rider-bootstrap gating;
- normal channel migration and same-socket club UI hand-off;
- accepted/rejected room admission;
- self-contained `GrCommandStartPacket` behavior;
- UDP-synchronized and timeout-based race start;
- `PrStartCollectRecord` side-effect acceptance without a scene transition;
- finish, settlement, ceremony, podium, and room return;
- rejection of out-of-order result packets and cross-scene lobby snapshots;
- leave-room escape, disconnect reset, and transactional error handling.

`cargo test -p p5136-client-oracle --test final_stage_scheduler` additionally
covers strict threshold boundaries, both individual observer animation holds,
ordinary team timing, ready-stage selection, and the flag-`0x80` action-13
gate.

`cargo test -p p5136-client-oracle --test item_operation_semantics` executes
the complete 149-branch base census, the 15 supplemental controller/Course
branches, lifecycle storage/removal/no-op behavior, deferred-marker emission,
malformed-input rollback, and the 149-branch production-server
decode/registry/byte-exact-relay gate.
