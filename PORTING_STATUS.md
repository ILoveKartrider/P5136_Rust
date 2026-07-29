# Rust port status and resumable handoff

Last updated: 2026-07-29

This file is the resumable checkpoint for the independent Rust port. It records
what is implemented, which invariants must not regress, and the order in which
work should continue.

## Scope and repositories

- Rust repository:
  `C:\Users\drash\Documents\kartrider\kartrider_p5136_rust`
- C# source and behavioral reference:
  `C:\Users\drash\Documents\kartrider\KartRider-P5136`
- C# stability findings:
  `C:\Users\drash\Documents\kartrider\KartRider-P5136\P5136_STABILITY_AUDIT.md`
- `PhysicsSim` is a separate project and is out of scope.
- Do not copy old analysis artifacts or `PhysicsSim` into this repository.
- The user has explicitly placed confirmed C# source defects in scope for
  repair. Keep C# and Rust edits, validation, history, and commits in their own
  repositories; never copy either repository's dirty tree over the other.
- The C# repository already has valuable unrelated dirty work. Inspect and
  preserve it before adding stability fixes.
- Current branch: `main`
- Current committed checkpoint:
  `4bd09cd Document resumable Rust port checkpoint`
- The FirstState/Secede live-profile tranche described below has passed its
  independent reviews and full validation.
- The working tree contains the validated live-presentation, equipment, and
  migration-safety tranche described below. It is awaiting its final independent
  reviews and checkpoint commit. Do not discard, overwrite, or stash over it.

## Compatibility policy

The C# server is a protocol and product-intent reference, not a specification
whose bugs must be cloned.

- Preserve packet identifiers, field layout, status values needed by the stock
  client, and externally meaningful success ordering.
- Prefer a documented Rust correction when the C# behavior loses accepted work,
  races disconnect or migration, partially mutates identity, trusts invalid
  persisted state, or turns a request-scoped error into server failure.
- Fence every intentional difference with a deterministic test and record why
  it is a hardening or correctness fix rather than an accidental compatibility
  regression.
- Grant and ownership validation for rider equipment is an intentional Rust
  hardening. Revisit it only if a real client capture proves that valid stock
  behavior depends on equipping ungranted items.
- Invalid MyRoom presentation data must not undo an otherwise valid durable
  equipment/reward write or identity migration. Rust retains the last valid
  cached presentation and continues the independent operation.

## Current uncommitted checkpoint

The dirty tree is based on `4bd09cd` and contains changes in:

```text
PORTING_STATUS.md
crates/p5136-core/src/equipment_protocol.rs
crates/p5136-server/src/identity.rs
crates/p5136-server/src/lib.rs
crates/p5136-server/src/myroom_hub.rs
crates/p5136-server/src/myroom_persistence.rs
crates/p5136-server/src/profile_io.rs
crates/p5136-server/src/runtime.rs
crates/p5136-server/src/session.rs
crates/p5136-server/src/world.rs
crates/p5136-server/src/equipment_persistence.rs  # new, untracked
```

Do not run a destructive Git command against this tree.

### Work implemented but not committed

- Added identity-free `MyRoomProfilePresentation` projection and silent Hub
  refresh. One refresh updates every matching owner/visitor role without an
  immediate wire packet.
- Made `GetRider`, channel migration, and durable reward completion carry fresh
  full presentation data rather than reconstructing it from a historical or
  partial cache.
- Added cancellation-independent rider-equipment persistence:

  - World reserves a bounded completion slot and mints an opaque ticket;
  - a registered capability owns the admitted profile lane;
  - RAII guards report abort-before-submit and accepted-outcome-loss paths;
  - the durable transaction validates grants, saves the exact equipment
    selection, and normalizes a nonzero kart with serial zero to serial one;
  - World revalidates the ticket, identity generation, durable value, and full
    presentation before publication;
  - an active exact session silently refreshes MyRoom, refreshes the game-room
    cache, and fans out to peers except the sender;
  - disconnect cleanup is deferred while that source owns a pending equipment
    write;
  - graceful and forced shutdown accounting includes equipment tickets and
    per-user indexes.

- Made the equipment session continuation reload the full canonical profile
  while it still owns the profile lane. The bound session can no longer retain
  unrelated stale profile fields after an equipment write.
- Matched the useful C# `SetRiderItems` framing behavior: an authenticated short
  body is silently ignored, while trailing bytes after the first 65 are ignored.
  The Rust path still identity-fences both cases.
- Made `GetRider` normalize `kart != 0 && serial == 0` to serial one, durably
  save it, and reply from the exact immutable receipt.
- Made live race equipment changes update the frozen participant's GameResult
  character and kart fields. Historical reward calculation remains sealed to
  the completed result.
- Reworked migration around an actor-minted exact transfer ID:

  - preflight freezes that exact source identity generation;
  - a linear RAII registration owns a pre-reserved completion slot;
  - dropping the request reports abort independently of request cancellation;
  - stale aborts cannot ABA-release a newer transfer using the same permit;
  - completion revalidates source, destination, permit, and transfer ID;
  - TTL is checked at actor dequeue time;
  - graceful and forced shutdown account for admitted migration transfers.

- Separated optional MyRoom presentation publication from durable reward,
  equipment, migration, and GetRider correctness. Invalid presentation retains
  the last valid Hub snapshot without killing World or rejecting the independent
  operation.
- Added focused tests for durability uncertainty, exact kart normalization,
  malformed/trailing equipment packets, dropped response waiters, explicit
  deferred close, silent MyRoom refresh, game result serialization, profile-lane
  recovery, migration cancellation/ABA/TTL, and shutdown blocking/accounting.

### Validation snapshot for the dirty tree

Passed:

```text
cargo test -p p5136-server --lib
# 282 passed, 0 failed

cargo check --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
# 547 passed, 0 failed

cargo fmt --all -- --check
git diff --check
```

The 2026-07-29 source scan found no production Rust `unsafe` syntax, and the
workspace forbids unsafe code.

Three final read-only reviews examined this diff:

- protocol/state review found no P0/P1 regression in equipment, GetRider,
  live-race equipment, invalid-presentation isolation, exact-ID migration, or
  deferred close;
- Rust safety review found no P0 and confirmed zero unsafe code, but identified
  the pre-existing graceful-shutdown operation-drain P1 recorded below;
- test review found no blocking false positive and confirmed that the central
  tests reach production handler, persistence, actor, serializer, and shutdown
  paths.

The remaining P2 coverage gaps are an outbound-queue-triggered deferred close,
preflight reply cancellation before capability receipt, TTL expiry while the
profile lane is blocked, and exact final GameResult batch ordering in the
live-equipment test. The durable reward receipt mismatch is also a P2 typed
terminal-invariant problem rather than a direct actor crash.

### Highest-priority follow-up

Migration freeze currently rejects new identity authorization immediately but
does not yet track and drain source packet operations that were authorized
before the freeze. C# attempts this with `ActiveOperations`; the next Rust
tranche should implement the intended property with exact actor-owned operation
leases rather than copy the C# mechanics:

1. every admitted authenticated operation owns a linear, generation-bound
   operation guard;
2. beginning migration prevents new operation admission;
3. preflight completion waits for all already-admitted guards to retire;
4. cancellation or error releases exactly its own guard;
5. timeout/abort cannot release another migration or leak a frozen identity;
6. accepted durable work retains its normal publication/reply semantics.

The same lease must also fix an existing graceful-shutdown P1: normal shutdown
currently quiesces World and aborts session tasks before every already-admitted
wire operation reaches its socket reply and session-context update. A profile
save can therefore commit durably while the client receives no result. Graceful
shutdown must close new packet admission, wait for operation leases to reach
zero while World/profile/sidecars stay alive, and only then retire sessions.
Forced shutdown may bypass the wait, but must report the exact abandoned
operation count.

This is a correctness fix and may intentionally differ internally from C#.

## Earlier implemented architecture

### Exact profile receipts

- `ProfileTransaction::Unchanged` now returns the `SavedProfile` that was
  confirmed from the same locked disk snapshot.
- MyRoom idempotent writes no longer release the store lock and then attach a
  potentially newer `latest` revision to an older value.
- A cross-store regression test advances the head after the original snapshot
  and proves that the original immutable receipt is preserved.
- Unknown flattened MyRoom profile fields survive an absolute owner-info
  update.

### Cancellation-independent MyRoom persistence

- A dedicated bounded completion mailbox is sized from the World identity
  capacity.
- World mints a nonzero, monotonic ticket and stores one pending write per user.
- Registration binds the exact `IdentityBinding`, proposed bounded
  `MyRoomInfo`, pre-reserved owner echo, and final result sender.
- A clone-free registered capability owns the profile admission and proposal.
- Dropping it before submission sends `AbortedBeforeSubmission` through a
  pre-reserved completion permit.
- After profile acceptance, a second RAII guard reports
  `AcceptedOutcomeLost` if the callback cannot run.
- The completion carries `ProfileIoCompletion`, so the canonical profile lane
  remains held through World revalidation, Hub commit, echo enqueue, and final
  reply enqueue.
- Completion payloads contain bounded MyRoom data, not a full `Profile`.
- `MyRoomInfoWriteReceipt` cannot be freely constructed by World code. It can
  only be derived from the opaque durable value produced by profile I/O.

### World actor integration

- Admission is rejected before disk I/O when World is quiescing, the session
  is stale/unauthenticated, the profile subject differs, the requester is not
  the present owner, another write is pending, or the owner outbound queue
  cannot be reserved.
- The owner echo queue slot is reserved before disk commit.
- A successful completion revalidates the full identity generation and the
  exact persisted value.
- Publication outcomes are typed:

  - active exact owner: Hub update plus one owner echo;
  - current ownerless generation: Hub update without echo;
  - role changed: durable disk value only;
  - superseded generation: durable disk value only;
  - released generation: durable disk value only.

- Graceful World shutdown refuses to finish while a MyRoom ticket or its user
  index remains.
- Completion is selected after migration expiry and before the ordinary World
  command mailbox, heartbeat, and UDP ingress.

### `RmNotiMyRoomInfoPacket` session behavior

- A nonmember gets no response and its body is not parsed.
- A visitor's body is not parsed, even if malformed; the visitor receives the
  current owner info.
- A present owner is parsed and validated, persisted through the profile
  runtime, committed to the Hub, and echoed exactly once through the
  actor-owned outbound queue.
- The bound session snapshot updates only `profile.my_room` and its revision.
  It does not replace unrelated fields with a stale full-profile clone.

### Graceful shutdown order

The normal path is now:

1. World quiesce
2. actor-owned session drain
3. session task abort/join
4. reward persistence drain
5. profile runtime admission close and accepted-job drain while World is alive
6. MyRoom completion FIFO barrier
7. guarded World shutdown/join
8. UDP and messenger cleanup

A deterministic supervisor test blocks the MyRoom profile worker, starts
shutdown, proves World remains responsive, releases the worker, and verifies
that the durable completion retires before World exits.

### Force-shutdown visibility

- Force shutdown captures the pending MyRoom ticket and user-index counts
  synchronously at the start of the actor command, before its first await.
- A nonzero count emits a structured warning before the World is stopped.
- The public API documents that accepted profile jobs are still drained and may
  commit to disk, while pending Hub publication, reserved owner echo, and final
  request reply are deliberately abandoned.
- `Ok(())` means forced teardown joined successfully, not that those actor
  publications completed.
- A World test proves exact `1/1` reporting and no owner echo/final reply.
- A supervisor test blocks an accepted profile job, proves World stops first,
  then proves force waits for the disk job to finish and the durable profile
  value survives.

### Live-profile FirstState and Secede

- `RmFirstRequestPacket` and `ChRqSecedeMyRoomPacket` validate the exact
  request hash and intentionally ignore every trailing byte, matching the C#
  handlers.
- A nonmember FirstState receives the exact 988-byte all-empty slot packet
  without profile I/O.
- A nonmember Secede receives the exact success reply without changing the Hub
  revision or publishing to peers.
- A member command uses a two-phase, actor-owned capability:

  1. World authorizes the requester and mints an opaque, bounded
     `MyRoomWirePlan` containing the exact requester and eight slot bindings.
  2. The session profile runtime reloads each occupied slot from disk, one
     canonical nickname lane at a time.
  3. `MyRoomWireProjection` validates occupancy, user number, nickname, source
     IP, UTF-16/wire bounds, and plan ownership.
  4. World reauthorizes the requester and compares the exact room topology
     before serializing or mutating.

- Topology churn during profile I/O is a typed request-scoped stale-plan result.
  The session retries at most three actor-minted plans; it never publishes a
  mixed roster.
- Wire presentation comes from the latest profile values for P2P port, the
  65-byte rider-item snapshot, RP, and club name. User number, nickname, and
  source IP remain World-owned identity fields.
- Owner and visitor FirstState packets use the same fresh room projection.
- Secede overlays the fresh projection onto the actor-planned post-leave
  topology. A visitor disappears, while an explicitly departed owner remains
  as the fresh slot-zero tombstone while visitors remain.
- Secede serializes and reserves every peer publication first, then the
  requester success reply. All permits are acquired before Hub commit; failure
  drops prior permits and leaves state unchanged.
- The TCP session returns no direct FirstState/Secede packet. Actor outbound is
  the only publication path, including when the request acknowledgement is
  cancelled.

## Non-negotiable invariants

- Keep `unsafe_code = "forbid"`; syntactic Rust `unsafe` is currently zero.
- Do not replace the pre-reserved completion permit with `try_send`,
  `blocking_send`, or an untracked spawned callback.
- Do not call `ProfileJobAdmission::run().await` for actor-owned writes. The
  request future is cancellable; ownership must transfer with
  `submit_with_completion`.
- Do not release the profile lane before World has revalidated and published
  the durable outcome.
- Never hold a `MyRoomTransition` across disk I/O. Plan it from fresh Hub state
  only after the durable completion arrives.
- Keep the echo reservation before disk mutation; a full queue must not
  produce a durable write that cannot publish its required owner echo.
- Compare full immutable durability receipts and full bounded persisted values,
  not revision numbers alone.
- Keep one pending MyRoom write per user and validate both the ticket map and
  user index on removal.
- Do not put full profiles into the completion mailbox.
- Do not serialize a member FirstState or Secede publication from the Hub's
  entry-time `MyRoomPlayerSlot` cache. Mint an identity-only wire plan, reload
  every occupied profile outside the actor, and revalidate the exact plan in
  the actor.
- Keep profile loads sequential across MyRoom slots. Holding several nickname
  lanes together can deadlock when two owners visit each other's rooms.
- Treat cached publication slots only as post-transition topology evidence.
  Every emitted player value must come from the sealed live projection.
- A stale wire plan is an expected request race, not a terminal World/sidecar
  failure. Projection invariant failures and unrelated Hub invariant failures
  must keep their typed sources.

## Verification completed at the previous clean checkpoint

Passed:

```text
cargo test -p p5136-profile
# 80/80 profile tests

cargo test -p p5136-server durable_myroom_owner_write -- --nocapture
cargo test -p p5136-server accepted_myroom_write_survives -- --nocapture
cargo test -p p5136-server dropped_registration_reply -- --nocapture
cargo test -p p5136-server full_owner_outbound_rejects -- --nocapture
cargo test -p p5136-server guarded_shutdown_waits -- --nocapture
cargo test -p p5136-server myroom_info_dispatch_is_silent_for_nonmember_and_skips_visitor_body -- --nocapture
cargo test -p p5136-server graceful_supervisor_keeps_world_alive_until_myroom_profile_completion_drains -- --nocapture
cargo test -p p5136-server force_shutdown_reports_abandoned_myroom_ticket_and_user_index -- --nocapture
cargo test -p p5136-server forced_supervisor_drains_disk_after_abandoning_myroom_publication -- --nocapture
cargo test -p p5136-server myroom_first_dispatch_ignores_body_and_uses_actor_outbound_for_all_roles -- --nocapture
cargo test -p p5136-server myroom_owner_secede_broadcast_reloads_live_tombstone_profile -- --nocapture
cargo test -p p5136-server stale_myroom_wire_projection_is_retryable_and_side_effect_free -- --nocapture

cargo test -p p5136-server --all-features
# 267 unit tests and 8 integration tests

cargo test --workspace --all-features
# 532 tests across the workspace

cargo clippy -p p5136-server --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The focused actor tests cover disk-to-Hub-to-echo ordering, requester result
cancellation, registration reply cancellation, profile-lane recovery,
outbound backpressure, the pending-ticket shutdown guard, live disk projection,
owner tombstones, stale topology rejection, and acknowledgement cancellation.

The durable owner-info review found no P0, P1, or new P2 issue. The initial
First/Secede review correctly rejected the cached presentation path as a P1
C#-fidelity mismatch. That path was replaced with the opaque, two-phase live
wire plan described above.

Three independent final reviews approved the replacement with no remaining
P0, P1, or P2 findings:

- fidelity review: C# live-profile reads, exact packet behavior, owner
  tombstone, and publication ordering verified;
- safety review: topology and generation fencing, typed error preservation,
  cancellation behavior, permit-before-commit ordering, and zero Rust
  `unsafe` verified;
- test review: no blocking defect; optional future stress coverage is listed
  in the resume plan below.

The full server suite, focused live-wire tests, workspace-wide tests,
workspace-wide strict Clippy, formatting, and diff checks passed for committed
checkpoint `4bd09cd`. See "Validation snapshot for the dirty tree" above for
the newer, uncommitted tranche.

## Known gaps and decisions still required

1. **Migration active-operation drain**

   Implement the exact operation-lease property described in
   "Highest-priority follow-up," including the graceful-shutdown ordering fix.
   This is the next correctness tranche after the current checkpoint commit.

2. **Reward completion terminal invariant**

   Recheck whether a durable reward receipt whose canonical subject does not
   match its ticket can escape as an ordinary command error and terminate
   World. Impossible actor-owned completion contradictions should be explicit
   terminal invariant failures with diagnostic context; expected request or
   stale-generation races should remain typed, nonterminal outcomes.

3. **Remaining MyRoom requests**

   `UpdateInfo`, `FirstState`, and `Secede` are connected to TCP dispatch. The
   other nine classified requests are identity-fenced no-ops:

   - re-enter;
   - random enter;
   - direct enter;
   - owner items;
   - character position;
   - rider talk;
   - password check;
   - emblem list;
   - main-emblem update.

4. **Fresh presentation beyond request-driven First/Secede**

   The live wire plan now makes FirstState and Secede faithful to C# disk
   reads. The dirty tranche adds silent refresh for migration, equipment,
   `GetRider`, and reward completion. Freshness still must cover:

   - direct/random/re-enter must load fresh owner and entrant presentation as
     part of the entry transition;
   - disconnect/owner-close publications need an explicit policy because they
     cannot perform blocking profile I/O in the World actor.

   Rust still lacks the C# `ChClientP2pAddrPacket` profile-port write and club
   create/rename profile writes. Port those before claiming complete MyRoom
   presentation compatibility.

5. **Expected rejection policy**

   Recheck which owner-info registration races should be soft protocol drops
   instead of terminating the login session. Persistence/infrastructure
   failures must retain their typed source.

6. **Intentional owner-disconnect difference**

   The Rust Hub closes an owner's room and ejects visitors when the exact owner
   is released. The C# server leaves an offline owner rendered in slot zero
   while visitors remain. Rust's deterministic cleanup may be the safer product
   behavior. Decide from client-observable requirements and captures, not solely
   from C# implementation shape, then keep the chosen behavior explicit in
   tests and compatibility notes.

7. **Force architecture follow-up**

   A stronger design would replace the boolean force flag with a monotonic
   `Running -> Graceful -> Force` shutdown state and let force break reward
   drain without killing World until profile callbacks drain. Do not simply
   remove the current direct World force request: a reward dead-letter can
   otherwise deadlock shutdown.

8. **Cross-platform and end-to-end gates**

   Windows, macOS, and Linux CI, Wine/CrossOver client launch, differential
   C#/Rust captures, and a two-client race remain later completion gates.

9. **C# stability audit parity**

   `P5136_STABILITY_AUDIT.md` identifies confirmed missing reconnect restore
   for per-user `PartsData.json` and `LevelData.json`, an incomplete-kart grant
   risk, insufficient `GameSlotPacket` framing checks, missing movement relay
   fallback, and possible finish/ceremony state divergence. Repair confirmed
   defects in C# with codec fixtures, and independently check the Rust port for
   the intended safety property. Do not mechanically port the faulty C# path.

## Exact resume plan

1. Finish the three read-only reviews of the current dirty diff:

   - protocol and intended-state compatibility, separating required behavior
     from C# defects;
   - Rust abstraction, cancellation, typed-error, terminal-invariant, and
     `unsafe` audit;
   - production-path and missing-interleaving test audit.

   Resolve every P0/P1. Resolve or explicitly record P2 findings, rerun the
   stabilization gate, and commit this tranche as one coherent checkpoint.

2. Implement migration operation drain in a separate small commit:

   - actor-minted, generation-bound, linear operation leases;
   - freeze-before-drain so no new source work enters;
   - cancellation-independent exact lease retirement;
   - pending preflight release only after the active set becomes empty;
   - deterministic timeout, abort, stale-generation, shutdown, and request-
     cancellation tests;
   - an accepted durable equipment/profile operation must still publish and
     receive its normal result before migration commits.

3. In the C# repository, convert the stability audit into tested fixes without
   disturbing its pre-existing dirty work:

   - restore per-user X-parts and level exception data during P5136 login;
   - evaluate Tune/Level12/Parts12 sibling restore streams from captured client
     requirements;
   - quarantine or completeness-filter invalid kart grant candidates;
   - validate `GameSlotPacket` framing before every typed read and log bounded
     diagnostic metadata;
   - add generation-fenced observed-endpoint movement relay fallback;
   - define finish/result/ceremony admission from coherent race state.

4. Harden remaining Rust completion paths:

   - make impossible durable reward receipt mismatches actor-terminal and
     diagnostic if the final review confirms the gap;
   - add an outbound-queue-triggered deferred-close regression in addition to
     the existing explicit-close tests;
   - prove equipment completion cannot publish through a superseded identity;
   - keep invalid optional presentation isolated from durable operation success.

5. Apply the same intended stability properties to Rust where applicable:

   - reconnect restore for every supported equipment enhancement generation;
   - catalog completeness filtering;
   - bounded typed item parsing and diagnostic summaries;
   - movement relay fallback using actor-owned generation-fenced UDP routes;
   - coherent race-result and ceremony participation.

6. Resume the remaining MyRoom requests in small commits:

   - direct/random/re-enter with intentional status codes and fresh entry
     presentation;
   - owner-item profile reads, deciding whether the C# empty-kart quirk is
     required client behavior or a defect;
   - position and chat peer fanout;
   - password and emblem flows;
   - main-emblem durable write and session refresh.

7. Port `ChClientP2pAddrPacket` and club-name mutation paths before declaring
   MyRoom presentation complete.

8. Resolve owner-disconnect semantics using stock-client captures and explicit
   tests. Do not copy the C# tombstone behavior unless it is externally useful.

9. Add Windows, macOS, and Linux CI, then validate the connector through
   Wine/CrossOver and run a two-client login/channel/room/race/persistence flow.

10. For every request, preserve required wire behavior with a malformed-input
   test, exact-generation test, backpressure/cancellation test where relevant,
   and exact packet fixture.

11. Run this stabilization gate before every Rust checkpoint:

   ```text
   cargo fmt --all -- --check
   cargo test -p p5136-profile
   cargo test -p p5136-server --lib
   cargo test -p p5136-server --all-features
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace --all-features
   git diff --check
   ```

12. Search for accidental unsafe code:

   ```text
   rg -n "\bunsafe\b" crates -g "*.rs"
   ```

   Text in error names or comments is not Rust unsafe syntax; any actual
   `unsafe` block is a stop-and-review condition.

## Port completion goal

The port is complete only when the supported P5136 packet surface has explicit
coverage, no classified request silently falls through by accident, profile
writes are cancellation-safe and crash-diagnosable, normal/force shutdown
semantics are documented and tested, strict Clippy/workspace tests pass on all
three desktop platforms, and the stock client can complete a two-client
login/channel/room/race/persistence flow through the Rust server and connector.
