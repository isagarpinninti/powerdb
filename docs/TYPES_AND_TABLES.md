# Design: types, tables, constraints and indexes (Phase 2)

> **Status: slices 2a to 2g are built.** 2g makes tables reachable: HTTP routes (`/api/v1/tables…`, administrators only), `powerdb table …`, `db.tables` / `db.table()` in the SDK and a Tables section in the dashboard. The 2f note below still describes the engine API the routes use. Uploads into file columns are now content-checked like other uploads. The JSON form of every type is in `engine/src/types/json.rs` and tested by `engine/tests/json_values.rs`; the routes by `engine/tests/tables_api.rs`, `cli_tables.rs` and the contract test.
>
> **Status before 2g (kept for the record): slices 2a to 2f are built.** 2f adds `file` and `file[]` columns: a row can hold files (one, or a list), stored in the same content-addressed chunk store as record files and bucket objects. A file is uploaded for a row (`stage_table_file`, then `insert_row_with` / `update_row_with`), or an object's file is attached (`file_value_of_object`). 13 tests, including a model over rows and objects sharing file contents that checks every file reads back and that, once the release queue is empty, the stored files are exactly the distinct files something still holds; four reference-counting rules were each shown to fail when broken; crash recovery is tested with file references in the workload.
>
> **What a file column holds, and a change from the design.** The design said a file column resolves to an *object reference*. It holds a reference to the stored **file** instead: its content identity, plus size, type and name. That reuses the reference counting already proven for records, and an object's file can still be attached. The consequence: a row's file does not depend on the object it came from (deleting every version of the object leaves the row's file intact, and the other way round), and there is no live link to a catalog entry. A restrict-while-referenced link to a specific object version would be a separate feature.
>
> **Accounting rules.** Every reference in a stored row counts as one use of the file: a file named twice in a list counts twice; records, objects and rows all count in the same total. A row's references are released when it is deleted (including by a cascade), when the value is replaced or set to null, when its table is dropped, and when its file column is dropped (which rewrites the rows to remove the value, in one batch with the catalog change). A file value must name a file that is stored (or staged with the same write) and carry its true size; it cannot be given a default, and a file cannot be a primary key, unique or indexed. A CHECK can read a file's labels (`doc != null && doc.size < 5000000`). The files of a table created with `encrypt` are encrypted at rest when uploaded through it; an existing file attached to a row keeps whatever protection it was stored with.
>
> **Not built:** file columns in an `ON UPDATE`-style cascade (not applicable: a file column is not a key).
>
> **Slices 2a to 2e:** 2e adds user-created secondary indexes (`Change::AddIndex` / `DropIndex`) and finding rows through them (`Db::scan_index`), on the same index entries 2d introduced. An index covers one or more columns of any indexable type; a lookup names the first *k* of its columns (`k` up to all of them) and returns the matching rows in index order, then primary-key order, one page at a time with an opaque cursor. The index behind a unique constraint or a foreign key can be searched by the constraint's name. A row with a null in any indexed column is not in the index (so a lookup cannot find "where this is null"). An index is maintained in the same batch as every write, built from the existing rows when it is added, checked and recomputed by `powerdb check`, and a column it covers cannot be dropped or retyped (it can be renamed). 12 tests, including a random model that adds and drops indexes while rows change, and restarts, and compares every lookup with a plain filter and sort; each maintenance rule was shown to fail when broken.
>
> **Measured** (one Apple-silicon laptop, release build, 100,000 rows, one run): building an index took about 108 ms; 200 lookups each returning 100 rows took 18 ms in total (about 0.09 ms each), against about 38 ms for one full scan of the table through `scan_rows`. That is the cost of finding rows by an equal value, not a benchmark against any other database.
>
> **Not what the design said:** the index is built synchronously under the commit lock, in the same batch as the catalog change (writers wait while it runs), not in the background. A big table would hold writers for the length of the build; a background build that becomes visible when complete is still to do. Range scans (greater than, less than) and ordering by an indexed column are not exposed yet: they come with the query layer.
>
> **A fix found along the way:** a CHECK on a column called `id` rejected every row, because the shared expression language read a bare `id` as the record's own id (empty for a table row). A real field now wins over those names; collections cannot have such a field, so nothing changes for them.
>
> **Slices 2a to 2d** are described below, and 2d first:
>
> **Slice 2d:** 2d adds constraints inside the commit (`engine/src/storage/tables.rs`): generated values (`uuid`, `now`, `sequence`), UNIQUE, CHECK and FOREIGN KEY with restrict, cascade and set null, plus the index entries that make a uniqueness check and a "which rows refer to this one" lookup an index read instead of a table scan. 19 tests (behaviour of each kind, and a random model over four linked tables with each delete mode, restarts and `powerdb check` every ten steps), and crash tests, including one that crashes at every commit of an insert, insert, cascading-delete workload. Each rule was shown to fail when broken.
>
> **What the constraints do, and don't.**
> - **UNIQUE** (single or composite): a row with a null in any of the columns is not constrained, so many rows can hold null (as in SQL).
> - **CHECK** uses the same expression language as access rules and filters, read against a JSON view of the row. Numbers compare as floating point (so a very large int64 or a decimal compares approximately); a date, time or timestamp is its ISO text; a comparison with a null is false, so write `x == null || x >= 18` if null should pass. A check cannot read request values. A column a check reads cannot be dropped or renamed.
> - **FOREIGN KEY** refers to another table's whole primary key (or its own). A null in any of its columns means it refers to nothing. On delete: *restrict* (the default) refuses; *cascade* deletes the rows that refer to it, and theirs, up to 10,000 rows per delete (more is refused); *set null* clears the columns (they must be nullable, have no default and not be in the primary key). A delete is planned in full first (everything that cascades, then restrict and set null on the rows outside that set) and then written as one atomic batch, so a refusal anywhere refuses all of it. A table that others refer to cannot be dropped.
> - **Generated** columns fill in when an insert leaves the column out; an explicit value wins. A NOT NULL generated column can only be added to an empty table (old rows cannot be generated for after the fact). A sequence does not reuse a number after a delete or a restart, and an insert that fails does not use one up. `uuid` makes a version 7 (time-ordered) UUID.
> - Adding a constraint to a table that already has rows validates every row first (and writes the index entries in the same batch as the catalog change); if one row fails, nothing changes. Writers wait while that runs. A column named by a unique constraint or foreign key cannot be dropped or have its type changed.
> - **Not built:** `ON UPDATE` (a primary key cannot change anyway), deferred constraints, and a cap of the memory a very large constraint-adding scan uses.
>
> **Slices 2a, 2b and 2c** are described below.
>
> **Earlier:** 2c, 2b and 2a 2c (`engine/src/storage/tables.rs`, layout 3) stores typed tables: create, alter, drop, and insert, read, update, delete and scan rows by primary key, with `powerdb check` verifying them, 12 tests (behaviour, a random row-and-schema model test with restarts, damaged-data detection) and the crash tests extended to table writes. It is engine-level: no HTTP routes, CLI or dashboard yet (slice 2g).
>
> **A change from the design below.** Decision 4 says existing collections become tables, which means rewriting every record. 2c does not do that: **typed tables live beside collections**, in new keyspaces; collections, records and the `/api/v1` collection routes are untouched, and the layout migration rewrites nothing, so it is cheap to reason about and easy to back out. Unifying the two (a collection as a view over a table) is deferred until typed tables have earned trust. Cost: for a while there are two record models, and constraints (2d), indexes (2e) and file columns (2f) apply to tables only.
>
> **Two rules the row format forces.** A null is an *absent* value, and a column's default fills in for an absent value. So **a column with a default cannot hold an explicit null** (it would read back as the default), and an update that tries is refused with a message saying so. And **a primary key is fixed once a table is stored** (its columns cannot be dropped, made nullable or retyped, not even widened, because every row's key would change); a key column can be renamed.
>
> **Also built earlier:** slices 2a and 2b. 2b is `engine/src/types/schema.rs` (tables, columns with stable ids, schema versions, the changes of decision 4 and reading an old row under a new schema) with 11 tests, including a model test of random schema changes against rows that were written under older schemas and never rewritten. Its first version of the "dropped column ids are never reused" test missed the case the model found (dropping the column with the highest id); both now cover it. Persisting the catalog comes with slice 2c, because it shares the layout change.
>
> **Slice 2a is built** (`engine/src/types/`): the type system, the row codec and the key codec, as a library that changes nothing on disk.
> It has 16 tests (seeded random round trips for every type, order of keys against an independent oracle, 15 malformed-row cases, mutation fuzzing
> of both decoders under an allocation guard) and two libFuzzer targets (`row`, `key`; compiled, run by the weekly fuzz workflow, not run locally).
> Each check was shown to fail when the thing it guards is broken. Slices 2b to 2g are not started.
>
> What building it showed: **(1)** the strict, canonical row format caught that `serde_json` by default does not round-trip floats exactly, so a
> JSON value written by PowerDB could fail to read back; the `float_roundtrip` feature of the crate is now on (no new dependency; it also makes
> every float in existing records parse exactly). **(2)** A typical invoice row is 80 bytes against 193 bytes as JSON (about 41%), though the row
> relies on the catalog for column names, which JSON carries in every record.

This is the design for the second phase of [the roadmap](ROADMAP.md). It is written before any code because the row encoding becomes part of the on-disk format: a mistake here is paid for in every migration afterwards. Nothing in this page is built yet. Where a choice is a recommendation, the alternatives and the reason are given so it can be overruled.

## What has to be true when it is done

1. A column has a real type: an integer is a 64-bit integer, a decimal is exact, a timestamp has a zone. Nothing is stored as "a JSON number" unless the column says so.
2. Constraints (`NOT NULL`, `UNIQUE`, `CHECK`, `DEFAULT`, `FOREIGN KEY`) are enforced inside the same atomic commit as the write. Not in the API layer.
3. A secondary index is part of the commit that changes the row, and `powerdb check` can prove it matches the rows.
4. A `FILE` or `FILE[]` column holds object references (the Phase 1 catalog), counted for garbage collection like any other reference.
5. Every existing collection, rule, API key, signed URL, file id and record id keeps working after the upgrade ([COMPATIBILITY.md](COMPATIBILITY.md)).
6. New types can be added later (vector, full text) without invalidating a single stored row.

## Decision 1: how a row is encoded

**Recommendation: a self-describing, tagged binary row, keyed by stable column ids.**

```
row  := version:u8  count:varint  ( column_id:varint  type_tag:u8  value )*
```

- A **column id** is a number assigned when the column is created and never reused. A column's name lives only in the catalog, so a rename touches no row. A column added later is simply absent from old rows and reads as its default. A dropped column's values are ignored on read and disappear when the row is next rewritten.
- A **type tag** is stored with each value, so a row can be decoded without the schema and a new type is a new tag. This is what makes decision 6 above cheap.
- A null is the column being absent.

Alternatives, and why not:

| Alternative | Why not |
|---|---|
| Positional rows with a schema version (like a classic row store) | Smaller and faster, but a new column or a rename needs the old schema versions kept forever and every reader to carry them; a bug there corrupts reads silently. |
| Keep JSON | Cannot represent 64-bit integers or decimals faithfully; slow to decode; no type safety. |
| Columnar | A different engine. Not for this workload. |

Cost: a tag and an id per value, a few bytes. Measured later against the current JSON size on the demo data; if it is not smaller than JSON, that is a result worth knowing before committing.

## Decision 2: the types

| Type | Stored as | Notes |
|---|---|---|
| `BOOLEAN` | 1 byte | |
| `INT8` `INT16` `INT32` `INT64` | varint (zig-zag) | One tag per width, so range is checked on write. |
| `UINT8` `UINT16` `UINT32` `UINT64` | varint | |
| `FLOAT32` `FLOAT64` | IEEE 754, little-endian | `NaN` is stored but sorts last; `-0` and `0` compare equal. |
| `DECIMAL(p,s)` | sign, scale, unscaled integer (up to 38 digits) | Exact. Arithmetic in SQL is exact; conversion to float is explicit. |
| `TEXT` | UTF-8, length-prefixed | `STRING` is an alias. A maximum length is a constraint, not a type. |
| `UUID` | 16 bytes | |
| `DATE` | days since 1970-01-01, `i32` | |
| `TIME` | microseconds since midnight, `i64` | |
| `TIMESTAMP` | microseconds since the epoch, `i64` | No zone: a wall-clock value. |
| `TIMESTAMPTZ` | microseconds since the epoch, UTC, `i64` | Always an instant. The zone is a display matter. |
| `JSON` / `JSONB` | one stored form, canonical (sorted keys, no duplicates) | `JSONB` is the same bytes with an indexable form; a single representation until a second one is needed. |
| `BINARY` | length-prefixed bytes | For small values. Large content is an object, not a column. |
| `ENUM(a,b,c)` | the member's index | Members can be appended, never removed or reordered. |
| `ARRAY(T)` | count, then values of `T` | One level of nesting to start. |
| `FILE` / `FILE[]` | object references | See decision 5. |
| `NUMBER` (legacy) | JSON number | What today's `number` fields become, so nothing already stored changes meaning. Not available for new columns. |

The numbered gaps in the tag space are left on purpose for `VECTOR`, `EMBEDDING` and full-text types.

## Decision 3: keys and index entries sort correctly

Index and primary-key keys must compare as bytes the same way the values compare as values. Each type has an order-preserving encoding: signed integers with the sign bit flipped and big-endian; floats with the standard total-order transform; text as UTF-8 with an escaped terminator so a composite key `("a", "b")` cannot collide with `("ab", "")`; decimals normalised first. This is the part most likely to hide a bug, so it gets property tests (random values: the byte order equals the value order, and encode then decode is the identity) before anything is built on it.

## Decision 4: the catalog and schema changes

A **table** is a catalog entry: a name, a stable id, its columns (id, name, type, nullability, default, constraints), its indexes, and a **schema version** that increases on every change. Changes are applied as one commit that writes the new catalog entry. Which changes are cheap and which are refused:

| Change | Cost |
|---|---|
| Add a nullable column, or one with a default | Nothing is rewritten. |
| Rename a column | Catalog only. |
| Drop a column | Catalog only; values disappear lazily. |
| Add `NOT NULL`, `UNIQUE`, `CHECK`, `FOREIGN KEY` to existing data | A validating scan first; refused with the first offending row if any exists. |
| Widen a type (`INT32` to `INT64`) | Catalog only: the reader accepts the narrower tag. |
| Narrow or change a type | Refused unless the table is empty. Converting needs an explicit migration the user writes. |

Existing **collections** become tables in the catalog with one reserved shape: `id` (time-ordered text, the primary key), `created_at`, `updated_at`, and their old fields as columns, `number` fields as `NUMBER`. The `/api/v1` collection routes are an adapter over this, so their behaviour is unchanged and pinned by the contract test.

## Decision 5: constraints and relationships inside the commit

All enforced while the commit lock is held, in the same batch as the write:

- `NOT NULL`, `CHECK` (an expression over the row, using the existing expression parser extended with types), `DEFAULT` (including `now()` and generated UUID, ULID or sequence values).
- `UNIQUE`: implemented as a unique index. The index entry and the row are written in one batch, so two concurrent inserts of the same value cannot both succeed.
- `FOREIGN KEY`: checks that the target row exists on insert and update; on delete of the target, applies the declared action (`RESTRICT`, the default, `CASCADE`, `SET NULL`). A cascade is part of the same batch, with a limit on how much one statement may cascade, so it cannot lock up the writer.
- `FILE` and `FILE[]`: each reference counts as one use of the file, exactly like an object version does today, so the garbage collector needs no change. A `FILE` column holds an object id and version, not a copy.

This is where the most care is needed: a missed reference-count update deletes data. The Phase 0 model test is extended to tables with every constraint kind before any of this ships.

## Decision 6: secondary indexes

B-tree style, kept as ordinary entries in the same LSM: `index id, encoded key columns, primary key` with an empty value. They are written in the same atomic batch as the row, so there is no window in which they disagree. `powerdb check` recomputes every index from the rows and compares. Unique, composite and (later) partial indexes use the same layout. Creating an index on existing data is a background build that becomes visible only when complete; until then queries do not use it and writes keep it up to date.

## Plan, in slices (each ends with tests and a release candidate)

| Slice | What | Proof |
|---|---|---|
| 2a | The type system and the row and key codecs, as a library with no storage change | Property tests: encode/decode identity for every type; key order equals value order; fuzzing the decoder with a huge-allocation guard. Size compared with JSON on the demo data. |
| 2b | The catalog: tables, columns, schema versions, `ALTER` | Model test of random schema changes against old rows; every cheap change shown not to rewrite data. |
| 2c | Typed tables in storage beside collections; layout 3 | Done: the upgrade test from 0.4.0 data (with objects) and from 0.3.1; `check` extended; model and crash tests. The collection-adapter idea is deferred (see the status note). |
| 2d | Constraints and foreign keys in the commit | Done: the model test over linked tables, crash points around the new writes, a crash at every commit of a cascading workload. Also introduced the index entries slice 2e will expose (the unique and foreign-key indexes). |
| 2e | Secondary indexes | Done, except the background build: a model test compares every lookup with a plain filter and sort; `check` proves the index equal to the rows. |
| 2f | `FILE` and `FILE[]` columns | Done: the model test over rows and objects; deleting a row (or cascade, or dropping the table or the column) frees its files; the policy is stated: a row holds the file, not the object, so removing an object never affects a row. |
| 2g | API, SDK, CLI and dashboard for typed tables | Done: contract pins for the new routes; real-server tests through HTTP, the CLI binary and the SDK; the dashboard checked by hand in a browser (no automated UI test). |

SQL (Phase 5) builds on 2a to 2f and is deliberately not started until they are solid, because a SQL layer over untyped rows would have to be redone.

## Open questions

1. **`TIMESTAMP` without a zone.** Many systems have both; some find the pair confusing. Keeping both follows the brief. The risk is users picking the wrong one; the docs and the dashboard will say which to use (almost always `TIMESTAMPTZ`).
2. **Integer primary keys with auto-increment.** A counter is a hot spot under a single commit lock only if it is slow, and it is not; but it must survive a crash without reusing a value. It will be stored in the same batch as the row that used it.
3. **How much of `CHECK`.** The existing rule-expression language covers comparisons, logic and `~`. Anything needing functions (`length()`, `lower()`) is a small, closed list added deliberately, not an open expression evaluator.
4. **Foreign keys across collections that already hold dangling ids.** Adding a key to an existing relation field with bad data is refused with the first bad row; there will be a tool to list all of them.
