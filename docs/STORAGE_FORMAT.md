# On-disk format (layout 1)

This describes the format written by version 0.3.1. It is the baseline every migration starts from. Layout number: **1** (`FORMAT_VERSION` in `engine/src/storage/mod.rs`).

## Directory

```
data/
  FORMAT            the layout number as text, written atomically (temp file, then rename)
  meta/             fjall (LSM) store: the keyspaces below
  blobs/seg-NNNNNN.pdb   the chunk log, one file per segment (256 MiB by default)
  backups/          written by the backup command (not part of the live format)
```

A directory with no `FORMAT` file is either new or written before the marker existed; both are treated as layout 1. A newer layout is refused without touching anything. An older layout is upgraded in place (see below).

## Blob log

Entries are appended and never modified:

```
magic "PDB1" (4) | chunk key (32) | raw length (4, LE) | stored length (4, LE) | flags (1) | payload (stored length bytes)
```

- **Chunk key** is the BLAKE3 hash of the raw bytes when `flags == 0`. For an encrypted chunk (`flags != 0`) it is derived from the flags and that hash, so the same bytes stored in a public and in an encrypted collection do not share an entry and nothing is learned about encrypted content from the key.
- **flags** select the codec: `0` plain, other values name an encrypting codec. A reader that does not have the codec refuses the chunk.
- A segment is sealed when the next one starts. Space returns only by compaction, which rewrites live entries into a new segment and deletes the old one.
- A crash can leave a torn entry at the end of the newest segment. On open the last whole entry is found and the file is truncated there.

## Metadata keyspaces

All are fjall keyspaces under `meta/`. A commit writes across them in one atomic batch.

| Keyspace | Key | Value |
|---|---|---|
| `collections` | collection name | JSON `CollectionDef` (fields, rules, `encrypt`, `created_at`) |
| `records` | `collection` + `0x00` + record id | JSON `Record` (`id`, `collection`, `created_at`, `updated_at`, `fields`, `files`) |
| `files` | manifest id (32 bytes: BLAKE3 of the manifest) | manifest: ordered list of chunk key and length |
| `file_refs` | manifest id | `u64` LE: number of records that reference this file |
| `chunks` | chunk key (32 bytes) | pointer: segment `u32`, offset `u64`, stored length `u32`, raw length `u32`, flags `u8` (21 bytes, LE) |
| `chunk_refs` | chunk key | `u64` LE: number of manifests that contain this chunk |
| `gc_queue` | manifest id | a dead file waiting to have its chunks released |
| `seg_dead` | segment number | bytes of dead entries in that segment, to choose compaction candidates |
| `users`, `keys`, `sys` | (auth module) | users with Argon2id hashes, API key hashes and scopes, server secrets |

Record ids are time-ordered strings: a 48-bit millisecond clock, a counter and random bits, so ordering by key is ordering by creation time.

## Identity

- A file's identity is the BLAKE3 hash of its manifest, so identical bytes chunked the same way have one identity. It is stable across backup, restore and migration because it depends only on content.
- A record's identity is its id within its collection. Neither depends on a physical path or an offset; offsets live only in `chunks` and are rewritten by compaction.

## Versioning and upgrades

`format::ensure` runs on every open:

1. No marker: record the current layout.
2. Marker newer than the binary: refuse, leaving the directory untouched.
3. Marker older: copy `meta/` to `pre-migration-v<N>/`, run each migration step in order (each brings layout `n` to `n + 1`), rewrite `FORMAT` atomically after each step, and stop at the first failure, naming the safety copy.

The blob log is not copied and a migration must not rewrite it in place.

**Status.** `MIGRATIONS` is empty. The machinery is tested with injected steps; it has not run on a real layout change.

## What this layout cannot represent

These are why the roadmap includes a layout change (see [ROADMAP.md](ROADMAP.md)):

- Typed values beyond JSON: no 64-bit integers, decimals, timestamps with zones, uuids, arrays or enums as distinct stored types.
- Any index other than the primary key range.
- A file independent of a record: there is no catalog of objects, buckets, keys or versions.
- Reference counts that count anything other than record fields.
- Per-record or per-object metadata outside the record's own JSON.
