# Roadmap: from records-and-files to a general-purpose database

This is the plan for the product brief (a general-purpose database with native objects and relationships), grounded in [the audit](ARCHITECTURE.md). It is an ordered plan, not a promise or a schedule. Phases are sized in rough effort so the scale is clear; the sizes are estimates, not measurements.

## Principles

- **Vertical slices.** Each phase ends with working, tested, documented behaviour and a release. No phase leaves `main` broken.
- **The commit path and the chunk store are the core.** They are reused, not replaced.
- **Never claim a property without a test that could fail.** "ACID", "serializable", "high availability" are used only when a test demonstrates them. Until then the docs say what is and is not guaranteed ([TRANSACTIONS.md](TRANSACTIONS.md)).
- **Compatibility first.** Every phase runs the frozen `/api/v1` contract suite and the migration test ([COMPATIBILITY.md](COMPATIBILITY.md)).

## Decisions needed before Phase 1

1. **Keep fjall's journal; do not write a second WAL.** Recommended. The brief asks for a production-grade WAL; the project already has a durable, crash-tested journal through fjall, and duplicating it adds risk. The effort goes to failure injection instead. *Needs your confirmation.*
2. **Parser: adopt the `sqlparser` crate (Apache-2.0) and write the planner and executor ourselves.** A hand-written SQL parser is months of work for no product value. Licence review is part of Phase 5. *Needs your confirmation.*
3. **Transactions: evaluate fjall's transactional keyspaces against a transaction layer of our own on top of batches and snapshots.** A one-week spike with a written result, before Phase 3 commits to either.
4. **Typed row format.** Choose the encoding (a self-describing tagged binary format is the leading candidate) before Phase 2 writes any code. It becomes part of the on-disk layout, so it is hard to change later.

## Phases

### Phase 0: safety net (about 1 to 2 weeks) — done

No new features. Make later work safe. Status: all five items below exist and run in CI. The model test found a data-loss bug in file garbage collection on its first run (fixed; see the changelog), and each new test was shown to fail when the thing it guards is broken.

- Freeze the `/api/v1` contract: a test suite that records each route's request, response and status for the documented behaviour, run in CI.
- A migration test that builds a database with the **previous release's binary** and opens it with the current one.
- Failure-injection harness: named points in the commit and compaction paths where a test can stop the process (before and after log write, log fsync, batch, journal fsync, each compaction step), then run recovery and `powerdb check`.
- Model-based test for reference counting: random create, update, delete, GC against a simple in-memory model, asserting the invariants in [TRANSACTIONS.md](TRANSACTIONS.md).
- Record the invariants as executable checks inside `powerdb check`. (`powerdb check` already recomputes reference counts and verifies files, manifests and chunks; the model and failure-injection tests apply it after every step and every crash.)

Exit: all invariant tests pass; contract suite in CI.

### Phase 1: the object engine (about 3 to 5 weeks) — mostly built, released in 0.4.0

**Status.** Built and tested: the catalog, versioning, delete markers, restore, layout 2 and its migration, the HTTP routes, the CLI, the SDK, the dashboard Files view, backup and verifier coverage, crash tests with object writes, and a measured 1 GiB streamed upload and download (about 445 and 1,140 MiB/s through the CLI on one Apple-silicon laptop, server memory peaking at 55 MiB). **Not built:** retention and expiry fields, a multipart upload route (the file is the raw request body; the dashboard and SDK use that), object metadata filtering, and permissions finer than "administrator" (Phase 4). The list below is the original plan.

The lowest-risk, highest-differentiation step: files that exist without a record, using the chunk store as it is.

- **Layout 2, step 1.** New keyspaces: `buckets`, `objects` (bucket, key, version → manifest id, filename, MIME type, size, checksum, created/modified, custom metadata), plus a reference count for catalog entries in `file_refs`.
- Object ids are stable and independent of keys and paths (content id stays the manifest hash; the catalog id is new).
- Versioning is optional per bucket: list, read a version, restore, delete a version.
- Soft delete and retention fields; deferred physical deletion through the existing GC with reference counting extended to catalog entries.
- Routes: `/api/v1/buckets`, `/api/v1/buckets/{b}/objects/{key}` (PUT, GET with ranges, HEAD, DELETE, list with prefix), streaming multipart and raw-body upload.
- CLI: `powerdb bucket create`, `powerdb object put|get|rm|ls`.
- Dashboard: a Files view; SDK: `db.bucket(...)`.
- Migration from layout 1: none needed for existing data (new keyspaces start empty); the format number still goes to 2 so older binaries refuse the directory.
- Backup, verify, GC and snapshot all learn the new keyspaces. A test fails if a keyspace is added without being included in each.

Exit: a bucket and objects work with no table; a test uploads a 1 GiB object with bounded memory; backup and restore round-trip objects, versions and metadata; crash tests cover interrupted uploads.

### Phase 2: typed tables and integrity — slices 2a to 2e built (see [TYPES_AND_TABLES.md](TYPES_AND_TABLES.md)) (about 6 to 10 weeks)

- **Layout 2, step 2.** The type system: `BOOLEAN`, sized and unsigned integers, `FLOAT32/64`, `DECIMAL`, `TEXT`, `UUID`, `DATE`, `TIME`, `TIMESTAMP(TZ)`, `JSON`, `BINARY`, `ENUM`, arrays, nullable; stored in a typed row encoding. Designed so a type can be added later without invalidating old rows (the future `VECTOR`, `EMBEDDING` and full-text types fit the same slot).
- Schema catalog with versions and `ALTER` that migrates or refuses; a collection becomes a table whose columns include a JSON column, so existing collections map onto the new model.
- Primary keys: UUID, ULID, auto-increment integer, existing time-ordered id.
- Constraints enforced **inside the commit**: `NOT NULL`, `UNIQUE`, `CHECK`, `DEFAULT`, `FOREIGN KEY` (restrict, cascade, set null).
- Secondary indexes (B-tree, unique, composite), maintained in the same atomic batch as the row, rebuilt by `powerdb check --repair-indexes`.
- `FILE` and `FILE[]` columns resolve to object references (Phase 1 catalog), counted for garbage collection.
- Migration from layout 1 records to typed rows, with the pre-migration copy; tested on a database built by 0.3.x.

Exit: constraint violations are rejected atomically in tests, including under concurrency; a test deletes a referenced row and sees the policy applied; indexes match a full scan on a randomised workload.

### Phase 3: transactions (about 4 to 6 weeks)

- `BEGIN`/`COMMIT`/`ROLLBACK` for records, objects, relationships and index updates together, built on the Phase 3 spike's choice.
- Snapshot isolation, with optimistic conflict detection by row version; the docs state the anomalies it permits (write skew) and a test shows them.
- The blob-log staging protocol is unchanged: staged chunks are unreferenced until commit.
- Rollback leaves no broken references and no orphaned catalog entries; interrupted uploads inside a transaction are tested with the Phase 0 harness.

Exit: the flagship case works: insert a customer, an invoice and an attached file in one transaction; kill the process at every injected point; the result is all or nothing every time.

### Phase 4: operate it (about 3 to 4 weeks, can overlap Phase 3)

- Stable error-code set and mapping; audit log; configurable resource limits (body, file, result size, connections, transaction size, statement size).
- Permissions beyond collections: database, table, bucket, object, with one enforcement point used by every entry path.
- Roles and service accounts.
- Backups that include the whole new state (schema, indexes or a rebuild marker, objects, catalog, permissions); incremental backups evaluated, point-in-time recovery designed, documented as not yet implemented.
- Metrics for objects, dedup ratio, WAL (journal) and queries.
- OpenAPI document, validated in CI against the server.

### Phase 5: SQL (about 8 to 12 weeks)

- `sqlparser` for parsing; our own AST validation, logical plan, optimiser (index selection, join order for small cases), executor, parameterised statements.
- `CREATE/ALTER/DROP`, `INSERT/UPDATE/DELETE/SELECT`, `WHERE`, `ORDER BY`, `LIMIT/OFFSET`, `GROUP BY/HAVING`, joins, aggregates, `DISTINCT`, `BEGIN/COMMIT/ROLLBACK`, `EXPLAIN`.
- A documented dialect (PowerSQL), with object statements for files in the dialect.
- `powerdb shell`, a SQL console in the dashboard, a SQL SDK method.
- Fuzzing of the parser and planner; a differential test against SQLite for the subset both support.

### Phase 6: developer experience and release (continuous, with a final push of about 3 to 4 weeks)

- Docs in the brief's structure, every command run by a test; README rewritten around the new positioning once the features it describes exist.
- Examples, and the Invoice Manager demonstration app.
- Python, Go and Rust clients after the protocol settles.
- Benchmarks with methodology: insert, point and range lookup, scans, aggregation, joins, transactions, concurrency (1 to 500 clients where the hardware allows), large and many small files, duplicate files; comparison against SQLite and PostgreSQL where meaningful, reported even when PowerDB loses.
- Signed installers (needs an Apple Developer ID and a Windows certificate), SBOM, `cargo audit` and `cargo deny` clean, penetration-test preparation.

## Explicitly deferred

Replication and any high-availability claim; PostgreSQL wire-protocol compatibility (made possible later by a correct SQL layer); distributed anything; vector and semantic search (the type system leaves room); local-first sync (object and row ids are already content-stable or id-stable, which keeps it possible); full-text search beyond a design note.

## Honest size

The phases above add up to roughly **6 to 9 months of focused work** for a team that already understands this code, and longer for one person. The brief's own checklist ("launch-ready") is realistic only after Phase 5 and the Phase 6 hardening. Phase 1 alone delivers a useful, shippable product change (a file store with versions on top of what exists) and is the right first release (0.4).

## Risks

- **Reference-count bugs delete data.** Mitigation: the Phase 0 model test, run on every change to the commit path.
- **The typed row format is permanent.** Mitigation: decide it in writing first, with room for new types.
- **SQL scope grows without end.** Mitigation: each statement ships with tests and a differential check, and the dialect's limits are documented in `LIMITATIONS.md`.
- **A transaction layer on an LSM is easy to get subtly wrong.** Mitigation: the Phase 3 spike, snapshot isolation only at first, and the failure-injection harness.
- **fjall is a young dependency.** Already seen: one parser trusting stored lengths (guarded). Keep the fuzzing and the checksum guard; track upstream.

## First step

Phase 0 can start immediately and changes no behaviour. The decisions at the top need your answers before Phase 1.
