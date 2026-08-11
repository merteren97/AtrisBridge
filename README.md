# AtrisBridge

AtrisBridge is a local-first desktop application for keeping engineering and software project workspaces portable, inspectable, and ready for safe synchronization across machines.

> **Status:** early alpha (`0.1.0-alpha.4`). Local inventory, durable SQLite state, restricted Google Drive observation, guarded backup, and explicit verified restore are implemented. Remote deletion and automatic two-way synchronization remain disabled.

[Türkçe README](README_TR.md)

## Why AtrisBridge?

Moving active projects between development machines often turns into manual ZIP archives, ad-hoc cloud folders, stale copies, and uncertainty about which version is current. AtrisBridge is designed to provide a safer layer between a local project and a storage provider:

- local-first workspace metadata and scanning,
- durable SQLite file state across application restarts,
- conservative source-code and secret ignore rules,
- explicit `.atrisbridgeignore` support,
- BLAKE3 content fingerprints,
- no symlink traversal during inventory or restore path resolution,
- evidence-first conflict and tombstone handling,
- provider-independent architecture with Google Drive as the first transport.

## What works today

Phase 0 through Phase 5 provide the local state, cloud observation, guarded backup, and explicit restore paths:

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
- guarded local → Drive backup planning and execution,
- explicit Drive → local restore planning and execution,
- fresh local and remote observation before planning and again before execution,
- per-file safe action / blocked decisions,
- staged restore downloads with remote MD5 + size verification,
- recoverable local update through a temporary `.bak` copy,
- interrupted-transfer recovery on application startup,
- two-step desktop flows: **Prepare → review → Run**,
- Linux CI for frontend and Rust validation.

Removing a workspace deletes only AtrisBridge metadata, never the project directory. Forgetting a provider connection removes only local provider metadata and the in-memory session; it does not delete Drive data.

## Phase 4 backup safety

Phase 4 is deliberately **local → Google Drive only**. Preparing a plan refreshes local and remote evidence but uploads nothing. Real writes are restricted to an `AtrisBridge/...` managed remote root. Execution refreshes both inventories again and revalidates every planned item before sending bytes.

New objects use a targeted existence check plus single-file `copyto --immutable`. Existing objects are updated only when the current remote ID/checksum still matches the known AtrisBridge baseline and plan evidence. After transfer, AtrisBridge verifies that the local file did not change while being read and requires the resulting Drive size and MD5 to match local evidence before the SQLite baseline is committed.

Local deletions never become remote deletes. Remote-only files, duplicate remote paths, conflicts, and unverified local/remote overlaps remain untouched and are blocked rather than overwritten.

See [docs/backup-engine.md](docs/backup-engine.md).

## Phase 5 restore safety

Phase 5 adds an explicit **Google Drive → local** restore path. It is not an automatic pull loop and it never treats remote absence as permission to delete local content.

A restore plan classifies remote-only files as safe local creates and a remote change as a safe local update only when the existing local file still matches the last synchronized BLAKE3 baseline. Local changes, both-sides changes, unverified overlaps, unsafe paths, missing provider evidence, and case-insensitive filename collisions are blocked for manual review.

rclone never downloads directly onto the final project file. AtrisBridge first downloads into a unique hidden staging file, verifies its size + MD5 against Google Drive, performs a final remote stat, rechecks the local target, and only then applies the content. Existing local files are moved to a recoverable `.bak` before replacement and that recovery copy is removed only after SQLite journal completion succeeds.

If AtrisBridge exits during local apply, startup recovery rolls back only when the downloaded BLAKE3 + size prove that doing so is safe. Uncertain files are preserved rather than overwritten automatically.

Phase 5 restores regular file content; it does not promise portable restoration of Unix executable bits, ACLs, ownership, alternate data streams, or provider-specific filesystem metadata.

See [docs/restore-engine.md](docs/restore-engine.md) and [docs/rclone-transport.md](docs/rclone-transport.md).

## Planned roadmap

1. **Phase 0/1 — foundation and local inventory** ✅
2. **Phase 2 — SQLite sync journal and durable file state** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — safe incremental backup** ✅
5. **Phase 5 — safe pull and restore** ✅
6. **Phase 6 — conflict-aware two-way synchronization**
7. **Phase 7 — persistent secure credential storage + optional client-side encryption**
8. **Phase 8+ — continuous watch mode, tray, additional providers, and release pipeline**

See [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), [docs/security.md](docs/security.md), [docs/rclone-transport.md](docs/rclone-transport.md), [docs/backup-engine.md](docs/backup-engine.md), and [docs/restore-engine.md](docs/restore-engine.md).

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

Google Drive uses browser-based OAuth with `drive.file`, avoiding broad visibility into unrelated Drive content. AtrisBridge backup and restore operations are additionally restricted to an `AtrisBridge/...` managed workspace path.

The OAuth token is held only in process memory. Restarting AtrisBridge requires reconnecting the provider until a dedicated secure credential layer is introduced; plaintext persistent tokens are not used as an interim shortcut.

Remote provider checksums such as MD5 are stored as provider evidence and never treated as BLAKE3. A synchronized baseline stores local BLAKE3 and remote provider evidence separately after a verified transfer.

Native Google Docs are skipped by the current Drive adapter; Phase 4/5 operate on regular file content with provider checksum evidence.

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

The core database remains schema v3 from Phase 4. Phase 5 adds restore-plan tables idempotently without rewriting existing Phase 4 tables. Interrupted backup/restore execution is never silently retried or marked synchronized; a later attempt requires fresh evidence and a new plan.

## Security and project policy

AtrisBridge can reduce accidental leakage, but it cannot grant permission to upload proprietary or customer-controlled code to third-party infrastructure. Always follow the policy, DLP, contractual, data-residency, export-control, and authorization requirements that apply to the project you are synchronizing.

Do not report security vulnerabilities in public issues. See [SECURITY.md](SECURITY.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
