# Phase 5 restore engine

Phase 5 adds an explicit **Google Drive → local workspace** restore path. It is intentionally not an automatic two-way synchronizer. AtrisBridge owns the decision model, local path policy, verification, rollback, and synchronized-baseline journal; rclone remains a restricted byte transport.

## User flow

Restore is always a two-step action:

1. **Prepare restore** refreshes the complete local BLAKE3 inventory and the bound Google Drive inventory. It persists a restore plan and changes no project file.
2. The user reviews safe restores and blocked entries.
3. **Run restore** refreshes both inventories again, revalidates every plan item, and executes only the still-safe entries.

Backup and restore actions are mutually disabled in the normal desktop UI while one of them is active.

## Decision table

AtrisBridge compares the current local evidence and current Google Drive evidence against the last synchronized baseline.

| Local state | Remote state | Baseline | Phase 5 decision |
| --- | --- | --- | --- |
| missing | present with ID + MD5 | not required | `create` local file |
| unchanged from baseline | changed from baseline | verified | `update` local file |
| unchanged | unchanged | verified | skip |
| changed | unchanged | verified | block; preserve local change |
| changed | changed | verified | block; manual conflict resolution |
| present | present | no verified baseline | block; unverified overlap |
| present | missing | any prior baseline | block; never delete local |
| missing | missing | any | skip |

A provider object without a stable ID, size, or MD5 evidence is blocked instead of being restored without verification.

Remote restore candidates also pass through the **same built-in and `.atrisbridgeignore` policy used by the local scanner**. A remote-only `.env`, private-key/certificate file, ignored generated directory, or user-excluded path is blocked rather than recreated locally. The ignore policy is evaluated during planning and again immediately before execution, so a rule added after plan preparation still stops the restore.

## Portable path boundary

A remote relative path must be safe to map onto every currently supported desktop filesystem. Phase 5 blocks:

- absolute paths, `.` and `..` segments,
- backslash-based remote names,
- control characters,
- `< > : " | ? *`,
- path segments ending in a dot or space,
- Windows reserved names such as `CON`, `NUL`, `COM1`, and `LPT1`,
- case-insensitive collisions such as `Readme.md` and `README.md`,
- paths excluded by AtrisBridge built-in safety or `.atrisbridgeignore` rules,
- any local parent or target that resolves through a symbolic link,
- a remote file whose target collides with an existing local directory.

Missing directories are created only during an approved execution, after the plan has been reviewed and after the remote object has passed targeted preflight verification.

## Staged download and verification

AtrisBridge never points rclone directly at the final project file. For each approved restore item:

1. Revalidate the persisted local and remote evidence.
2. Re-evaluate the current AtrisBridge ignore policy.
3. Perform a targeted Google Drive stat and require the planned remote ID, size, checksum type, and checksum.
4. Resolve the local target without following symlinks.
5. Download the object to a unique hidden sibling staging file, `.atrisbridge-<operation>.part`.
6. Compute BLAKE3 for local journal evidence and MD5 for comparison with Google Drive.
7. Require staging size + MD5 to match the reviewed remote evidence.
8. Perform another targeted remote stat and require the same remote identity/evidence.
9. Revalidate the local target immediately before apply.
10. Persist an `applying` marker with the downloaded BLAKE3 + size before mutating the project tree.
11. Apply the verified content with filesystem renames.
12. Fingerprint the final local target again.
13. Commit the new local/remote synchronized baseline in SQLite.

If staging verification, final provider verification, or local revalidation fails before apply, the regular staging file is cleaned up and the final project file remains untouched.

## Recoverable local apply

### Create

A `create` operation requires the target to remain absent. The verified staging file is renamed into place. If the SQLite baseline cannot be committed, AtrisBridge removes the new target only when its BLAKE3 + size still exactly match the verified downloaded content.

### Update

An `update` operation requires the current local target to continue matching the planned local BLAKE3 + size. AtrisBridge:

1. renames the original target to `.atrisbridge-<operation>.bak`,
2. renames the verified staging file into the target path,
3. verifies the new target,
4. commits the synchronized baseline,
5. removes the recovery `.bak` only after successful journal completion.

If final fingerprint/metadata verification, apply, or journal completion fails, AtrisBridge rolls back to the `.bak` copy only when doing so can be proven safe. If the restored target changed unexpectedly after apply, AtrisBridge preserves both files for manual inspection rather than overwriting uncertain user data.

## Restart recovery

Restore plan items have a distinct `applying` state. Before a local mutation, AtrisBridge persists the downloaded BLAKE3 + size. On the next application startup:

- interrupted `running` items may only have a staging file; the regular staging file is removed,
- interrupted `create` applies are rolled back only when the target still exactly matches the downloaded fingerprint,
- interrupted `update` applies restore the `.bak` copy when the current state proves that rollback is safe,
- uncertain targets are preserved for manual inspection,
- the item becomes `failed` and the plan becomes `partial`.

Interrupted restores are never silently retried or marked synchronized.

## Journal storage

The core AtrisBridge database remains schema v3 from Phase 4. Phase 5 adds `restore_plans` and `restore_plan_items` as idempotent feature-owned tables when the restore subsystem opens the existing SQLite database. No Phase 4 table is rewritten or destructively migrated.

The restore plan persists:

- expected local presence, BLAKE3, and size,
- expected remote ID, size, checksum type, and checksum,
- action and status,
- downloaded local BLAKE3 + size before apply,
- completion/error evidence.

A future database consolidation can move these additive tables into the central migration version without changing their safety semantics.

## Deliberate limits

Phase 5 restores regular file **content**, not complete cross-platform filesystem metadata. It does not promise to reproduce Unix executable bits, ACLs, ownership, alternate data streams, or provider-specific metadata.

Phase 5 also does not:

- delete local files because a remote file disappeared,
- write, delete, move, or purge remote data,
- automatically resolve conflicts,
- bypass built-in secret exclusions or `.atrisbridgeignore`,
- expose generic rclone arguments, RC, mount, sync, or bisync,
- enable continuous/two-way synchronization.

Those broader synchronization semantics remain separate work, with conflict-aware two-way operation planned for Phase 6.
