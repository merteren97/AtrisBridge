# rclone transport boundary

Phase 3 introduces rclone as a transport implementation without making rclone the synchronization authority.

## Trust boundary

AtrisBridge owns:

- workspace selection and policy,
- local BLAKE3 inventory,
- durable sync state,
- remote observations,
- conflict/tombstone decisions,
- future operation planning and approval.

rclone is limited to provider transport and provider metadata queries. Phase 3 does not expose copy, sync, move, delete, purge, cleanup, bisync, mount, serve, or remote-control commands.

## Runtime resolution

AtrisBridge never searches the system `PATH` for rclone.

- Development: `src-tauri/binaries/rclone(.exe)` prepared by `npm run sidecar:prepare`.
- Packaged application: a future release step places the verified executable under the application resource directory at `rclone/rclone(.exe)`.

The runtime accepts only the pinned rclone `v1.74.4` runtime. The preparation script pins the official `v1.74.4` archives and verifies SHA-256 before copying the executable.

## Google Drive authorization

Phase 3 uses rclone's browser authorization flow with the `drive.file` scope. The OAuth JSON token is parsed from the successful authorization response and retained only in `ProviderSessionStore`, an in-memory Rust structure managed by Tauri.

The token is **not** written to:

- SQLite,
- `rclone.conf`,
- environment files,
- logs,
- PR-visible configuration.

Provider metadata stored in SQLite contains only provider ID/type, display/account label, timestamps, workspace mapping, and remote inventory observations. Restarting the application intentionally drops the OAuth session and requires reconnecting.

## Invocation model

rclone is launched directly with `std::process::Command`; there is no shell interpolation. AtrisBridge removes inherited rclone credential/config environment variables before each invocation and supplies the session token only to the child environment as `RCLONE_DRIVE_TOKEN`.

The phase-3 allowlist is implemented as dedicated functions rather than a generic "run rclone arguments" command:

- `rclone version`
- `rclone authorize drive`
- `rclone config userinfo :drive:`
- `rclone about :drive:`
- `rclone lsjson :drive:`

No frontend command accepts arbitrary executable names or arbitrary rclone argument arrays.

## Remote inventory

Remote files are listed from the provider root and filtered in Rust to the workspace's dedicated remote prefix. This means a not-yet-created workspace folder safely appears as an empty inventory instead of requiring Phase 3 to create remote directories.

Google Drive's provider checksum (normally MD5 for ordinary project files) is stored as `remote_checksum` with its checksum type. It is deliberately separate from AtrisBridge's BLAKE3 fields. Two different hash algorithms are never treated as equal content evidence.

If the same relative path exists locally and remotely before AtrisBridge has a known synchronized baseline, the entry becomes a conflict. Phase 3 never guesses that the files are identical from timestamps or sizes.

## Why transfers stay disabled

A provider connection proving that AtrisBridge can observe Drive is not enough to authorize writes. Phase 4 will add an explicit operation planner that converts durable file state into upload operations, performs preflight checks, records operation state transactionally, and verifies the remote result before establishing a synchronized baseline.
