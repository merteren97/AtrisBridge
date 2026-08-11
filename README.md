# AtrisBridge

AtrisBridge is a local-first desktop application for keeping engineering and software project workspaces portable, inspectable, and ready for safe synchronization across machines.

> **Status:** early alpha (`0.1.0-alpha.3`). Local inventory, durable SQLite state, restricted Google Drive observation, and the first guarded local-to-cloud backup path are implemented. Pull, remote deletion, and two-way synchronization remain disabled.

[Türkçe README](README_TR.md)

## Why AtrisBridge?

Moving active projects between development machines often turns into manual ZIP archives, ad-hoc cloud folders, stale copies, and uncertainty about which version is current. AtrisBridge is designed to provide a safer layer between a local project and a storage provider:

- local-first workspace metadata and scanning,
- durable SQLite file state across application restarts,
- conservative source-code and secret ignore rules,
- explicit `.atrisbridgeignore` support,
- BLAKE3 content fingerprints,
- no symlink traversal during inventory scans,
- evidence-first conflict and tombstone handling,
- provider-independent architecture with Google Drive as the first transport.

## What works today

Phase 0 through Phase 4 provide the local state, cloud observation, and first guarded write path:

- Tauri 2 + React + TypeScript desktop shell,
- local workspace management and native directory picker,
- Rust scanner with BLAKE3 fingerprints,
- built-in exclusions for generated output, Git metadata, IDE caches, `.env*`, and common key/certificate formats,
- optional `.atrisbridgeignore`,
- SQLite state under the OS application-data directory,
- persistent scan history and complete file inventory,
- local / remote / last-synchronized evidence,
- restart-safe tombstones and file states,
- exact pinned rclone `v1.74.4` runtime validation,
- Google Drive OAuth with `drive.file`,
- OAuth token held in process memory only,
- workspace-to-Drive-folder bindings,
- remote inventory with provider IDs and checksums kept separate from BLAKE3,
- SQLite schema v3 backup plans,
- fresh local and remote observation before planning and again before execution,
- per-file `create`, `update`, or `blocked` decisions,
- targeted remote ID/checksum preflight for updates,
- local BLAKE3 + MD5 evidence around the transfer,
- post-upload remote size + MD5 verification before accepting a baseline,
- interrupted-backup recovery on application startup,
- two-step desktop flow: **Prepare plan → review → Run backup**,
- Linux CI for frontend and Rust validation.

Removing a workspace deletes only AtrisBridge metadata, never the project directory. Forgetting a provider connection removes only local provider metadata and the in-memory session; it does not delete Drive data.

## Phase 4 backup safety

Phase 4 is deliberately **local → Google Drive only**. Download, remote delete, move, purge, bisync, mount, serve, rclone RC, and arbitrary rclone execution are not exposed.

Preparing a plan refreshes local and remote evidence but uploads nothing. Real writes are restricted to an `AtrisBridge/...` managed remote root. Execution refreshes both inventories again and revalidates every planned item before sending bytes.

New objects use a targeted existence check plus single-file `copyto --immutable`. Existing objects are updated only when the current remote ID/checksum still matches the known AtrisBridge baseline and plan evidence. After the transfer, AtrisBridge verifies that the local file did not change while being read and requires the resulting Drive size and MD5 to match the local evidence before the SQLite baseline is committed.

The current rclone adapter does not claim atomic compare-and-swap semantics for an existing Drive object. A small provider-side race can remain between the final targeted preflight and the write. That limitation is explicit; a future direct provider adapter can use provider-native conditional requests if a suitable precondition is available.

Local deletions never become remote deletes in Phase 4. Remote-only files, duplicate remote paths, conflicts, and unverified local/remote overlaps remain untouched and are blocked rather than overwritten.

See [docs/backup-engine.md](docs/backup-engine.md) and [docs/rclone-transport.md](docs/rclone-transport.md) for the detailed decision and trust boundaries.

## Planned roadmap

1. **Phase 0/1 — foundation and local inventory** ✅
2. **Phase 2 — SQLite sync journal and durable file state** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — safe incremental backup** ✅
5. **Phase 5 — pull and restore**
6. **Phase 6 — conflict-aware two-way synchronization**
7. **Phase 7 — persistent secure credential storage + optional client-side encryption**
8. **Phase 8+ — continuous watch mode, tray, additional providers, and release pipeline**

See [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), [docs/security.md](docs/security.md), [docs/rclone-transport.md](docs/rclone-transport.md), and [docs/backup-engine.md](docs/backup-engine.md).

## Development

### Requirements

- Node.js LTS
- npm
- Rust stable
- Tauri 2 platform prerequisites for your OS

### Prepare the pinned rclone development sidecar

AtrisBridge does not execute an arbitrary `rclone` from the system `PATH`.

```bash
npm install
npm run sidecar:prepare
npm run tauri:dev
```

`sidecar:prepare` downloads rclone `v1.74.4` from the official release host, verifies the platform-specific SHA-256 checksum, and places the executable under `src-tauri/binaries/`. The binary is ignored by Git.

Frontend validation:

```bash
npm run build
```

Rust validation:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo check --manifest-path src-tauri/Cargo.toml
```

## Google Drive behavior

Google Drive uses browser-based OAuth with `drive.file`, avoiding broad visibility into unrelated Drive content. AtrisBridge backup writes are additionally restricted to an `AtrisBridge/...` managed path.

The OAuth token is held only in process memory. Restarting AtrisBridge requires reconnecting the provider until a dedicated secure credential layer is introduced; plaintext persistent tokens are not used as an interim shortcut.

Remote provider checksums such as MD5 are stored as provider evidence and never treated as BLAKE3. A synchronized baseline stores local BLAKE3 and remote provider evidence separately after a verified transfer.

## `.atrisbridgeignore`

AtrisBridge supports gitignore-compatible project rules in `.atrisbridgeignore` at the workspace root. Built-in safety rules remain active even if the custom file is absent.

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

Built-in rules intentionally exclude `.git`, common generated folders, `.env*`, and common private-key/certificate file extensions.

## Durable sync journal

AtrisBridge stores state in `atrisbridge.db` under the OS application-data directory. SQLite uses foreign keys, WAL journaling, and a bounded busy timeout. Local and remote observations are recorded separately so transport code does not guess from modification times.

If the application exits during a running upload, startup recovery retires the interrupted operation as failed/partial. It is not silently retried or marked synchronized; the next attempt requires fresh evidence and a new plan.

## Security and project policy

AtrisBridge can reduce accidental leakage, but it cannot grant permission to upload proprietary or customer-controlled code to third-party infrastructure. Always follow the policy, DLP, contractual, data-residency, export-control, and authorization requirements that apply to the project you are synchronizing.

Do not report security vulnerabilities in public issues. See [SECURITY.md](SECURITY.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
