# Sync Engine Design

Synchronization is intentionally staged. Phase 0/1 performs local inventory work; Phase 2 persists that inventory and synchronization intent in a restart-safe SQLite journal.

## Local inventory

The scanner:

1. validates the workspace root,
2. reads `.atrisbridgeignore` when present,
3. applies non-optional safety exclusions,
4. does not follow symbolic links,
5. recursively walks regular directories,
6. hashes regular files with BLAKE3,
7. returns aggregate statistics and a bounded UI preview,
8. passes the complete inventory to the durable journal,
9. records a scan history row and the latest successful scan timestamp.

Unreadable entries are surfaced as warnings rather than silently interpreted as deletions.

## Built-in exclusions

The current safety baseline excludes:

- `.git`, `.vs`, `.idea`, `.next`,
- `node_modules`, `dist`, `build`, `bin`, `obj`, `target`, `coverage`,
- `.env` and `.env.*`,
- `.pem`, `.key`, `.pfx`, `.p12` files.

Custom project rules are applied through `.atrisbridgeignore` using gitignore-compatible syntax.

## Durable SQLite journal

Phase 2 stores state in `atrisbridge.db` under the OS application-data directory. SQLite runs with foreign keys enabled, WAL journaling, and a bounded busy timeout.

The core tables are:

- `workspaces` — local roots and workspace-level configuration,
- `scan_runs` — immutable scan summaries and warnings,
- `file_entries` — local, remote, and last-synchronized observations per relative path,
- `pending_operations` — durable future transport actions,
- `app_meta` — migration markers and application-level metadata.

Each file entry can retain:

```text
workspace_id
relative_path
local_present
local_size
local_modified_at
local_hash
remote_id
remote_size
remote_modified_at
remote_hash
last_synced_hash
state
tombstone
first_seen_at
last_seen_at
last_synced_at
last_seen_scan_id
```

This separation makes comparisons explicit and restart-safe.

## Local reconciliation states

The journal currently derives the following states while scanning:

- `local_only` — present locally with no synchronized baseline,
- `synced` — current local hash matches the last synchronized hash and no known remote divergence exists,
- `local_modified` — local content changed relative to the synchronized baseline,
- `local_deleted` — a previously synchronized local file disappeared,
- `removed_before_sync` — an unsynchronized local-only file disappeared; this never creates delete intent,
- `remote_only` — reserved for a known remote file that is not present locally,
- `remote_modified` — local still matches baseline but known remote state changed,
- `conflict` — both observations cannot be reconciled safely without an explicit decision.

Remote states become fully active when Phase 3 introduces a provider transport.

## Tombstone rule

A missing file is not automatically equivalent to an authorized delete.

AtrisBridge only creates a tombstone when a missing local file has a known `last_synced_hash` and there is no known remote divergence. An unsynchronized file that disappears becomes `removed_before_sync` with no tombstone. If remote state is already known to have changed, the result is a conflict rather than delete intent.

Future provider deletion will still verify remote state and prefer provider trash/recoverable deletion instead of permanent removal.

## Pending operation journal

The Phase 2 schema already reserves durable operations for:

- upload,
- download,
- remote trash,
- local restore,
- keep-both conflict resolution.

No transport code creates or executes these operations yet. Phase 3/4 will add a planner that writes operations only after provider state has been reconciled.

## Planned synchronization order

1. Backup: local → remote, no remote-to-local mutation.
2. Pull/restore: explicit remote → local operation with overwrite protection.
3. Two-way: only after conflict and tombstone semantics are tested.

## Conflict rule

Two sides changing relative to the same `last_synced_hash` is a conflict. Modification time alone is never sufficient to auto-resolve it.
