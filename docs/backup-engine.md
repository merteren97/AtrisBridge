# Safe Incremental Backup Engine

Phase 4 introduces the first remote write path in AtrisBridge. It is intentionally narrower than synchronization: only explicit, reviewed, local-to-Google-Drive backup is supported.

## Goals

- Upload new local files without overwriting an unexpected remote object.
- Update a previously synchronized remote file only when its observed evidence still matches the AtrisBridge baseline.
- Keep local BLAKE3 evidence separate from provider-native checksums.
- Never translate a local deletion into a remote deletion.
- Persist backup decisions before executing remote writes.
- Revalidate evidence at execution time instead of trusting a stale plan.
- Accept a synchronized baseline only after the uploaded content is verified remotely.

## Two-step workflow

### 1. Prepare plan

`prepare_backup_plan` performs a fresh local scan and fresh remote inventory before storing a plan in SQLite schema v3. Preparing a plan does **not** upload anything.

Each candidate records:

- relative path,
- local BLAKE3 and size,
- expected remote presence,
- expected remote object ID,
- expected remote checksum type/value,
- decision: `create`, `update`, or `blocked`.

Only backup-mode workspaces bound under the managed `AtrisBridge/...` remote namespace may prepare a Phase 4 write plan.

### 2. Run backup

`execute_backup_plan` accepts only a plan still marked `ready`. Before the plan enters `running`, AtrisBridge refreshes both inventories again and verifies that the workspace/provider binding has not changed.

Each upload item is then processed independently:

1. Compare current SQLite evidence with the persisted plan snapshot.
2. Canonicalize the local path and ensure it remains inside the selected workspace root.
3. Recompute local BLAKE3 and size.
4. For updates, target-stat the remote object and compare ID/checksum with the plan evidence.
5. Compute local MD5 evidence and recheck BLAKE3 before sending bytes.
6. Execute the restricted single-file upload primitive.
7. Recompute local BLAKE3 after the transfer to detect a file that changed while being read.
8. Read the resulting Drive object and require its size and MD5 to match the local evidence.
9. Commit the remote ID/checksum and local BLAKE3 as the synchronized baseline in one SQLite transaction.

If an upload cannot be verified, its baseline is not accepted. Safe independent items can complete while blocked or failed paths remain untouched.

## Planner decisions

| Local evidence | Remote evidence | Baseline | Phase 4 decision |
| --- | --- | --- | --- |
| Present | Absent | None | `create` |
| Present | Present | None | `blocked` — unverified overlap |
| Changed | Matches baseline | Present | `update` |
| Matches baseline | Changed/missing | Present | `blocked` — remote divergence |
| Changed | Changed/missing | Present | `blocked` — conflict |
| Deleted | Present | Any | preserve remote; no delete |
| Absent | Remote-only | None | preserve remote; no download |

Timestamp proximity and equal file size never establish synchronization by themselves.

## rclone boundary

AtrisBridge owns the plan and evidence. The pinned rclone sidecar is only the transport implementation.

Phase 4 adds only the primitives required for an approved single-file backup:

- `lsjson` / `lsjson --stat` for provider evidence,
- local `hashsum MD5` for upload verification evidence,
- `copyto` for one approved file.

There is no generic frontend rclone command. `sync`, `bisync`, `delete`, `purge`, `move`, `mount`, `serve`, and RC remain unavailable.

New-file writes use an immediate targeted existence check plus `--immutable`. Remote duplicate paths detected during inventory are blocked instead of automatically deduplicated.

## Interrupted execution

If AtrisBridge exits while a plan/item is `running`, startup recovery retires the interrupted item as `failed` and the plan as `partial`. It is not silently retried or marked synchronized. The next backup requires fresh local/remote observation and a new plan.

This is important because a provider can accept a write even when the client process later loses the response. During a normal execution, AtrisBridge handles the same ambiguity by checking the remote size and MD5: exact verified content can be accepted without blindly retrying the transfer.

## Concurrency boundary

The current rclone adapter does **not** provide atomic compare-and-swap semantics for an existing Drive object. There is a small provider-side race window between the final targeted preflight and the update write.

AtrisBridge reduces this risk with:

- fresh full local/remote inventories before execution,
- planned remote ID/checksum comparison,
- targeted remote stat immediately before update,
- one transfer attempt rather than automatic write retries,
- post-upload local BLAKE3 recheck,
- post-upload remote size + MD5 verification.

A future direct provider adapter can close this window if the provider exposes a suitable conditional-write precondition.

## Deletion policy

Phase 4 never sends a remote delete. Local deletions remain tombstones/evidence for later policy decisions. Remote-only files are also preserved. Backup mode never downloads them and never overwrites an unverified overlap.

## Credential policy

OAuth tokens remain in process memory. Backup plans, plan items, and provider metadata contain no OAuth token or client-secret fields. Tokens are not written to SQLite, `rclone.conf`, `.env`, or repository files.

## Organizational policy

AtrisBridge only transports files the user is authorized to move. It does not grant permission to copy company, customer, regulated, export-controlled, or otherwise restricted material to a cloud provider. Organization security, DLP, contractual, and data-residency requirements remain authoritative.
