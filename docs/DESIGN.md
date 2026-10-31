# PowerDB design

One binary that stores structured records (JSON) and files in the same database, with the record and its files becoming visible together or not at all.
This document is the source of truth for what is built. Every performance figure in the repository is labelled **measured** (with the command that produced it)
or **assumed**; none is a goal presented as a result.

## Layout of a database directory

```
data/
  meta/            fjall (LSM) keyspaces: collections, records, files, file_refs, chunks, chunk_refs, gc_queue, seg_dead, users, keys, sys
  blobs/seg-NNNNNN.pdb   append-only chunk log, one segment file per 256 MiB; entries are  magic | blake3 hash | raw len | stored len | flags | payload
```

## Storage engine (WiscKey-style key/value separation)

* **Records and indexes** live in fjall's LSM tree. A record is small JSON plus references to files; the LSM is sorted by `collection \0 id`, and ids are time-ordered
  (48-bit millisecond clock + counter + random), so "newest first" is a reverse prefix scan with no sort.
* **File bytes never enter the LSM.** A file is split by FastCDC (content-defined: min 256 KiB, average 1 MiB, max 4 MiB) into chunks; each chunk is hashed with
  BLAKE3 and appended to the blob log once. A file is a *manifest*: the ordered list of chunk hashes and lengths. The file id is the BLAKE3 of the manifest.
* **Deduplication and delta updates fall out of content addressing.** Two records with the same file share one manifest; a new version of a large file that changes
  a few bytes shares every chunk the change did not touch (content-defined boundaries move only locally), so only the changed chunks are written. Fixed 1 MiB blocks
  would not do this: one inserted byte shifts every later block.
* **Reference counts**: `file_refs` (records per file) and `chunk_refs` (files per chunk). They change only inside the commit batch below.

## Atomicity: how a record and its files appear together

1. *Stage.* File bytes are chunked, hashed and appended to the blob log (and fsynced when `sync = commit`). Nothing refers to them yet.
2. *Commit.* Under one short mutex, one fjall write batch inserts the manifest, the new chunk index entries, the reference-count changes and the record. A fjall batch is atomic
   across keyspaces, so readers see the old state or the new state, never a record whose file is missing. A crash between 1 and 2 leaves unreferenced chunks in the log;
   they are dropped the next time that segment is compacted.
3. *Durability.* With `sync = commit` the log is fsynced before the batch and the journal is synced after it. With `sync = never` neither happens (faster; a power cut can lose recent commits).

What this is **not**: a general multi-statement transaction system. There is one writer at a time for the commit step (it is short: metadata only), reads never block, and
isolation is snapshot reads of the LSM. The single-call "create with files" and "update with files" are the transactions.

## CRUD paths

| Operation | What happens |
|---|---|
| create | stage chunks, then one batch: manifest + chunks + refs + record |
| read | one LSM point lookup, JSON decode; file bytes are read on request from the log by `pread` using the manifest (HTTP ranges map to chunks by binary search over cumulative sizes) |
| update | JSON merge into the record; a replaced file releases the old file reference (one counter) and takes a new one; only chunks not already stored are written |
| delete | one batch removes the record and decrements the file counter; a file whose counter reaches zero is queued for the garbage collector. The record (and so every URL for its files) is gone immediately; space is reclaimed later, off the request path |
| GC / compaction | releases the chunks of dead files, then rewrites segments whose dead-byte ratio is above a threshold and deletes the old segment |

## Where this deliberately departs from the first draft of the requirements

* **"Zero-copy `sendfile` and encryption at rest" cannot both hold for one file.** `sendfile` moves bytes from the page cache to the socket untouched; an encrypted chunk must be
  decrypted in user space first. Public collections store chunks in the clear and can be streamed straight from the log; private collections encrypt each chunk with
  ChaCha20-Poly1305 and are decrypted while streaming. Which one applies is a per-collection setting, and the dashboard says so.
* **`io_uring` is Linux-only.** The engine uses ordinary positioned I/O (`pread`/`pwrite`) from a blocking thread pool, which works everywhere; an `io_uring` backend is a later, Linux-only option.
  Development and every measurement here are on macOS.
* **"SQL injection is mathematically impossible"** is true only in the sense that there is no SQL. The filter language is parsed into an AST and evaluated against decoded records;
  user input is never concatenated into anything that is executed. The parser itself is fuzzed.
* **Magic-byte checking** stops a file whose bytes do not match its declared type (an `.exe` named `.png`). It does not detect malware, and a polyglot file that is valid as two formats passes.
* **"Sub-millisecond"** is a claim about the engine's own operations; it is measured (`powerdb bench`) and reported with its conditions, not promised for network round trips.


## Durability and what it costs (measured)

`--durability` chooses when a write is on disk before it is acknowledged:

| mode | meaning | creates over HTTP, 16 connections |
|---|---|---|
| `commit` (default) | the log, then the metadata journal, are fsynced before the reply | **333 / s**, p50 48 ms (one flush at a time, about 3 ms each) |
| `interval` (`--interval-ms`, default 100) | acknowledged once the operating system has it; a background thread fsyncs every interval. A process crash loses nothing; a power cut loses at most about one interval of recent writes and never leaves the database inconsistent | **57,900 / s**, p50 under 1 ms, p99 1 ms |
| `never` | no fsync until shutdown | 59,400 / s |

Measured on one MacBook Air (M2, internal SSD), release build, ApacheBench `-k -n 4000` against `powerdb serve` on 127.0.0.1, one run per row. ApacheBench is single-threaded and may be limiting the fast rows.
The storage library admits one fsync at a time and blocks other writers while it runs, so the `commit` rate is flat in the number of writers (checked in the engine without HTTP: 1, 4 and 16 threads all give about 335 commits/s). Combining
concurrent commits into one flush would raise it but requires combining their reference-count updates into a single batch; it is not built. The default is the safe mode on purpose: "fast" is something you choose, knowing what it risks.

Reads, measured the same way (16 connections): health check 160,000 / s; a record by id 63,000 / s (65,000 with an API key); list of the newest 20 42,000 / s; a filtered and sorted list over 300 records 14,500 / s; a 100 KiB file through a signed link 26,000 / s.
