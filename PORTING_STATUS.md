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

Current implementation checkpoint:
`64a6d55 Port terminal MyRoom Career list`

## Current Rust checkpoint

The current checkpoint uses available stock-client static analysis to make one
Career behavior wire-safe without inventing server data: an exact hash-only
list request and a conservative terminal `0,0,0` response. It reuses the
actor-owned owner-resource authorization boundary, consumes only the kind-2
capability for protected visitors, and does not clone the incomplete C#
server's silent fallthrough. Nonempty records and owner-info remain
evidence-limited. C# remains unchanged.

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
- Do not replace pre-reserved completion permits with untracked spawned
  callbacks or best-effort `try_send`.
- Do not release a profile lane before World has revalidated and published the
  durable outcome.
- Reserve required outbound capacity before mutating state. Expected
  backpressure is a typed request error; impossible actor-state contradictions
  remain typed terminal errors.
- Parse and validate malformed request bodies before any actor mutation or
  authorization side effect when the protocol requires parsing.
- Bounded mailboxes, collections, wire fields, and identity counts must remain
  bounded on all error paths.

## Validation snapshot

The current worktree passed on Windows:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --workspace --all-features -- --ignored
# 704 regular tests and 2 opt-in proprietary-fixture tests passed
git diff --check
```

The 704 regular passing tests comprise 9 CLI, 35 connector, 141 core, 85
profile, 13 RHO5, 413 server unit, and 8 server integration tests. The two
opt-in tests exercise local proprietary RHO5 metadata and the full
RHO5-to-`EmblemCatalog` runtime path; both pass when explicitly enabled with
the installed fixture. Doc-tests also passed.

Focused regressions cover:

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
   requests, so it is not a behavioral specification. Club rename requires a
   global membership/name namespace; club creation is a separate system-design
   slice rather than a per-profile string write. Finish kart tuning/upgrades
   and the remaining quest/attendance/progression surface.
   Password request values are bounded and redacted but still use ordinary
   `String` storage and comparison, matching the existing plaintext profile
   fields; a later storage redesign should add zeroization/at-rest protection
   and per-requester attempt throttling without weakening actor admission
   bounds.

4. **Evidence-dependent packet behavior**

   Add captures/fixtures for generic type-12 bodies, special observer-map
   master policy, AI roster/start and nonzero AI-master payloads, the real
   track-pool/control surface, and any P5136-vs-modern packet difference still
   represented by a fallback.

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

1. Continue the packet-disposition ledger: every known request must be
   implemented, an evidence-backed deliberate no-reply, explicitly
   unsupported, or capture-blocked. The generic authenticated fallback now
   returns `UnsupportedIdentityPacket`; it no longer reports silent success.
2. Select the next evidence-complete request from that ledger and keep it a
   bounded codec/dispatch slice. Leave nonempty Career records and owner-info
   explicitly capture/evidence-blocked rather than inferring ownership,
   pagination, or authorization policy.
3. Capture endpoint-report behavior with two stock clients and NAT-relevant
   topologies before adding a live peer-refresh/fanout packet or coupling the
   durable presentation port to observed UDP routing. Existing peers may
   retain an earlier serialized endpoint until a normal room snapshot.
4. Add TCP-issued UDP bind capabilities without weakening the existing
   generation/IP/logical-epoch fences.
5. Capture and implement movement sequence/tick behavior per sender and exact
   race generation; never copy the broken C# recipient-global predicate.
6. Close remaining economy and packet-fixture gaps, then design race-wide
   reward recovery.
7. Run the existing three-desktop CI matrix to green, record the run, and
   exercise the connector on Wine/CrossOver.
8. Run the stock two-client end-to-end flow and record its exact environment,
   packets, persistence outcome, and shutdown result.
9. Before every checkpoint run:

   ```text
   cargo fmt --all -- --check
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace --all-features
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
