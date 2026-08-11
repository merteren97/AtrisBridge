# AtrisBridge

AtrisBridge is a local-first desktop application for keeping engineering and software project workspaces portable, inspectable, and ready for safe synchronization across machines.

> **Status:** early alpha (`0.1.0-alpha.2`). Local inventory, the durable SQLite sync journal, and the first Google Drive transport-observation layer are implemented. File transfer and destructive synchronization remain disabled.

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
- provider-independent transport architecture with Google Drive as the first provider.

## What works today

Phase 0 through Phase 3 establish the local state and cloud-observation foundation:

- Tauri 2 + React + TypeScript desktop shell,
- add and remove local workspaces,
- native directory picker,
- Rust workspace scanner and BLAKE3 file fingerprints,
- built-in exclusions for generated output, Git metadata, IDE caches, environment files, and common key/certificate formats,
- optional `.atrisbridgeignore` creation,
- SQLite database stored under the OS application-data directory,
- automatic one-time import of the earlier `workspaces.json` metadata,
- persistent scan history and complete file inventory,
- local/remote/last-synced observations prepared for provider reconciliation,
- restart-safe file states and recoverable tombstones,
- durable pending-operation schema for later transfers,
- pinned rclone runtime validation (exact `v1.74.4`),
- Google Drive OAuth using the least-privilege `drive.file` scope,
- OAuth token kept in process memory only in Phase 3 — no token is written to SQLite or `rclone.conf`,
- explicit workspace-to-Drive-folder bindings,
- read-only remote inventory via `rclone lsjson`,
- Google Drive IDs, timestamps and provider checksums recorded separately from AtrisBridge BLAKE3 hashes,
- safe conflict classification when local and remote content overlap without a known synchronized baseline,
- Linux CI for frontend and Rust validation.

Removing a workspace from AtrisBridge deletes only AtrisBridge metadata for that workspace. It never deletes the project directory. Forgetting a provider connection removes only local provider metadata and the in-memory session; it does not delete anything from Google Drive.

## Planned roadmap

1. **Phase 0/1 — foundation and local inventory** ✅
2. **Phase 2 — SQLite sync journal and durable file state** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — safe incremental backup**
5. **Phase 5 — pull and restore**
6. **Phase 6 — conflict-aware two-way synchronization**
7. **Phase 7 — persistent secure credential storage + optional client-side encryption**
8. **Phase 8+ — continuous watch mode, tray, additional providers and release pipeline**

See [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), [docs/security.md](docs/security.md), and [docs/rclone-transport.md](docs/rclone-transport.md).

## Development

### Requirements

- Node.js LTS
- npm
- Rust stable
- Tauri 2 platform prerequisites for your OS

### Prepare the pinned rclone development sidecar

AtrisBridge intentionally does not execute an arbitrary `rclone` from the system `PATH`. Prepare the verified local development binary first:

```bash
npm run sidecar:prepare
```

The script downloads rclone `v1.74.4` from the official release host, verifies the platform-specific SHA-256 checksum, and places the executable under `src-tauri/binaries/`. The binary is ignored by Git.

### Run

```bash
npm install
npm run sidecar:prepare
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

## Google Drive behavior in Phase 3

Phase 3 intentionally exposes **observation only**. Connecting Google Drive opens the browser-based rclone OAuth flow and requests `drive.file`, which means AtrisBridge/rclone can access only files and folders created through that OAuth application. This is suitable for the dedicated AtrisBridge storage area and avoids broad visibility into unrelated Drive content.

The OAuth token is held only in process memory. Restarting AtrisBridge therefore requires reconnecting the provider. Durable secret storage is deliberately deferred until a dedicated secure credential layer is introduced; plaintext tokens are not used as an interim shortcut.

Remote inventory uses provider-native checksums such as MD5 as observations. Those values are never compared directly with BLAKE3. AtrisBridge will only establish a cross-provider synchronized content baseline after it performs a verified transfer in Phase 4 or later.

## `.atrisbridgeignore`

AtrisBridge supports gitignore-compatible project rules in a file named `.atrisbridgeignore` at the workspace root. Built-in safety rules remain active even if the custom file is absent.

Example:

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

Built-in safety rules intentionally exclude `.git`, common generated folders, `.env*`, and common private-key/certificate file extensions.

## Durable sync journal

AtrisBridge stores application state in `atrisbridge.db` under the operating system's application-data directory. SQLite is configured with foreign keys, WAL journaling, and a bounded busy timeout. The journal records complete local and remote observations separately so later transport phases do not need to guess from modification time alone.

A local file disappearing does not automatically authorize a remote delete. Only files with a known synchronized baseline can become tombstones, and future transport code must still verify remote state before moving data to provider trash.

## Security and project policy

AtrisBridge is designed to reduce accidental leakage; it cannot grant permission to upload proprietary or customer-controlled code to third-party infrastructure. Always follow the policy and authorization requirements that apply to the project you are synchronizing.

Do not report security vulnerabilities in public issues. See [SECURITY.md](SECURITY.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
