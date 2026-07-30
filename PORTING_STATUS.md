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
  `dacb7bc Drain graceful wire operations in phases`
- The live-profile, equipment, exact-transfer migration, and two-phase graceful
  wire-drain tranches have passed independent reviews and full validation.

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

## Current checkpoint

`dacb7bc` adds cancellation-safe global wire admission and a stable outbound
producer barrier:

- every fully decoded login frame owns one non-clone request guard through
  handler completion, direct response, context update, and ready actor replies;
- every actor-owned `OutboundBatch` owns a distinct non-clone guard from queue
  reservation through successful write, write failure, or cancellation;
- graceful shutdown closes and drains request admission while World/profile
  services remain live, quiesces producers, drains accepted durable work,
  establishes a stable World producer seal, closes and drains outbound
  admission, then retires sessions and actors;
- force shutdown closes both phases, wakes any graceful waiter, reports exact
  abandoned request/outbound counts, and preserves the existing durable-disk
  cleanup contract;
- quiesce blocks timers, UDP ingress, new registration, and wire-producing
  commands while allowing the exact completion/read/barrier commands required
  for convergence;
- World session close and ownerless migration expiry reconcile silently during
  quiesce, so cleanup cannot manufacture post-seal wire work.

The preceding `5cfcd02` checkpoint contains the cancellation-independent
equipment writes, exact transfer IDs, live MyRoom presentation refresh, and
migration safety work described later in this document.

### Validation snapshot

Passed:

```text
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
# 562 passed, 0 failed
git diff --check
```

The 2026-07-29 source scan found no production Rust `unsafe` syntax, and the
workspace forbids unsafe code.

Independent World/test and Rust-safety reviews found no P0/P1/P2 blocker.
They verified exact RAII retirement on reserve/send/write failures, typed actor
error recovery at the final World join, the producer command allowlist, and
zero new production panic/unwrap/unsafe. Optional P3 coverage remains for a
direct instrumented `AsyncWrite` cancellation test and a real TCP durable
request that transitions from graceful to forced shutdown.

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

Do not implement this as a copyable generation stamp. A pre-freeze command can
remain queued after its requester is cancelled, so the World command or durable
ticket must own the lease and return it with its reply. Compose the global wire
guard and per-generation identity lease into one non-clone typestate capability.
Acquire in the order global request guard → identity lease → profile lane.

Migration preflight must freeze in the World actor, return a drain waiter, and
perform the await in the session future; the actor must never await its own
operation drain. Only after drain may migration acquire the profile lane.
Success retires the old gate and installs a fresh gate for the new generation;
exact abort/TTL reopens only the matching frozen gate.

TCP is not the whole boundary. Production UDP ingress must acquire the same
generation gate before readiness/dispatch. Keep recipient/background lookup
separate so freezing source admission does not hide valid outbound recipients.

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

1. close login request admission;
2. wait for every admitted request guard while World/profile services are live;
3. quiesce World timers, UDP ingress, registration, and new producers;
4. stop/drain reward persistence and profile I/O;
5. cross the MyRoom completion FIFO and stable World producer barriers;
6. close outbound admission and wait for every queued writer guard;
7. reconcile/drain World sessions, then abort/join their writer tasks;
8. guarded World shutdown/join, followed by UDP and messenger cleanup.

Deterministic supervisor tests hold request guards and MyRoom profile work,
prove World remains responsive, verify the durable publication reaches the
active owner, and prove a later force request wakes an in-progress graceful
wire drain.

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

## Historical live-profile verification

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

These counts describe the earlier `4bd09cd` live-profile checkpoint. The
current `dacb7bc` validation and larger counts are recorded in "Current
checkpoint" above.

## Known gaps and decisions still required

1. **Migration active-operation drain**

   Implement the exact operation-lease property described in
   "Highest-priority follow-up." Global graceful request/outbound draining is
   complete; this next tranche closes the per-generation migration gap for
   queued commands, durable tickets, cancellation, and UDP ingress.

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
   reads. The `5cfcd02` checkpoint adds silent refresh for migration, equipment,
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

   The C# tree now repairs confirmed Tune/Plant/Level/Parts reconnect restore,
   kart 814 quarantine, TCP/GameSlot bounds, frozen result admission, settlement
   cleanup, active-race team mutation, and Modern-vs-P5136 parser/RP separation.
   Movement fallback and endpoint rebinding remain capture/policy questions,
   not confirmed defects to copy mechanically.

## Exact resume plan

1. Implement migration operation drain in a separate small commit:

   - actor-minted, generation-bound, linear operation leases;
   - freeze-before-drain so no new source work enters;
   - non-clone frame capability moved through queued World commands and durable
     MyRoom/equipment tickets, then returned with the operation reply;
   - cancellation-independent exact lease retirement;
   - pending preflight release only after the active set becomes empty;
   - source freeze and drain before acquiring the profile lane;
   - UDP source admission through the same generation gate;
   - deterministic timeout, abort, stale-generation, shutdown, and request-
     cancellation tests;
   - an accepted durable equipment/profile operation must still publish and
     receive its normal result before migration commits.

2. Commit the reviewed C# stability tranche in its own repository. Keep the
   audit's remaining evidence gates explicit: generic type-12 body shapes,
   race-wide reward atomicity, nonzero AI master behavior, and advertised vs
   observed P2P endpoints.

3. Harden remaining Rust completion paths:

   - make impossible durable reward receipt mismatches actor-terminal and
     diagnostic if the final review confirms the gap;
   - add an outbound-queue-triggered deferred-close regression in addition to
     the existing explicit-close tests;
   - prove equipment completion cannot publish through a superseded identity;
   - keep invalid optional presentation isolated from durable operation success.

4. Apply the same intended stability properties to Rust where applicable:

   - reconnect restore for every supported equipment enhancement generation;
   - catalog completeness filtering;
   - bounded typed item parsing and diagnostic summaries;
   - movement relay fallback using actor-owned generation-fenced UDP routes;
   - coherent race-result and ceremony participation.

5. Resume the remaining MyRoom requests in small commits:

   - direct/random/re-enter with intentional status codes and fresh entry
     presentation;
   - owner-item profile reads, deciding whether the C# empty-kart quirk is
     required client behavior or a defect;
   - position and chat peer fanout;
   - password and emblem flows;
   - main-emblem durable write and session refresh.

6. Port `ChClientP2pAddrPacket` and club-name mutation paths before declaring
   MyRoom presentation complete.

7. Resolve owner-disconnect semantics using stock-client captures and explicit
   tests. Do not copy the C# tombstone behavior unless it is externally useful.

8. Add Windows, macOS, and Linux CI, then validate the connector through
   Wine/CrossOver and run a two-client login/channel/room/race/persistence flow.

9. For every request, preserve required wire behavior with a malformed-input
   test, exact-generation test, backpressure/cancellation test where relevant,
   and exact packet fixture.

10. Run this stabilization gate before every Rust checkpoint:

   ```text
   cargo fmt --all -- --check
   cargo test -p p5136-profile
   cargo test -p p5136-server --lib
   cargo test -p p5136-server --all-features
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace --all-features
   git diff --check
   ```

11. Search for accidental unsafe code:

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
