# rclone transport boundary

AtrisBridge uses rclone as a narrow provider transport adapter. rclone is **not** the synchronization authority: AtrisBridge owns workspace policy, BLAKE3 inventory, durable state, planning, conflict decisions, and baseline acceptance.

## Runtime resolution

AtrisBridge never searches the system `PATH` for rclone.

- Development: `src-tauri/binaries/rclone(.exe)` prepared by `npm run sidecar:prepare`.
- Packaged application: the verified executable is expected under the application resource directory at `rclone/rclone(.exe)`.

The runtime accepts exactly rclone `v1.74.4`. The preparation script downloads the pinned official archive and verifies the platform-specific SHA-256 digest before copying the executable.

## Google Drive authorization

Google Drive uses the browser authorization flow with the `drive.file` scope. The OAuth JSON token is retained only in the Tauri-managed in-memory `ProviderSessionStore`.

The token is not written to SQLite, `rclone.conf`, `.env` files, repository files, or provider metadata. Restarting AtrisBridge intentionally drops the session and requires reconnecting until a dedicated secure credential layer is introduced.

## Invocation model

rclone is launched directly with `std::process::Command`; there is no shell interpolation. AtrisBridge removes inherited rclone credential/config environment variables before every invocation and supplies the session token only to the child process.

There is no frontend command that accepts arbitrary rclone arguments. The Phase 4 adapter exposes only dedicated functions for:

- `rclone version`
- `rclone authorize drive`
- `rclone config userinfo`
- `rclone about`
- `rclone lsjson` for inventory and targeted stat
- local `rclone hashsum MD5` for upload evidence
- single-file `rclone copyto` for an approved backup item

`sync`, `bisync`, `delete`, `purge`, `move`, `mount`, `serve`, remote control, and generic command execution remain unavailable.

## Remote inventory

The adapter queries the workspace's bound Google Drive folder directly instead of listing all visible Drive content and filtering it locally. A provider "directory not found" result is treated as an empty workspace inventory so the first approved backup can create its managed path.

Google Drive checksums are recorded separately from local BLAKE3 fingerprints. They are different evidence types and are never directly compared as if they used the same algorithm.

Duplicate relative paths returned by Google Drive are treated as unsafe. AtrisBridge blocks planning instead of automatically running a destructive deduplication command.

## Phase 4 write boundary

Remote writes are restricted to an `AtrisBridge/...` workspace root. A backup plan captures local BLAKE3/size and expected remote ID/checksum before any upload is allowed.

For each approved file AtrisBridge:

1. revalidates the plan against fresh local and remote inventories,
2. performs a targeted remote stat before an update,
3. computes local BLAKE3 and MD5 evidence,
4. executes one `copyto` with retries limited to one,
5. recomputes local BLAKE3 after the transfer,
6. reads the resulting Drive object,
7. requires remote size and MD5 to match the local evidence,
8. only then commits the synchronized baseline in SQLite.

New-file operations also perform a targeted existence check immediately before the write and use `--immutable` as an additional refusal-to-overwrite guard.

A transport process can report an error after the provider has already accepted a file. In that case AtrisBridge does not blindly retry: if the exact remote size and MD5 match the local evidence, the verified remote observation can be accepted for that operation. This reduces duplicate-object risk on Google Drive.

## Concurrency limitation

The Phase 4 rclone adapter does not claim atomic compare-and-swap semantics for updates to an existing Drive object. There remains a small provider-side race window between the final targeted preflight and the write. AtrisBridge mitigates it with fresh inventories, remote ID/checksum comparison, a targeted stat, a single transfer attempt, and post-upload content verification. A future direct provider adapter can use provider-native conditional writes if an appropriate precondition is available.

## No deletion semantics

Phase 4 never maps a missing local file to a remote delete. Remote-only files, local tombstones, unverified overlaps, and conflicts are preserved as evidence and are not modified by the backup executor.
