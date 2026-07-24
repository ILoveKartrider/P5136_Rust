# Porting ledger

This ledger keeps “port complete” tied to the behavior of the P5136 C# source.
A checked item needs Rust tests or an end-to-end capture; compilation alone is
not completion evidence.

## Compatibility foundation

- [x] P5136 port topology and boundary validation
- [x] zero-seeded packet-name Adler-32
- [x] little-endian primitives and .NET-compatible UTF-16 strings
- [x] login TCP encryption, checksum, IV progression, and bounded frames
- [x] exact production `PcFirstMessage` payload and plaintext frame
- [x] transactional four-socket bind and controlled shutdown
- [x] fragmented and coalesced TCP frame coverage
- [ ] encoded primitive substitution table
- [ ] game UDP framing and relay crypto
- [ ] messenger framing

## Connector

- [x] exact P5136 `KartRider.xml` bytes
- [x] exact launcher-profile XML bytes
- [x] Windows-compatible nickname validation on every host
- [x] messenger-port reachability probe
- [x] native, Wine, and CrossOver launch specifications
- [ ] PIN/BML/encoded-block read/write
- [ ] build detection by executable SHA-256 and PIN header
- [ ] endpoint replacement and NGS toggle
- [ ] immutable pristine backup and absent-marker transaction
- [ ] process-wide patch lock and atomic same-directory replacement
- [ ] Windows `runas` launch
- [ ] live Wine/CrossOver launch
- [ ] no-argument desktop GUI

## Login and identity

- [ ] `PqCnAuthenLogin` / `PrCnAuthenLogin`
- [ ] BML-backed `PqLogin` parser
- [ ] duplicate nickname rejection and stable user number
- [ ] `PrLogin` and startup response sequence
- [ ] session generation and stale-owner rejection
- [ ] channel-switch permit creation
- [ ] `PqChannelMovein` ownership transfer
- [ ] source-disconnect deferral and permit expiration

## World and transport state

- [x] actor-owned state mutation baseline
- [x] atomic eight-slot concurrent room admission
- [ ] channel catalog and channel membership
- [ ] complete room create/list/join/leave protocol
- [ ] ready/team/master/observer/AI state
- [ ] generation-bound game UDP endpoint registration
- [ ] generation-bound P2P endpoint registration
- [ ] UDP/P2P room relay
- [ ] messenger identity validation, chat rooms, and single-writer queues

## Profile and gameplay

- [ ] profile load/cache/versioned atomic writer
- [ ] rider/account initialization packets
- [ ] inventory and equipment
- [ ] kart grant/tuning/upgrades
- [ ] quests, rewards, attendance, and progression
- [ ] MyRoom state
- [ ] race start/grid/ready sequence
- [ ] race movement relay and finish/ranking
- [ ] track/mode/random-track controls
- [ ] disconnect cleanup and persistence failure recovery

## Completion gates

- [ ] all supported packet serializers have C#-derived golden fixtures
- [ ] C# and Rust decode one another’s synthetic PIN files
- [ ] differential request/response harness passes for the supported flow
- [ ] Windows, macOS, and Linux CI pass
- [ ] native Windows connector launches a stock P5136 client
- [ ] Wine or CrossOver connector launches the same client
- [ ] two clients can login, migrate channel, join a room, race, and persist
- [ ] operational documentation covers server and connector deployment
