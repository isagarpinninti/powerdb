# Design: types, tables, constraints and indexes (Phase 2)

> **Status: slices 2a and 2b are built.** 2b is `engine/src/types/schema.rs` (tables, columns with stable ids, schema versions, the changes of decision 4 and reading an old row under a new schema) with 11 tests, including a model test of random schema changes against rows that were written under older schemas and never rewritten. Its first version of the "dropped column ids are never reused" test missed the case the model found (dropping the column with the highest id); both now cover it. Persisting the catalog comes with slice 2c, because it shares the layout change.
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
| 2c | Typed rows in storage; collections as an adapter; layout 3 migration | The contract test unchanged; upgrade test from a 0.4.0 data folder; `check` extended. |
| 2d | Constraints and foreign keys in the commit | Model test with every constraint kind and random operations; crash points around the new writes. |
| 2e | Secondary indexes | `check` proves them equal to the rows after every step of the model test. |
| 2f | `FILE` and `FILE[]` columns | Reference counting through the model test; deleting a row frees its files; an object cannot be removed while a row refers to it (or the policy chosen is stated). |
| 2g | API, SDK, CLI and dashboard for typed tables | Contract pins for the new routes; real-server tests. |

SQL (Phase 5) builds on 2a to 2f and is deliberately not started until they are solid, because a SQL layer over untyped rows would have to be redone.

## Open questions

1. **`TIMESTAMP` without a zone.** Many systems have both; some find the pair confusing. Keeping both follows the brief. The risk is users picking the wrong one; the docs and the dashboard will say which to use (almost always `TIMESTAMPTZ`).
2. **Integer primary keys with auto-increment.** A counter is a hot spot under a single commit lock only if it is slow, and it is not; but it must survive a crash without reusing a value. It will be stored in the same batch as the row that used it.
3. **How much of `CHECK`.** The existing rule-expression language covers comparisons, logic and `~`. Anything needing functions (`length()`, `lower()`) is a small, closed list added deliberately, not an open expression evaluator.
4. **Foreign keys across collections that already hold dangling ids.** Adding a key to an existing relation field with bad data is refused with the first bad row; there will be a tool to list all of them.
