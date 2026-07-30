# Rust port status and resumable handoff

Last updated: 2026-07-29

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

Committed baseline before the current tranche:
`52b35c9 Port strict MyRoom password kind reply`

## Current Rust checkpoint

The current worktree corrects the committed placeholder password response from
new stock-client static evidence, finishes direct protected-room entry, and
connects an uncached protected item-password success to a single matching
follow-up without retaining credential plaintext.

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
  Emblem and Career grants remain reserved for their still-unported matching
  request paths and cannot authorize owner items.
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
- `RiderTalk` deliberately remains unported in this tranche. The C# handler
  ignores `TalkLock`, but stock-client static analysis now establishes that
  zero is chat-off and any nonzero value is chat-enabled. The next slice can
  therefore enforce the authoritative owner policy, canonical sender slot,
  sender exclusion, and all-peer queue reservation without guessing the flag
  direction.

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
  fanout, and Secede.
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
# 637 passed, 0 failed
git diff --check
```

The 637 tests comprise 9 CLI, 35 connector, 134 core, 80 profile, 371 server
unit, and 8 server integration tests. Doc-tests also passed.

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
- strict character-position input, canonical sender slots, nonmember and
  self-echo suppression, stale-source migration fencing, exact peer packets,
  atomic multi-peer backpressure/retry, dropped ACK, and quiesce rejection;
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

   Finish TalkLock-aware chat fanout, consume the reserved kind-1/kind-2
   one-shot grants in the emblem/career flows, add the bounded emblem catalog,
   main-emblem persistence, P2P-port profile writes, club create/rename
   refresh, and any request still intentionally classified as a no-op. Finish
   kart tuning/upgrades and the remaining quest/attendance/progression surface.
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

1. Enumerate every classified request and mark it implemented, intentionally
   unsupported with a typed response, or capture-blocked. No silent fallthrough.
2. Preserve the now-explicit separation between direct room-password entry and
   item-password UI checks. Keep one-shot grants move-only, plaintext-free, and
   scoped to exact requester/owner generations plus owner-info revision.
   Connect the reserved Emblem and Career resources only when their matching
   request paths are implemented; never broaden a grant to another resource or
   a reusable session-global flag. Continue the remaining MyRoom requests in
   small vertical slices with malformed-input, stale-generation,
   cancellation/backpressure, and exact-packet tests.
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
