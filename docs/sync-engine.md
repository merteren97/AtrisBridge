# Sync Engine Design

Synchronization is intentionally staged. Phase 0/1 performs only local inventory work.

## Local inventory

The scanner currently:

1. validates the workspace root,
2. reads `.atrisbridgeignore` when present,
3. applies non-optional safety exclusions,
4. does not follow symbolic links,
5. recursively walks regular directories,
6. hashes regular files with BLAKE3,
7. returns aggregate statistics and a bounded preview,
8. records the latest successful scan timestamp.

Unreadable entries are surfaced as warnings rather than silently interpreted as deletions.

## Built-in exclusions

The current safety baseline excludes:

- `.git`, `.vs`, `.idea`, `.next`,
- `node_modules`, `dist`, `build`, `bin`, `obj`, `target`, `coverage`,
- `.env` and `.env.*`,
- `.pem`, `.key`, `.pfx`, `.p12` files.

Custom project rules are applied through `.atrisbridgeignore` using gitignore-compatible syntax.

## Planned durable journal

Before upload support, Phase 2 will introduce a SQLite journal containing at least:

```text
workspace_id
relative_path
local_hash
remote_hash
last_synced_hash
local_modified_at
remote_modified_at
remote_file_id
state
sync_version
```

This makes comparisons explicit and restart-safe.

## Planned synchronization order

1. Backup: local → remote, no remote-to-local mutation.
2. Pull/restore: explicit remote → local operation with overwrite protection.
3. Two-way: only after conflict and tombstone semantics are tested.

## Conflict rule

Two sides changing relative to the same `last_synced_hash` is a conflict. Modification time alone is never sufficient to auto-resolve it.

## Delete rule

A missing file is not immediately equivalent to an authorized delete. Future synchronization will persist tombstones and prefer provider trash/recoverable deletion instead of permanent removal.
