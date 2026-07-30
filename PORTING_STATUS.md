# Rust port status and resumable handoff

Last updated: 2026-07-30

This is the authoritative resume document for the independent Rust port. The
short feature ledger is in [PORTING.md](PORTING.md).

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

State: clean, reviewed packet-diagnostics checkpoint. The protocol resume order
remains item 1 under **Exact resume plan**; do not reopen the completed
favorite-item migration without a new compatibility/security finding.

Current implementation checkpoint:
`533df45 Port lease-bound favorite sidecar migration` +
`bb84027 Harden favorite sidecar import bounds` +
`9b26159 Add GUI server controls` +
`c67426c Document LAN E2E setup` +
`8be036e Add bounded packet diagnostics`

## Current Rust checkpoint

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

### Desktop server and connector GUI

- `p5136` remains one native binary by design. `p5136 server` and
  `p5136 connect` remain the scriptable CLI surfaces; launching with no
  arguments opens the desktop Server/Connector GUI.
- The Server tab maps directly to the public CLI server configuration: bind
  address, advertised IPv4 address, configured base port, profile root,
  optional `KartCatalog.xml`, optional client `Data` directory, remote profile
  creation, and the advanced first-message/session timeout and login-limit
  values. Inputs are validated before a bind and apply only to the next start;
  the GUI deliberately persists no machine-specific paths or endpoints.
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
- The accepted Korean P5136 types are exactly `1`, `2`, `9`, `10`, `11`, and
  `12`. Claimed player IDs must be in `0..=15`; type-specific masks, exact
  declared lengths, complete consumption, item-vector counts, pickup
  operation hashes, and finite pickup coordinates are validated before the
  actor command is created. Modern-only types `5`, `7`, `8`, and `17` are
  nonfatal unsupported drops.
- Type 12 uses a checked-in static allowlist of 74 exact
  operation/base-operation hash pairs rather than scanning a packet-name enum
  at runtime. Tests independently recompute every pair from its two names,
  require 74 unique entries, and parse every entry. Banana, Course, Rocket,
  and Barricade also require captured raw lengths `30`, `32`, `73`, and `73`;
  a wrong length cannot fall back to the generic branch.
- A Barricade operation additionally requires its marker, inner owner,
  reserved field, and all twelve transform floats to be valid. Only that
  validated operation includes the sender. The remaining 70 generic pairs
  currently prove only the pair, nonzero low-16 mask, bounded exact envelope,
  and overall cap; their inner bodies are not claimed to be capture-verified.
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
- Type 9, type 10, and ordinary type 12 relay the exact original bytes to all
  active exact-generation frozen recipients except the sender. Type 11 sends
  only to frozen player IDs selected by its low-16 recipient mask and still
  excludes the sender. A validated Barricade reaches the whole exact audience,
  including its sender. Missing, migrated, released, or replacement
  generations are never silently substituted.
- All recipient queue permits are reserved before the first publication. One
  full queue drops the whole time-sensitive event, releases earlier permits,
  leaves race state unchanged, and does not enqueue a heartbeat retry. An
  empty audience is a valid zero-recipient outcome. Quiesce continues to block
  the enclosing `WorldCommand::Race` before publication.
- Valid type-1/type-2 pickup frames are not relayed. In C#, the field at the
  live-rank offset is replaced with a server-selected item before a new room
  packet is synthesized; relaying the request would present rank as item ID.
  Rust records an explicit deferred/no-relay outcome until an authoritative
  item award and serializer are supported by stronger fixtures.
- Rust also omits the C# type-10/type-11 kart side effects, bonus item
  synthesis, probability rerolls, and item remapping. The stability audit
  identifies double transformation and synthetic packet behavior as failure
  risks. Exact raw relay is implemented without reproducing those defects.
- GameSlot wire errors, unsupported P5136 types, wrong phase, spoofing,
  observer source, inactive frozen membership, closed settlement, and outbound
  saturation are structured nonfatal drops. Stale global identity ownership,
  actor termination, invariant failures, quiesce closure, and an impossible
  command/outcome mismatch still propagate. Ordinary runtime events include
  bounded metadata and typed reasons; the dedicated local `p5136_packet` file
  sink additionally retains bounded raw packet diagnostics as documented
  above.
- The audit records 1,471 compatible C# traces, but that corpus is not checked
  into either repository and could not be replayed independently. Actual
  type-9/type-10 and generic type-12 capture-derived differential fixtures,
  authoritative pickup synthesis, and stock-client E2E remain evidence gaps.
  GameSlot-specific quiesce and session-after-queue-full tests are also absent;
  the shared Race quiesce gate, actor atomic backpressure test, and session
  expected-rejection test cover those mechanisms separately.

### Terminal protected-item list

- `PqLockedItemGet` (`0x2D8105C2`) is treated as a strict hash-only request.
  Both relevant C# handlers consume no request body, although a separate
  stock-client producer proof is not available; exact four-byte exhaustion is
  the safer Rust compatibility policy.
- `PrLockedItemGet` (`0x2D8F05C3`) begins with a signed `i32 count`. Both C#
  the P5136 compatibility handler and the stock-era handler return count zero,
  so Rust emits exactly eight bytes: the reply hash followed by `i32 0`.
- Truncation, the wrong hash, or trailing bytes produce a typed protocol error.
  Body validation happens before the bound in-memory profile lookup. The
  normal identity-operation and exact profile-binding fences still apply.
- The response is returned directly and only to the authenticated requester.
  It creates no World command, peer fanout, profile-store read, persistence
  write, or shared-state mutation.
- The general C# serializer shows the nonempty record shape, but Rust does not
  claim or persist nonempty protected-item ownership. `PqLockedItemUpdate`
  is parsed and persisted by modern code but ignored by the stock-era branch;
  without P5136 producer or capture proof, update and nonempty list support
  remain explicitly evidence- and design-blocked.

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
- The P2P report persists only the `u16` port through the canonical profile
  lane, `ProfileStore::transaction`, exact immutable-receipt confirmation, and
  a pre-reserved actor completion slot. Once submitted, the write and terminal
  publication survive requester cancellation. An identical value reuses the
  existing revision; port zero is an absolute durable clear.
- Runtime endpoint authority is separate from the historical profile field.
  Every login generation and same-channel replacement starts with runtime port
  zero, regardless of the value loaded from disk. Later profile/equipment,
  reward, and `PqGetRider` refreshes preserve only the exact live generation's
  runtime value and cannot resurrect a stored port.
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
  behavior. Its reported presentation port and observed UDP routing authority
  remain intentionally separate until stock-client/NAT captures justify a
  stronger link.

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
  record, aggregate packet, and aggregate response-byte limits. The unused
  `Parts12Data.json` is not read.
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

## Validation snapshot

The current worktree passed on Windows:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --workspace --all-features -- --ignored
# 815 regular tests and 2 opt-in local-data tests passed
git diff --check
```

The 815 regular passing tests comprise 9 CLI, 35 connector, 204 core, 100
profile, 13 RHO5, 446 server unit, and 8 server integration tests. The two
opt-in tests exercise local proprietary RHO5 metadata and the full
RHO5-to-`EmblemCatalog` runtime path; both pass when explicitly enabled with
the installed fixture. Doc-tests also passed.

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
- TCP GameSlot hash classification; strict type 1/2/9/10/11/12 parsing;
  1013-byte logical and 960-byte blob limits; every supported-frame
  truncation; nonfatal malformed/unsupported dispatch followed by a live
  request; and no direct synthetic response;
- all 74 fixed type-12 name/hash pairs, uniqueness, actual parse admission,
  four capture-derived exact lengths, Cube/CubeForBoss/Lucci exclusion,
  pickup and Barricade finite values, and strict Barricade body ownership;
- exact frozen-generation GameSlot routing for type 9/10/11/12, observer
  receive-only policy, claimed-ID spoof rejection, Loading and closed
  settlement rejection, open Settling relay, pickup deferral, byte-exact
  sender inclusion/exclusion, masked observer delivery, stale replacement
  exclusion, and all-recipient queue rollback/retry;
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

   The identity-bound dispatcher now rejects an unclassified hash explicitly;
   the MyRoom match is exhaustive, so adding a classified request without a
   handler is a compile error. Complete the evidence ledger for the existing
   deliberate no-reply list.

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
   The terminal empty protected-item list is implemented. Nonempty
   protected-item ownership and `PqLockedItemUpdate` remain blocked by
   unproven P5136 wire semantics and durable-state policy.
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

   TCP GameSlot now has a strict bounded envelope and safe relay policy.
   Capture real type-9/type-10 frames and each generic type-12 body before
   narrowing their internal layouts. Capture type-1/type-2 pickup requests and
   authoritative server replies before implementing item selection or
   synthesis. Do not add the C# type-10/type-11 side effects until fixtures
   prove both their state transition and any extra wire packet without double
   transformation.

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
   native Windows client launch, Wine/CrossOver launch, and a two-client login
   -> migrate -> room -> race -> persistence run.

7. **Explicit product-policy decisions**

   Resolve owner-disconnect tombstones, NAT rebinding, and special observer
   ownership from client-visible evidence. Keep safer deterministic Rust
   behavior unless a capture proves another behavior is required.

## Exact resume plan

1. Keep the completed bounded TCP GameSlot slice frozen at its current
   evidence boundary. Collect stock-client type-1/type-2 request/reply,
   type-9/type-10, and generic type-12 fixtures before implementing pickup
   synthesis, inner-body rules, or server item side effects. Differentially
   verify exact bytes and recipient behavior; never apply the TCP cap to the
   separate opaque UDP movement envelope.
2. Capture endpoint-report behavior with two stock clients and NAT-relevant
   topologies before adding a live peer-refresh/fanout packet or coupling the
   durable presentation port to observed UDP routing. Existing peers may
   retain an earlier serialized endpoint until a normal room snapshot.
3. Add TCP-issued UDP bind capabilities without weakening the existing
   generation/IP/logical-epoch fences.
4. Capture and implement movement sequence/tick behavior per sender and exact
   race generation; never copy the broken C# recipient-global predicate.
5. Close remaining economy and packet-fixture gaps, then design race-wide
   reward recovery.
6. Run the existing three-desktop CI matrix to green, record the run, and
   exercise the connector on Wine/CrossOver.
7. Run the stock two-client end-to-end flow and record its exact environment,
   packets, persistence outcome, and shutdown result.
8. Before every checkpoint run:

   ```text
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace --all-features
   cargo test --workspace --all-features -- --ignored
   git diff --check
   rg -n "\bunsafe\b" crates -g "*.rs"
   ```

## Definition of port complete

The port is complete only when every supported P5136 request has explicit
behavior and evidence, no classified request silently falls through, accepted
work is cancellation-safe and crash-diagnosable, normal/force shutdown is
tested, strict gates pass on Windows/macOS/Linux, and the stock client completes
a two-client login/channel/room/race/persistence flow through the Rust server
and connector.
