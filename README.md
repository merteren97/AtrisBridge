# AtrisBridge

AtrisBridge is a local-first desktop application for keeping engineering and software project workspaces portable, inspectable, and ready for safe synchronization across machines.

> **Status:** early alpha (`0.1.0-alpha.1`). Local workspace inventory and the durable SQLite sync journal are implemented. Cloud transport and two-way synchronization are intentionally not enabled yet.

[Türkçe README](README_TR.md)

## Why AtrisBridge?

Moving active projects between development machines often turns into manual ZIP archives, ad-hoc cloud folders, stale copies, and uncertainty about which version is current. AtrisBridge is designed to provide a safer layer between a local project and a storage provider:

- local-first workspace metadata and scanning,
- durable SQLite file state across application restarts,
- conservative source-code and secret ignore rules,
- explicit `.atrisbridgeignore` support,
- content fingerprints using BLAKE3,
- no symlink traversal during inventory scans,
- conflict and tombstone state designed before destructive synchronization is enabled,
- provider-independent architecture with Google Drive planned as the first transport.

## What works today

Phase 0 through Phase 2 establish the security, workspace, and durable state foundation:

- Tauri 2 + React + TypeScript desktop shell,
- add and remove local workspaces,
- native directory picker,
- Rust workspace scanner,
- BLAKE3 file fingerprints,
- built-in exclusions for generated output, Git metadata, IDE caches, environment files, and common key/certificate formats,
- optional `.atrisbridgeignore` creation,
- SQLite database stored under the OS application-data directory,
- automatic one-time import of the earlier `workspaces.json` metadata,
- persistent scan history and complete file inventory,
- local/remote/last-synced hash fields prepared for provider reconciliation,
- restart-safe file states such as `local_only`, `local_modified`, `local_deleted`, `remote_modified`, and `conflict`,
- recoverable tombstone state for previously synchronized files,
- a durable pending-operation table for future upload/download planning,
- UI metrics sourced from the durable journal instead of the current process session,
- Linux CI for frontend and Rust validation.

Removing a workspace from AtrisBridge deletes only AtrisBridge metadata for that workspace. It never deletes the project directory.

## Planned roadmap

1. **Phase 0/1 — foundation and local inventory** ✅
2. **Phase 2 — SQLite sync journal and durable file state** ✅
3. **Phase 3 — rclone sidecar and Google Drive provider**
4. **Phase 4 — safe incremental backup**
5. **Phase 5 — pull and restore**
6. **Phase 6 — conflict-aware two-way synchronization**
7. **Phase 7 — client-side encryption and secure credential storage**
8. **Phase 8+ — continuous watch mode, tray, additional providers and release pipeline**

See [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), and [docs/security.md](docs/security.md).

## Development

### Requirements

- Node.js LTS
- npm
- Rust stable
- Tauri 2 platform prerequisites for your OS

### Run

```bash
npm install
npm run tauri:dev
```

Frontend-only validation:

```bash
npm run build
```

Rust validation:

```bash
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo check --manifest-path src-tauri/Cargo.toml
```

## `.atrisbridgeignore`

AtrisBridge supports gitignore-compatible project rules in a file named `.atrisbridgeignore` at the workspace root. Built-in safety rules remain active even if the custom file is absent.

Example:

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

Built-in safety rules intentionally exclude `.git`, common generated folders, `.env*`, and common private-key/certificate file extensions. These defaults will become configurable only when doing so cannot silently weaken a workspace's safety posture.

## Durable sync journal

AtrisBridge stores application state in `atrisbridge.db` under the operating system's application-data directory. SQLite is configured with foreign keys, WAL journaling, and a bounded busy timeout. The journal records complete scan inventory and retains separate local, remote, and last-synchronized observations so later transport phases do not need to guess from modification time alone.

A local file disappearing does not automatically authorize a remote delete. Only files with a known synchronized baseline can become tombstones, and future transport code must still verify remote state before moving data to provider trash.

## Security and project policy

AtrisBridge is designed to reduce accidental leakage; it cannot grant permission to upload proprietary or customer-controlled code to third-party infrastructure. Always follow the policy and authorization requirements that apply to the project you are synchronizing.

Do not report security vulnerabilities in public issues. See [SECURITY.md](SECURITY.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
