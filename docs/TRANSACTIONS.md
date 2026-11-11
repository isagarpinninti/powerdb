# Transactions and durability (as built in 0.3.1)

This page says exactly what is guaranteed today. It does not describe the target; for that see [ROADMAP.md](ROADMAP.md). PowerDB does not currently claim to be an ACID database in the general sense, and this page is why.

## The unit of atomicity

One call that creates, updates or deletes **one record**, with any files attached to it in the same call.

1. **Stage.** File bytes are chunked, hashed, and appended to the blob log (fsynced in `commit` mode). Nothing refers to them yet.
2. **Commit.** Under one short mutex (`commit`), one fjall write batch applies the manifest, new chunk pointers, reference-count changes and the record. The batch is atomic across keyspaces.
3. **Acknowledge.** In `commit` mode the journal is fsynced before the call returns.

There is no way to group two calls into one atomic unit. There is no `BEGIN`, `COMMIT` or `ROLLBACK`.

## What is guaranteed

| Property | Holds | Basis |
|---|---|---|
| A reader never sees a record whose file is missing | Yes | The record and its manifest are in one batch; reads use snapshots. Tested. |
| A crash (process kill) after an acknowledged write loses nothing | Yes | Tested with `SIGKILL` during 12 parallel writers: every acknowledged write was present with the right bytes. |
| A crash between stage and commit leaves the database consistent | Yes | Staged chunks are unreferenced; compaction drops them. Tested. |
| A power cut in `commit` mode loses acknowledged writes | Should not | The log and journal are fsynced before acknowledgement. **Not tested**: a process-kill test cannot show what a disk does with unflushed writes. |
| A power cut in `interval` mode | Can lose up to one interval of recent commits, never older ones | By design. |
| A power cut in `never` mode | Can lose anything since the last shutdown | By design. |
| Two writers do not corrupt each other | Yes | One writer at a time in the commit step. |
| Reference counts stay equal to the number of references | Yes | Changed only inside the commit batch; `powerdb check` recomputes and compares. |
| A torn tail of the blob log is repaired on open | Yes | Truncation to the last whole entry. Tested. |

## What is not guaranteed

- **Multi-record atomicity.** Creating a customer and their invoice are two commits. A crash between them leaves one without the other.
- **Isolation levels.** Reads see a snapshot of the LSM at the moment they start. Beyond that, no isolation level is defined, and no test pins one down.
- **Write skew, lost updates across calls.** An update is read-merge-write on one record; two concurrent updates to the same record are serialised by the commit mutex, but the second does not detect that the first changed the record since the caller read it (no version check).
- **Referential integrity.** A `relation` field is checked to be a string. The target is not checked to exist and nothing stops deleting it.
- **Corruption detection of metadata.** `check_meta_files` guards against some corrupt metadata files; fjall's own checksums cover its blocks. Chunks are verified by hash when read (`verify_reads`, default on) and by `powerdb check`.

## Throughput cost of the safe mode

Measured on one MacBook Air (M2): about 333 commits per second at 16 connections in `commit` mode, flat in the number of writers because the journal admits one fsync at a time. See [DESIGN.md](DESIGN.md).

## What the target needs

Each item below is a gap to close before the words "transaction" or "ACID" can be used for the whole database. They are phased in [ROADMAP.md](ROADMAP.md).

1. A transaction object that buffers writes, reads its own writes, and commits as one atomic batch, with the blob-log staging protocol unchanged.
2. A defined isolation level (snapshot isolation is the natural fit for an LSM snapshot) and a test that demonstrates the anomalies it does and does not prevent.
3. Optimistic conflict detection on commit (a version check per touched record).
4. Referential integrity enforced inside the commit.
5. A failure-injection harness that stops the process at each named point (before and after the log write, fsync, batch, journal fsync, compaction step) and checks the invariants in [the next section](#invariants-to-test).
6. A power-loss test using a filesystem that discards unflushed writes. This needs infrastructure beyond a normal CI runner, so it may be a documented manual procedure at first.

## Invariants to test

Each is a sentence a test must be able to falsify.

- Every file a committed record names exists and all its chunks are stored.
- A chunk is never deleted while any manifest references it; a manifest never while any record references it.
- Reference counts equal the number of actual references.
- Uncommitted work is not visible after restart.
- Replaying recovery twice gives the same state.
- Every stored chunk matches its key when hashed.
- After any single injected failure, `powerdb check` passes and every acknowledged write is present.

All seven are now exercised: `model.rs` (random operations against a model, with `verify` after each step), `failpoints.rs` (a crash at 11 named points, then reopen and check), `crash.rs` (kill -9 under load) and `storage.rs`. Still not covered: power loss and a corrupt journal.
