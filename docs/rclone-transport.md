# rclone and Google Drive transport boundary

AtrisBridge owns synchronization policy. rclone and Google Drive are provider transports/control-plane adapters; neither decides which copy wins, whether a conflict may be overwritten, or whether a deletion is safe to propagate.

## Runtime resolution

AtrisBridge never searches the system `PATH` for rclone.

- Development: `src-tauri/binaries/rclone(.exe)` prepared by `npm run sidecar:prepare`.
- Packaged application: the verified executable is expected under the application resource directory at `rclone/rclone(.exe)`.

The runtime accepts exactly rclone `v1.74.4`. The preparation script downloads the pinned official archive and verifies the platform-specific SHA-256 digest before copying the executable.

## Google Drive authorization

Google Drive uses browser authorization with the `drive.file` scope. The OAuth JSON token is retained only in the Tauri-managed in-memory `ProviderSessionStore`.

The token is not written to SQLite, `rclone.conf`, `.env`, repository files, or provider metadata. Restarting AtrisBridge intentionally drops the session until the Phase 7 secure credential layer exists.

Phase 6 exact-ID Trash uses the current access token extracted from that same in-memory OAuth JSON for one narrow Drive API request. It does not add a second persistent credential store. If the current access token is rejected or expired, the Trash operation fails and the provider can be reconnected instead of persisting plaintext refresh credentials as a shortcut.

## rclone invocation model

rclone is launched directly with `std::process::Command`; there is no shell interpolation. Inherited rclone credential/config environment variables are removed before invocation and only the active in-memory provider session is supplied to the child process.

There is no frontend command accepting arbitrary rclone arguments. The adapters expose narrow operations for:

- `rclone version`,
- `rclone authorize drive`,
- `rclone config userinfo`,
- `rclone about`,
- `rclone lsjson` for inventory and targeted stat,
- local `rclone hashsum MD5` for transfer evidence,
- single-file `rclone copyto` for approved uploads,
- single-file `rclone copyto` into AtrisBridge-owned local staging for approved downloads.

`sync`, `bisync`, `purge`, `move`, `mount`, `serve`, remote control, and generic arbitrary command execution remain unavailable.

## Remote inventory

The adapter queries only the bound managed workspace path rather than using a provider-wide listing as synchronization authority.

Remote observations retain provider ID, size, modification metadata, checksum type, and checksum separately from local BLAKE3. Native Google Docs are skipped by the regular-file adapter because they do not supply the same regular-file checksum semantics.

Duplicate relative paths or case-insensitive path collisions are blocked instead of deduplicated destructively.

## Phase 4 upload boundary

Remote writes are restricted to an `AtrisBridge/...` workspace root. Backup and Phase 6 upload actions capture local BLAKE3/size plus expected remote evidence before an upload is approved.

New-file operations verify remote absence and use single-file `copyto --immutable`. Existing-file updates require the observed remote object to continue matching the known baseline/plan evidence. After transfer AtrisBridge verifies local stability and resulting provider size/checksum before accepting the new SQLite baseline.

rclone does not provide AtrisBridge with provider-native atomic compare-and-swap for an existing Drive object. There remains a provider-side window between targeted preflight and the write. AtrisBridge documents that boundary instead of treating a successful process exit as proof that no concurrent provider edit occurred.

## Phase 5/6 download boundary

rclone is never given an existing final project-file destination for a reviewed download. Every Drive → local transfer first targets a unique hidden sibling staging file.

AtrisBridge then:

1. requires planned remote ID/size/MD5 evidence,
2. resolves a portable local path without symlink traversal,
3. downloads to a new `.part` path,
4. verifies staging BLAKE3, size, and MD5,
5. performs final remote validation,
6. revalidates the local destination,
7. records local apply/recovery state,
8. applies the file through recoverable filesystem renames,
9. accepts the synchronized baseline only after final local verification and an evidence-locked SQLite update.

For an existing target, a `.bak` is retained until journal completion.

## Phase 6 exact-ID Trash control plane

Phase 6 does **not** use a path-based rclone deletion command for reviewed local-deletion propagation.

After fresh inventory and targeted remote ID/size/MD5 preflight, AtrisBridge sends one narrow Google Drive `files.update` request for the **exact reviewed file ID** with `trashed=true`. The request is restricted to the current OAuth session and is not exposed as a generic Drive API surface to the frontend.

Before that provider mutation, AtrisBridge also checks that the local path remains absent. After Trash, it checks the managed remote path again. If another object now occupies the same path, that object is preserved and the old deletion is not silently treated as full convergence.

Permanent Drive deletion is not implemented in Phase 6. Trash remains provider-recoverable.

### Remaining race boundary

Exact-ID addressing prevents deleting a different object merely because it reused the same path, but it does not create an atomic checksum precondition on the reviewed object. The same Drive file ID could still be modified by another client after AtrisBridge's final checksum preflight and before the Trash request.

AtrisBridge therefore does not claim atomic distributed deletion. It relies on explicit review, repeat preflight, exact ID, provider Trash instead of permanent delete, postflight observation, and a fresh plan after uncertain outcomes.

## Local deletion recovery is outside rclone

When a remote deletion propagates locally, rclone never deletes the local workspace file. The Rust sync engine owns that mutation:

1. targeted Drive checks prove the remote path remains absent,
2. current local BLAKE3 + size must still equal baseline,
3. a recovery copy is written under AtrisBridge app-data,
4. the recovery copy is fingerprinted and flushed,
5. remote absence is checked again,
6. an `applying` marker is persisted,
7. the local source is re-fingerprinted and remote absence checked again,
8. only then is the local file removed,
9. recovery metadata and deletion convergence are committed transactionally.

The recovery copy can later be restored locally through a separate user action; that action does not touch Drive.

## Capability summary

| Capability | Phase 6 status |
| --- | --- |
| provider inventory / targeted stat | enabled, constrained |
| single-file upload | enabled after reviewed plan |
| staged single-file download | enabled after reviewed plan |
| Drive Trash | enabled only by exact reviewed file ID |
| permanent remote delete | disabled |
| arbitrary rclone command surface | disabled |
| rclone `sync` / `bisync` | disabled |
| mount / serve / RC | disabled |
| automatic conflict resolution | disabled |
| continuous automatic sync | disabled |
