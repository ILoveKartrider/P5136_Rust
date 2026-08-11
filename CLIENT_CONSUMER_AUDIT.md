# P5136 client server-packet consumer audit

Last updated: 2026-08-11

## Purpose and scope

This audit inventories the Korean P5136 client's server-packet consumption
surface independently of the Rust server's current serializers.  The product
scope is the LAN multiplayer path:

1. transport, authentication, rider/profile bootstrap, and channel migration;
2. loadout state that changes the next race;
3. channel, room, AI-slot, ready/team, and start state;
4. Game/P2P UDP topology and fallback relay;
5. speed/item individual and team race lifecycle, item relay, leave/reconnect,
   and settlement.

Event campaigns, attendance/quest/reward systems, Club/Guild/Couple/Messenger
social surfaces, gifts/cash/shop promotions, test-server controls, fishing,
and unrelated minigames are not completion gates.  Boss/flag/jewel/deathmatch/
big-track and other specialized gameplay consumers are **parked**, not erased:
they remain in the raw census and can be promoted if the supported mode scope
expands.

## Reproducible census

The read-only IDAPython census is kept outside this Git repository at:

```text
C:\Users\drash\Documents\kartrider\analysis\ida_5136_server_packet_consumer_census.py
```

Its machine-readable output is likewise an external analysis artifact:

```text
C:\Users\drash\Documents\kartrider\analysis\p5136_server_packet_consumer_census.json
```

The scan found:

- 2,886 RTTI-backed serialized classes after excluding the abstract
  `the::Packet` base;
- 534 classes reachable from at least one typed consumer;
- 749 calls through the client's generated typed-cast adapters and 212 direct
  dynamic-cast consumer sites;
- 88 reachable `Gop*` nested operation classes.

The adapter distinction matters.  Common packets such as
`GameResultPacket`, `GrSlotDataPacket`, and `PrChannelMoveIn` first pass
through a roughly 50-byte `Packet -> T` adapter.  Treating only direct RTTI
xrefs as consumers incorrectly omits most login, room, and ceremony traffic.

This is a reachability census, not proof of a wire layout.  A packet becomes
oracle-covered only after its native reader, exact completion boundary, and
the relevant consumer branch have independent fixtures and tests.

## Core outer-packet baseline

The current LAN target has 63 outer consumer classes.  `PrLogin` is included
as a special bootstrap consumer: its native class/codec is present, but it is
handled before the normal stage-adapter caller graph.

| Domain | Count | Consumer classes |
|---|---:|---|
| Bootstrap and rider state | 16 | `PcFirstMessage`, `PrCnAuthenLogin`, `PrLogin`, `PrChannelSwitch`, `PrChannelMoveIn`, `PrServerTime`, `PrGetRider`, `PrGetRiderInfo`, `LoRpGetRiderItemPacket`, `LoRpGetRiderExcDataPacket`, `PrGetGameOption`, `PrItemPresetSlotDataList`, `PrFavoriteItemGet`, `PrLockedItemGet`, `PrUnLockedItem`, `PrKartSpec` |
| Channel and room state | 21 | `ChGetRoomListReplyPacket`, `ChCreateRoomReplyPacket`, `ChJoinRoomReplyPacket`, `ChLeaveRoomReplyPacket`, `ChReplyChannelInfo`, `GrSessionDataPacket`, `GrSlotDataPacket`, `GrSlotDataBasicAi`, `GrReplyBasicAiPacket`, `GrReplyClosePacket`, `GrReplySetSlotStatePacket`, `GrReplyStartPacket`, `GrChangeTeamPacketReply`, `GrCommandStartPacket`, `GrCommandLeavePacket`, `GrKickBroadcastPacket`, `GrReplyKickPacket`, `GrRiderEchoPacket`, `GrSlotItemOnPacket`, `GrSlotStatePacket`, `GameAiMasterSlotNoticePacket` |
| Game/P2P topology | 7 | `PcGameRequestRelay`, `PcGameRequestTcpRelay`, `GameRelayBroadcastingPacket`, `PrGameReportMyBadUdp`, `PqUdpEcho`, `PrUdpEcho`, `RoomSlotPacket` |
| Race lifecycle | 14 | `GameControlPacket`, `GameRaceTimePacket`, `GameNextStagePacket`, `GameResultPacket`, `PrStartCollectRecord`, `GameSlotPacket`, `GameKartItemInfoPacket`, `GameKartPacket`, `GameAiKartPacket`, `GameUserLeaveNoticePacket`, `GameRoadBlockRunnerLeaveNoticePacket`, `GameTeamBoosterSetGaugePacket`, `GameDualBoostStatePacket`, `GameFinalAniPacket` |
| Loadout mutation replies | 5 | `PrEquipTuningPacket`, `PrEquipXPartsItem`, `PrUnequipXPartsItem`, `PrItemPresetUpdateSlotData`, `PrItemPresetUseSlotData` |

`GameSlotPacket` is an outer aggregate rather than one layout. Its supported
completion gate includes the 80 native-writer type-12 `Gop*` schemas and outer
types 1/2/9/10/11/12/13/16/17. Types 4/5/6/7/8 remain strictly classified and
bounded for diagnostics, but their Lucci-world-object, bonus-item-world-object,
and team-flag gameplay is explicitly outside the port scope. The existing
item schema and receiver audits are evidence for that subgraph; they are not
yet independent `p5136-client-oracle` decoders.

For strictly decoded type-12 bodies, the World actor now supplies the common
race-object completion boundary independently of the client decoder: a
race-epoch/object-ID registry binds class, original installer generation, and
the latest reporter. Admission follows only class-specific producer/consumer
semantics: for example Barricade 0 initializes, 1 places, 2 impacts, 3
resolves, and 4 terminates, while Mine 5 removes and 6 respawns. Exact impact
fingerprints and terminal transitions are suppressed without imposing those
numbers on another class. Unknown and explicit client-no-op meanings relay
without mutating the registry. All peer queues are reserved before registry
commit and byte-exact fanout. This server admission layer does not itself
promote the remaining class contracts to independent client-oracle coverage;
those still require native producer and consumer evidence.

Representative recovered consumer fan-out confirms why one fixture per packet
name is insufficient:

- `GrSessionDataPacket` enters stage consumer `0x00CF5AB0`; room replies and
  slot mutations converge mainly at `0x00C3FB80`.
- `GameResultPacket` is accepted by 12 mode-stage consumers, while
  `GameControlPacket` reaches 15 and `GameSlotPacket` reaches 38.
- `GameRaceTimePacket`, `GameKartPacket`, and `GameKartItemInfoPacket` each
  have 12 or more mode-specific consumers.
- `0x00CC16A0` was initially labeled as the game/UDP `RoomSlotPacket`
  consumer. RTTI vtable recovery instead places it at `MyRoomStage` slot 34.
  It is therefore not evidence for the race relay path. `RoomSlotPacket`
  remains in the runtime-derived topology baseline, but its actual game/P2P
  receive path is an open native-consumer anchor; relay control itself
  converges at `0x00AD59F0`.

Consequently, exact byte consumption must be paired with branch fixtures for
the supported speed/item individual/team stages.  A successful decode in one
mode does not prove every consumer branch.

The ordinary basic-AI path now has an exact codec boundary but remains short
of a complete semantic oracle. `GrRequestBasicAiPacket` carries no difficulty;
`GrSlotDataBasicAi` carries only six `i16` loadout fields plus team; and native
`GrCommandStartPacket` reader/writer pairs at `0x0072D970`/`0x00730400` own a
counted vector at object offset `0x120`. Element codecs
`0x0071E720`/`0x0071F880` consume and emit six consecutive encoded floats.
This proves server ownership and 24-byte element size, but not the original
meaning names of all six fields or every `GoBasicAiKart` behavior branch. It
is therefore documented separately in
[AI_DIFFICULTY_AUDIT.md](AI_DIFFICULTY_AUDIT.md) and is not added to the 12
independent outer-class count below.

## Current independent-oracle coverage

Twelve of the 63 core outer classes are represented in the current oracle:

- native layout/consumer evidence: `GameResultPacket` and
  `GameNextStagePacket`, plus the exact `PrStartCollectRecord` raw-byte codec
  and common-`GameStage` side effect;
- partial native consumer plus deployed trace: `GameControlPacket`;
- C#-golden plus live-trace structural readers: `PrCnAuthenLogin`, `PrLogin`,
  `PrChannelMoveIn`, `ChGetRoomListReplyPacket`, `ChCreateRoomReplyPacket`,
  `ChJoinRoomReplyPacket`, `GrSessionDataPacket`, and `GrSlotDataPacket`.

The five existing Club oracle entries are useful regression tests but are not
counted toward this core baseline.  Therefore 51 core outer consumers still
lack an independent client reader/branch oracle.  Existing production-unit,
integration, C#-golden, and captured-log tests remain valuable, but they do
not close that semantic gap because they can share assumptions with the Rust
writer.

`PqStartCollectRecord`/`PrStartCollectRecord` is now closed at the codec
boundary from the installed 5136 executable rather than inferred from its
name. The request class uses the 16-byte base-only vtable at `0x01064E78`, so
its logical packet is exactly hash `0x529107F4`. The 20-byte reply class uses
vtable `0x01064E9C`; readers `0x00593260`/`0x00593590` consume one raw byte
into object offset `0x10`, and writers `0x005938C0`/`0x00593BF0` emit the same
raw byte. Thus the exact reply is five bytes: hash `0x52A407F5` followed by a
flag. Common `GameStage` consumer `0x00AD59F0` reaches typed adapter
`0x00AE8310`, checks only that the cast packet exists, then `0x00AE69A0`
passes `flag == 0` into helper `0x00AE6A00` and stores the original byte in
its owned race state. Nonzero truthiness is client-compatible; the production
serializer emits only canonical 0/1.

The adjacent recorded-race finish path is also structurally closed from the
pinned client IDB. `PcReportUserCollectedRecord` uses a 36-byte in-memory
object and exact 24-byte wire shape: its codec readers at `0x00728430` and
`0x0072B780` and writers at `0x0072E4A0` and `0x00730F30` transfer five
consecutive dwords from offsets `0x10..=0x20`. Producer `0x00A84930` proves
the first is collection elapsed milliseconds; the remaining four summary
metrics stay intentionally unnamed and non-authoritative. The captured finish
fixture decodes to `103634, 0, 103, 95, 313`.

`PqReportGameCollectedRecord` has a 16-byte base-only object whose four codec
slots all delegate to `0x00578C50`, proving a hash-only request. The retained
C# dispatcher has name-table entries but no handler/reply for either report.
Rust therefore strictly parses both and consumes them without a reply or FSM
transition; it does not invent a `PrReportGameCollectedRecord` response.

The server now originates that reply using the client-native equipment gate,
not an inferred packet-name policy. Race-state builder site `0x00B4A07C`
queries `sub_8E0970(12)` and treats a nonzero result as the recorder condition.
The installed catalog identifies category 12 as replay-recording cameras. Its
value is the ninth `u16` in the 65-byte rider-equipment block and retains the
legacy profile name `Set_HeadPhone`. Rust re-authorizes the session generation,
reads that field through a semantic accessor on the already bound profile, and
emits canonical flag 0/1. The retained C# no-reply behavior remains historical
compatibility evidence, but is no longer the Rust dispatch policy.

Nested type-12 coverage is tracked separately from that outer-consumer count.
`p5136-client-oracle::item_operation` now independently models the 63-class
bomb/mine/time/shield/UFO/Lockdown/Thunderbolt/ordinary-effect expansion with hard-coded pairs,
exact lengths, state offsets, source/target/token bindings, ignored fields,
conditional runtime guards, and Thunderbolt's counted target vector.
Differential tests compare all 164 recovered external branches with strict
`ItemOperation` promotion by the production outer GameSlot parser. This does
not replace a complete outer `GameSlotPacket` client reader. The sixth pass
adds concrete occurrence and consumer FSMs for `BossPrison`, `BoundRoad`,
`Course`, `Falling`, and `Piratebomb`; only the scope-excluded `Lucci` remains
fully semantic-unknown among the exact-writer classes.

The independent `ItemClientFsm` now executes the original 149 consumer
fixtures instead of leaving them as decode assertions. Its reviewed outcome
census is 74 `LocalOnly`, 70 `DeferredOutbound`, zero `ImmediateOutbound`, and
5 `UnknownSideEffect`. Angel state 0 now arms its proven later defense-hit
producer, while state 2 is a non-terminal defense impact and keeps the timed
team effect present. The
extra 14 boss/controller branches and one Course
branch plus the GoldShield supplement use the same executor, bringing the
accepted decoder/FSM surface to 166. Deferred means only that a separately recovered native producer can emit
a later state after a timer, collision, or local guard; it is a scheduler
marker and never a synthesized acknowledgement. Unknown and explicit no-op
branches cannot create local object state, and malformed bodies cannot partly
mutate the FSM.

The fixed 149-fixture base corpus also has an exhaustive Rust-server gate.
Every fixture passes the production outer `GameSlot` decoder, the exact
item-to-registry mapping shared with the World actor, isolated registry plan
and commit, and byte-preserving relay extraction. Its pinned admission census
is 88 tracked, 61 relay-only/untracked, and zero unexpected duplicate
suppressions. This closes decoder-versus-registry policy drift for the audited
branches; it is not evidence that all 149 native visual/controller effects
have been triggered in a live client.

`CLIENT_PROTOCOL_FSM.md` and the independent `protocol_fsm` tests now cover
the high-level scene guards and packet order across those consumers. This is
orthogonal to byte-layout coverage: an FSM event is not counted as an
independent packet decoder.

## Completion order

The remaining oracle work should follow crash radius rather than packet-name
order:

1. room mutation/start packets: ready/team/AI/close/start, slot equipment, and
   command-start;
2. countdown and running-state packets: all supported `GameControl` states,
   race time, kart/item snapshots, AI master, leave, and reconnect;
3. supported `GameSlotPacket` outer types plus all 80 emitted nested schemas,
   with supported-mode consumer branches and exact truncation/suffix rejection;
4. P2P/UDP topology, TCP fallback relay, and bad-UDP recovery;
5. rider/loadout mutations whose fields alter frozen next-race physics.

Each completed row needs: a native decoder anchor, immutable raw fixture,
independent reader with exact end-of-packet checking, supported consumer
branch assertions, malformed-prefix/suffix tests, and a differential test
against the Rust serializer.  C# or a live trace alone must remain labeled as
lower-grade evidence.
