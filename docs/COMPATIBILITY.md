# Compatibility

What may change, what must not, and how upgrades are handled. This is the policy the roadmap in [ROADMAP.md](ROADMAP.md) has to respect.

## Status

PowerDB is pre-1.0. The `0.x` versions may change the HTTP API and the on-disk layout, but **only with a documented migration and never silently**. 1.0 is reserved for when the layout, the API, the transaction semantics and the migration policy are stable.

## HTTP API

- `/api/v1` is the current API: collections, records, files, auth, keys, users, stats, maintenance.
- New features are added as **new routes or new optional fields**. An existing route, field or status code does not change meaning inside `v1`.
- A change that cannot be made that way goes in `/api/v2`. `v1` keeps working for at least two minor releases after `v2` ships, the changelog says which routes are deprecated, and the SDK supports both.
- Before any `v1` route is removed, a compatibility test must exist for it today. **That suite does not exist yet** (the API tests cover behaviour, but not as a frozen contract). Writing it is the first roadmap item, because every later change is checked against it.

## SDK

`powerdb-sdk` follows the API. Its public methods are not removed within a minor series; new ones are added. It is versioned with the server (0.3.x ↔ 0.3.x).

## On-disk layout

- The layout number is in `FORMAT`. See [STORAGE_FORMAT.md](STORAGE_FORMAT.md).
- **Forward:** a binary refuses a directory from a newer layout and touches nothing.
- **Backward (upgrade):** a binary upgrades an older layout in place, step by step, after copying `meta/` to `pre-migration-v<N>/`. The blob log is never rewritten by a migration.
- **Downgrade:** not supported once an upgrade step has run. Going back means restoring a backup taken before the upgrade, or the `pre-migration-v<N>/` copy. This will be stated in the release notes of any release that changes the layout.
- A migration must be tested on a directory built by the **previous release's binary**, not only on a fixture produced by the current one. No such test exists yet because there has been no migration.

## Backups

A backup records the layout it was taken at. Restoring it with a newer binary runs the same upgrade. Restoring with an older binary is refused.

## The existing record model during the roadmap

The collection model (JSON records, file fields, rules) stays valid. In the planned design a collection is a table of a fixed shape, so existing collections, rules, API keys, signed URLs and file ids keep working after the migration. Specifically these must not change:

- record ids and their time-ordered form;
- file ids (manifest hash) and the signed-URL scheme;
- the rule expression language;
- API key and user records;
- `powerdb check` and the backup and restore commands.

## Deprecation process

1. Mark deprecated in the changelog and in the API reference.
2. Ship the replacement in the same release.
3. Keep the old behaviour working for at least two minor releases.
4. Remove only at a version boundary, with the migration written down.
