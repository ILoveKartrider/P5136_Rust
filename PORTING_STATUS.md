# Rust port pause handoff

Last updated: 2026-07-29

This file is the resumable checkpoint for the independent Rust port. It records
what is implemented, which invariants must not regress, and the exact order in
which work should resume.

## Scope and repositories

- Rust repository:
  `C:\Users\drash\Documents\kartrider\kartrider_p5136_rust`
- Read-only C# reference:
  `C:\Users\drash\Documents\kartrider\KartRider-P5136`
- `PhysicsSim` is a separate project and is out of scope.
- Do not copy old analysis artifacts or `PhysicsSim` into this repository.
- The C# reference repository was not modified.
- Current branch: `main`
- Previous checkpoint: `4008740 Integrate actor-owned MyRoom lifecycle`
- The durable MyRoom owner-info checkpoint is the commit containing this file.

## Implemented in the paused tranche

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

## Verification completed at the pause

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

cargo test -p p5136-server --all-features
# 246 unit tests and 8 integration tests

cargo test --workspace --all-features
# 509 tests after the force-shutdown regressions

cargo clippy -p p5136-server --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
git diff --check
```

The focused actor tests cover disk-to-Hub-to-echo ordering, requester result
cancellation, registration reply cancellation, profile-lane recovery,
outbound backpressure, and the pending-ticket shutdown guard.

The final independent read-only review found no P0, P1, or new P2 issue. It
confirmed the RAII/capability abstractions, typed error sources, cancellation
coverage, zero syntactic Rust `unsafe`, and graceful shutdown ordering.

The full workspace tests, full server suite, focused tests, workspace-wide
strict Clippy, formatting, and diff checks all passed after the force-shutdown
regressions were added.

## Known gaps and decisions still required

1. **Remaining MyRoom requests**

   Only `UpdateInfo` is connected to TCP dispatch. The other eleven classified
   requests are identity-fenced no-ops:

   - re-enter;
   - random enter;
   - direct enter;
   - first-state snapshot;
   - owner items;
   - character position;
   - secede;
   - rider talk;
   - password check;
   - emblem list;
   - main-emblem update.

2. **Expected rejection policy**

   Recheck which owner-info registration races should be soft protocol drops
   instead of terminating the login session. Persistence/infrastructure
   failures must retain their typed source.

3. **Intentional owner-disconnect difference**

   The Rust Hub closes an owner's room and ejects visitors when the exact owner
   is released. The C# server leaves an offline owner rendered in slot zero
   while visitors remain. This is an intentional deterministic policy for now,
   but it must be called out in compatibility documentation or changed
   deliberately.

4. **Force architecture follow-up**

   A stronger design would replace the boolean force flag with a monotonic
   `Running -> Graceful -> Force` shutdown state and let force break reward
   drain without killing World until profile callbacks drain. Do not simply
   remove the current direct World force request: a reward dead-letter can
   otherwise deadlock shutdown.

5. **Cross-platform and end-to-end gates**

   Windows, macOS, and Linux CI, Wine/CrossOver client launch, differential
   C#/Rust captures, and a two-client race remain later completion gates.

## Exact resume plan

1. Inspect the checkpoint before editing:

   ```text
   git status --short
   git diff --check
   git diff --stat
   ```

2. Run the final validation for this tranche:

   ```text
   cargo fmt --all -- --check
   cargo test -p p5136-profile
   cargo test -p p5136-server --all-features
   cargo clippy --workspace --all-targets --all-features -- -D warnings
   cargo test --workspace --all-features
   ```

3. Search for accidental unsafe code:

   ```text
   rg -n "\bunsafe\b" crates -g "*.rs"
   ```

   Text in error names or comments is not Rust unsafe syntax; any actual
   `unsafe` block is a stop-and-review condition.

4. If code changed after this checkpoint, obtain another independent read-only
   review of:

   - `crates/p5136-server/src/myroom_persistence.rs`
   - `crates/p5136-profile/src/store.rs`
   - `crates/p5136-server/src/world.rs`
   - `crates/p5136-server/src/session.rs`
   - `crates/p5136-server/src/runtime.rs`

5. Resume the remaining MyRoom requests in small commits:

   - membership query plus first-state and secede;
   - direct/random/re-enter with exact status codes;
   - owner-item profile reads and C# empty-kart quirk;
   - position and chat peer fanout;
   - password and emblem flows;
   - main-emblem durable write and session refresh.

6. For every request, preserve C# wire behavior with a malformed-input test,
   exact-generation test, backpressure/cancellation test where relevant, and
   exact packet fixture.

## Port completion goal

The port is complete only when the supported P5136 packet surface has explicit
coverage, no classified request silently falls through by accident, profile
writes are cancellation-safe and crash-diagnosable, normal/force shutdown
semantics are documented and tested, strict Clippy/workspace tests pass on all
three desktop platforms, and the stock client can complete a two-client
login/channel/room/race/persistence flow through the Rust server and connector.
