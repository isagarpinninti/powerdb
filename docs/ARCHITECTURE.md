# Architecture (assessment of version 0.3.1)

This is an audit of what PowerDB is today, written before any change toward a general-purpose database. It states what the code does, where the code does not match the target in the product brief, and what would have to change. [DESIGN.md](DESIGN.md) remains the reference for the storage design and the measured figures; this page is the gap analysis.

Everything below was read from the source at commit `4bf2518` unless it is marked **not verified**.

## What exists

```
                 clients: HTTP /api/v1, TypeScript SDK, dashboard, desktop app
                                         │
        ┌────────────────────────────────┴───────────────────────────────┐
        │ api/    axum routes, auth middleware, rules, upload, files, oauth│
        ├──────────────────────────────────────────────────────────────────┤
        │ auth.rs users, API keys (scoped), JWT      security/  rule exprs, │
        │                                            crypto, magic bytes,  │
        │                                            signed URLs           │
        ├──────────────────────────────────────────────────────────────────┤
        │ storage/  Db: collections, records, files, commit, scan, gc,     │
        │           snapshot (hot backup), verify, format (migrations)     │
        └───────────────┬──────────────────────────────┬───────────────────┘
                        │                              │
              fjall LSM (meta/)                 blob log (blobs/seg-N.pdb)
     collections records files file_refs        append-only content-defined
     chunks chunk_refs gc_queue seg_dead        chunks, BLAKE3-addressed
     users keys sys
```

| Area | What it is | Where |
|---|---|---|
| Records | A record is a JSON map plus a map of `FileRef`s, keyed `collection \0 id` in one LSM keyspace. Ids are time-ordered. | `storage/model.rs`, `storage/mod.rs` |
| Schema | A `CollectionDef`: a list of fields of kind `text`, `number`, `bool`, `json`, `relation`, `file`, with `required`, `max`, `mime`, `relation` options. Plus rules and an `encrypt` flag. | `storage/model.rs` |
| Files | Chunked with FastCDC (256 KiB / 1 MiB / 4 MiB), each chunk hashed with BLAKE3 and appended once to the blob log. A file is a manifest (ordered chunk hashes); its id is the manifest hash. Reference counts: records per file, files per chunk. | `storage/chunker.rs`, `segment.rs`, `mod.rs` |
| Commit | Chunks are staged to the log, then one fjall batch writes manifest, chunk index, reference counts and the record. | `storage/mod.rs` (`create`, `update`, `delete`) |
| Durability | `commit` (fsync log then journal), `interval`, `never`. | `Durability` in `storage/mod.rs` |
| Queries | `filter` and `sort` over one collection. A filter is parsed to an AST and evaluated against each decoded record. `scan` walks the collection's key range; there are no secondary indexes. | `api/query.rs`, `security/expr.rs`, `storage/mod.rs` (`scan`) |
| Auth | Roles `admin` and `user`; API keys scoped to collections and to read or read-write; Argon2id passwords; JWT; OAuth (Google, GitHub, generic). | `auth.rs`, `api/authn.rs`, `api/oauth.rs` |
| Authorization | One rule expression per action (`list`, `view`, `create`, `update`, `delete`) per collection; absent means administrators only. | `security/expr.rs`, `model.rs` |
| Encryption | Per collection: file chunks encrypted with ChaCha20-Poly1305. Records and metadata are not encrypted. | `security/crypto.rs` |
| Backup | Hot backup through a fjall cross-keyspace snapshot plus segment lengths; restore; integrity check (`verify.rs`). | `storage/snapshot.rs`, `ops.rs` |
| Format versioning | A `FORMAT` file, a migration framework with a pre-migration copy, refusal of newer formats. No migration has been needed yet. | `storage/format.rs` |
| API | `/api/v1`: auth, collections, records (JSON and multipart), file download and signed links, keys, users, stats, metrics, maintenance. | `api/mod.rs` |
| Tests | Engine integration tests for storage, API, auth, OAuth, security, hardening, crash (process kill), ops (hot backup under load), fuzz; four cargo-fuzz targets; dashboard and SDK tests; desktop lifecycle tests. | `engine/tests`, `fuzz/` |

## What the product brief asks for, against what exists

`Yes` means present and tested. `Partly` says what is missing. `No` means absent.

| Brief | Status | Notes |
|---|---|---|
| Rust core, axum, TLS, HTTP API, SDK, dashboard, desktop | Yes | Keep all of it. |
| Chunked, content-addressed, deduplicated file storage | Yes | The strongest asset; reuse unchanged. |
| Atomic record + files commit | Yes, for one record | A single call creates or updates one record with its files. Not a general transaction. |
| Streaming uploads and downloads, ranges | Yes | Memory is independent of file size (measured earlier; see README). |
| Encryption at rest | Partly | File chunks only, per collection. Records, indexes and metadata are in the clear. |
| Backups, restore, integrity check | Yes | Full backups only; nothing incremental; no point-in-time recovery. |
| Typed columns (ints, decimal, uuid, timestamp, enum, array...) | No | Fields are text, number (JSON number), bool, json, relation, file. |
| Tables, catalog, `ALTER` | Partly | Collections with a stored schema; `PATCH` replaces the schema. Not versioned; no data migration. |
| Primary key choices (uuid, integer, auto-increment) | No | Ids are generated, time-ordered strings. |
| Secondary indexes (B-tree, unique, composite) | No | Only the primary key range is indexed; any other filter is a scan. |
| Constraints: unique, check, default, foreign key | Partly | `required`, `max` and MIME rules exist. **Relation fields are validated as strings only: nothing checks the target exists, and deleting a target does not touch referrers** (read in `model.rs`; no check found in `storage/mod.rs` or `api/records.rs`). |
| Relationships: one-to-many, many-to-many | No | A relation field holds an id; nothing joins or enforces. |
| Standalone objects / buckets / keys | No | A file exists only as a field of a record. A record is required to store a file. |
| Object versions, soft delete, retention | No | A replaced file is released and garbage-collected. |
| Object metadata queries | No | Name, size and MIME are in the record's `FileRef`; not queryable across records. |
| File-to-file relationships | No | |
| `FILE[]` | No | One file per file field. |
| SQL | No | The query language is a filter expression over one collection. |
| Query planner, `EXPLAIN` | No | |
| Multi-statement transactions, `BEGIN`/`COMMIT`/`ROLLBACK` | No | One writer at a time for the commit step; readers see snapshots. |
| MVCC, isolation levels | Partly | fjall gives snapshot reads of the LSM. No isolation levels are defined or tested. |
| Own write-ahead log | No, by design | Durability comes from fjall's journal plus the blob log's fsync. See "WAL" below. |
| Crash-recovery tests | Partly | Process kill (`SIGKILL`) with parallel writers is tested. Power loss, partial writes, corrupt WAL or chunk, interrupted compaction and checkpoint are not. |
| Failure injection framework | No | |
| Integrity checker | Yes | `powerdb check` verifies chunks and references. |
| Format versioning and migration | Yes (framework) | Never exercised on real data; only layout 1 exists. |
| Roles, API keys, service accounts | Partly | Two roles; keys scoped by collection and access; no custom roles, no service-account type, no per-database or per-object permissions. |
| Audit logging | No | |
| Rate limiting | Yes, for login and OAuth | Not general. |
| Metrics, health, structured errors | Partly | `/metrics` and `/healthz` exist; errors have kinds but not the stable code set the brief lists. **Not verified in detail.** |
| OpenAPI specification | No | |
| CLI shell, `powerdb bucket`, `object` | No | There is no SQL shell and no object commands. |
| Multiple databases in one server | No | One data directory is one database. |
| Replication, PITR, wire-protocol compatibility | No | The brief itself defers these. |
| Prometheus metrics | Yes | `/metrics`. |
| Benchmarks against SQLite / PostgreSQL / MySQL | No | `powerdb bench` and measured HTTP figures exist; no comparison runs. |

## Architectural conflicts

These are the places where the brief and the present design pull against each other. Each needs a decision before work starts.

1. **A file has no life of its own.** Today a file is reachable only through a record field, and its id is the hash of its manifest. The brief wants buckets, keys, versions and metadata independent of any record. Content-addressing stays: the manifest id identifies *bytes*. What is missing is an object catalog (bucket, key, version → manifest id, plus metadata) and reference counting that counts catalog entries as well as record fields.
2. **The record model is a JSON map.** Typed columns, constraints and SQL need a typed row representation and a catalog. Fields stored as JSON numbers cannot represent `INT64` or `DECIMAL` faithfully (JSON numbers are doubles in most clients). The type system must be added to the storage format, not just the API.
3. **No indexes beyond the primary key.** A SQL planner without secondary indexes is a table-scan engine. Index maintenance has to be part of the same atomic commit as the record.
4. **The commit path is a mutex plus one batch.** That is correct for one record. A multi-statement transaction needs buffered writes, conflict detection and a defined isolation level, on top of fjall's batch and snapshot primitives. Whether fjall's own transactional keyspaces fit is **not verified** and is the first thing to evaluate.
5. **"Implement a production-grade WAL."** The metadata journal is fjall's, and it is tested by that library's authors and by this project's kill tests. Writing a second WAL beside it would add a large correctness risk for no clear gain. The recommendation is to keep fjall's journal, document its guarantees precisely, and put the effort into failure injection around it. This departs from the brief and should be confirmed.
6. **Time-ordered string ids.** The brief asks for UUID, ULID and auto-increment keys. The current ids are neither, and "newest first" listing depends on them. They can stay as one option.
7. **`/api/v1` is collection-shaped.** The brief wants it preserved. The simplest way to keep it is to define a collection as a table with a fixed shape, so old clients keep working while new tables use new routes.
8. **Backups and restore know the current keyspaces.** Every new keyspace (objects, indexes, catalog) must be added to the snapshot, the verifier, the garbage collector and the migration, or backups silently omit it.

## Dangerous changes

In decreasing order of risk:

- Changing the key encoding or the record value format in the `records` keyspace. Existing databases would be unreadable without a migration that rewrites every record.
- Changing reference counting. A counting error deletes data that is still referenced, and the garbage collector does it silently later. Any change here needs a model-based test against a reference implementation.
- Touching the blob log. It is append-only and must never be rewritten in place by a migration (this is already a rule in `format.rs`).
- Replacing the commit path. It is the property the whole product is built on.
- Adding SQL on top of the current record model without a type system first.

## What can be reused as it is

The chunker, the blob log and the segment compaction, the content-addressed manifest model, the streaming and range code, signed URLs, the encryption codec, the hot-backup mechanism, the format and migration framework, auth (users, keys, JWT, OAuth), the rule-expression parser, the HTTP layer, the dashboard shell and the desktop app.

## What must be designed

The object catalog; the type system and row format; the schema catalog with versions; secondary indexes; foreign-key enforcement; the transaction layer; the SQL parser, planner and executor; the permission model beyond collections; the stable error-code set; the OpenAPI document.

## WAL, as it is today

Two logs are involved in a commit, in this order:

1. The blob log gets the new chunks and, in `commit` mode, is fsynced.
2. fjall's journal gets the batch (manifest, chunk index, counts, record). In `commit` mode it is fsynced before the call returns.

A crash between the two leaves unreferenced chunks, which compaction drops. A crash after the second leaves a complete commit. This is the whole protocol; there is no checkpoint, rotation or replay logic of the project's own, because there is no log of the project's own for the metadata. See [TRANSACTIONS.md](TRANSACTIONS.md).

## Where each future feature belongs

| Feature | Layer |
|---|---|
| Types, constraints, foreign keys, indexes, transactions, object catalog, versions, reference counting | Engine (`storage/`) |
| SQL parsing, planning, execution | A new `sql/` module over the engine |
| Permissions | Engine checks, enforced in one place, called by SQL, the HTTP API and the dashboard alike |
| Routes, validation of requests, error mapping | `api/` |
| Browsing, editing, file preview | Dashboard and SDK, calling the API only |

Nothing in the list above may be implemented only in the HTTP layer or the dashboard.
