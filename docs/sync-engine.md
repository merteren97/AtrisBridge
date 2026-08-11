# Sync Engine Design

AtrisBridge owns synchronization semantics. Provider transports supply observations and narrowly approved mutations; they do not decide which copy wins.

## Evidence model

Every synchronization decision is based on three evidence classes kept separately in SQLite:

1. **Current local observation** — presence, size, modification metadata, BLAKE3.
2. **Current remote observation** — presence, provider object ID, size, provider checksum type/value.
3. **Last successfully synchronized baseline** — local BLAKE3 plus remote provider checksum evidence accepted only after a verified transfer.

Modification time is useful metadata but is never sufficient conflict authority.

The local scanner applies built-in secret/generated exclusions, `.atrisbridgeignore`, and no-symlink traversal. AtrisBridge transfer/recovery `.part` and `.bak` artifacts are also excluded so recovery mechanics cannot become project content accidentally.

## Durable journal

The core `file_entries` row keeps local, remote, and baseline evidence for each relative path. Phase-specific plan tables are additive:

- `backup_plans` / backup items — Phase 4 local → Drive,
- `restore_plans` / restore items — Phase 5 Drive → local,
- `sync_plans` / `sync_plan_items` — Phase 6 conflict-aware two-way plans,
- `sync_recovery_entries` — verified recovery copies retained after a Phase 6 remote-deletion → local-delete operation.

Phase 6 plan items persist the exact evidence reviewed at planning time: local presence/hash/size, remote presence/ID/size/checksum, baseline local hash, baseline remote checksum, action, state, errors, and any local recovery evidence required around an apply operation.

## Two-Way mode is explicit

A workspace must be switched explicitly to `two_way`. Changing the mode performs no synchronization.

The user flow is always:

1. **Prepare sync** — refresh local inventory and remote inventory, classify every path, persist a plan, mutate nothing.
2. **Review** — inspect uploads, downloads, deletion convergence, conflicts, and blocks.
3. **Run sync** — refresh both inventories again, require plan evidence to remain unchanged, and execute only items that are still safe.

A running backup, restore, or another two-way operation blocks a concurrent two-way execution for that workspace.

## Phase 6 decision table

| Local current state | Remote current state | Baseline | Decision |
| --- | --- | --- | --- |
| present | absent | none | `upload_create` |
| absent | present with stable ID/size/MD5 | none | `download_create` |
| present | present | none | `blocked` — unverified overlap |
| unchanged | unchanged | verified | skip |
| changed | unchanged | verified | `upload_update` |
| unchanged | changed | verified | `download_update` |
| changed | changed | verified | `conflict` |
| deleted | unchanged | verified | `remote_trash` |
| deleted | changed | verified | `conflict` — delete/modify |
| unchanged | deleted | verified | `local_delete` |
| changed | deleted | verified | `conflict` — delete/modify |
| deleted | deleted | verified | `acknowledge_delete` |

An incomplete historical baseline is blocked rather than silently upgraded into authority. Case-insensitive path collisions, unsafe/non-portable paths, symlink traversal, ignored paths, and missing provider evidence are also blocked.

## Upload and download execution

Phase 6 reuses the verified Phase 4/5 transfer patterns.

### Upload

For an approved local change AtrisBridge:

1. requires the current SQLite evidence to equal the reviewed plan evidence,
2. resolves a regular local file without escaping the workspace,
3. recomputes local BLAKE3 + size,
4. for create, verifies the remote path is still absent; for update, verifies remote ID/size/checksum,
5. performs a single restricted upload,
6. verifies the local file did not change during transfer,
7. requires provider size/checksum evidence for the resulting object,
8. commits a new synchronized baseline only if the current journal row still equals the exact planned evidence.

### Download

For an approved remote change AtrisBridge:

1. requires current journal evidence to equal the plan,
2. performs targeted Drive preflight,
3. downloads to a unique hidden sibling `.part` path,
4. verifies staging size + MD5 against the reviewed Drive evidence and stores local BLAKE3,
5. performs final remote stat and local target validation,
6. persists an `applying` recovery state,
7. creates a `.bak` for an update before replacing the target,
8. fingerprints the applied target,
9. commits the new baseline only against the exact planned journal evidence,
10. removes temporary recovery content only after the journal commit succeeds.

Interrupted apply states are recovered conservatively at application startup; uncertain content is preserved for manual inspection.

## Deletion convergence

Deletion propagation is deliberately stricter than content transfer.

### Local deletion → Google Drive Trash

A local deletion can propagate only when:

- the path had a complete synchronized baseline,
- the current Drive object still has the reviewed ID, size, and MD5,
- the local path is still physically absent during live preflight immediately before the provider mutation.

AtrisBridge then moves the **exact reviewed Google Drive file ID** to Trash using a narrow Drive API request. Permanent deletion is not exposed.

After the request, the managed remote path is checked again. If another object now occupies the same path, the operation does not treat that new object as the deleted object or clear uncertain evidence. A fresh reviewed plan is required.

Trash is a recoverable provider action, but AtrisBridge does not claim an atomic provider compare-and-swap between its checksum preflight and the Trash request. Another Drive client can still change the same object in that interval.

### Remote deletion → recoverable local delete

A remote deletion can remove a local file only when:

- the local BLAKE3 + size still equal the synchronized baseline,
- targeted Drive checks continue to prove the remote path is absent,
- the path remains safe and is not ignored.

Before local removal AtrisBridge:

1. copies the local file into its OS application-data recovery area,
2. verifies recovery BLAKE3 + size,
3. flushes that recovery file,
4. checks remote absence again,
5. persists an `applying` state containing recovery evidence,
6. re-fingerprints the local source,
7. performs another live remote-absence check,
8. removes the local workspace file,
9. commits deletion convergence, recovery metadata, and item completion in one SQLite transaction.

If journal completion fails, AtrisBridge restores the original local file only when the recovery copy still proves the expected fingerprint.

### Both sides already absent

`acknowledge_delete` clears the old baseline only after live local and targeted remote absence checks. This turns a previously synchronized deleted path into a fully converged absent state without deleting anything.

## User-restorable local recovery

Verified recovery copies created by `local_delete` remain under AtrisBridge application data and appear in the Two-Way UI.

**Restore locally** is intentionally not a sync operation. It:

- requires no backup/restore/two-way execution to be active,
- validates the recovery file is canonical under AtrisBridge's recovery root,
- verifies its BLAKE3 + size,
- refuses ignored/unsafe paths and any existing destination,
- stages and verifies the copy before placement,
- updates the file journal to `local_only`,
- marks the recovery record restored in the same transaction,
- never modifies Google Drive.

The recreated local-only file is considered by the next fresh reviewed two-way plan.

## Conflicts

Phase 6 deliberately has no last-write-wins policy.

- **modify/modify:** both current contents differ from the shared baseline → conflict.
- **local delete / remote modify:** preserve the remote modification → conflict.
- **remote delete / local modify:** preserve the local modification → conflict.
- **unverified overlap:** both sides exist without an accepted baseline → blocked.

Phase 6 surfaces these states and leaves both sides untouched. The user can resolve content manually and then prepare a fresh plan. Provider-native automatic merge or keep-both conflict resolution is future work rather than an implicit heuristic.

## Race boundaries

Fresh inventory and plan revalidation close stale-journal hazards, but separate filesystem/provider calls cannot create a distributed atomic transaction.

AtrisBridge therefore uses:

- exact reviewed evidence in SQLite conditional updates,
- targeted provider preflight,
- repeated live absence checks around destructive-looking operations,
- exact-ID Drive Trash instead of path-based remote deletion,
- postflight remote checks,
- local staging/recovery copies,
- user review before execution,
- fail-closed handling when evidence changes.

The remaining provider-side race windows are documented rather than hidden. Continuous watching/automatic execution is intentionally deferred until later phases.
