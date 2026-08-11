# AtrisBridge

AtrisBridge is a local-first desktop application for keeping engineering and software project workspaces portable, inspectable, and ready for safe synchronization across machines.

> **Status:** early alpha (`0.1.0-alpha.6`). Local inventory, durable SQLite state, restricted Google Drive transport, guarded backup/restore, conflict-aware two-way synchronization, OS-backed credential persistence, and optional client-side content encryption are implemented.

[Türkçe README](README_TR.md)

## Why AtrisBridge?

Moving active projects between development machines often turns into manual ZIP archives, ad-hoc cloud folders, stale copies, and uncertainty about which version is current. AtrisBridge puts a conservative synchronization layer between a local project and a storage provider:

- local-first workspace metadata and scanning,
- durable SQLite file state across restarts,
- BLAKE3 local fingerprints,
- explicit local / remote / synchronized-baseline evidence,
- conflict-aware two-way plans instead of last-write-wins,
- recoverable deletion propagation,
- OS-native secure credential storage,
- optional client-side content encryption,
- provider-independent architecture with Google Drive as the first transport.

## What works today

Phase 0 through Phase 7 currently provide:

- Tauri 2 + React + TypeScript desktop shell,
- local workspace management and native directory picker,
- Rust scanner with BLAKE3 fingerprints,
- built-in exclusions for generated output, Git metadata, IDE caches, `.env*`, common private-key/certificate formats, and AtrisBridge recovery artifacts,
- optional `.atrisbridgeignore`,
- SQLite state under the OS application-data directory,
- persistent local and remote inventory evidence,
- pinned rclone `v1.74.4` runtime validation,
- Google Drive OAuth using `drive.file`,
- OAuth credentials persisted only through the operating-system credential vault,
- workspace-to-Drive-folder bindings,
- guarded local → Drive backup,
- explicit Drive → local restore with staging and rollback,
- conflict-aware reviewed two-way synchronization,
- exact-ID Google Drive Trash for reviewed deletion propagation,
- app-data recovery copies before reviewed local deletion propagation,
- explicit recovery-copy restore,
- optional per-workspace client-side **content encryption** using the restricted crypt transport,
- `AB1-...` recovery-key export/import for encrypted workspaces,
- fresh evidence before planning and again before execution,
- Linux CI for frontend and Rust validation.

Removing a workspace removes AtrisBridge coordination metadata and never deletes project files. Phase 7 also deliberately does **not** automatically destroy the encrypted workspace recovery key from the OS credential vault when workspace metadata is removed; silent key destruction could make remote ciphertext unrecoverable.

## Synchronization safety

AtrisBridge uses explicit reviewed plans rather than background last-write-wins behavior.

For two-way synchronization:

- local changed / remote unchanged → reviewed upload,
- local unchanged / remote changed → reviewed download,
- both changed → conflict,
- local deleted / remote unchanged → exact reviewed Drive object moves to Trash,
- remote deleted / local unchanged → verified local recovery copy, then reviewed local deletion,
- delete/modify overlap → conflict,
- both absent after a shared baseline → converged deletion acknowledgement.

Modification time is never used as conflict authority. Provider and filesystem state are refreshed before execution and journal completion remains evidence-locked.

## Phase 7 secure credential storage

Google Drive OAuth credentials are no longer intentionally session-only. AtrisBridge persists them through the operating system's secure credential facility and lazily reloads them into the Rust process when needed.

Credentials are not stored in:

- SQLite,
- `rclone.conf`,
- `.env` files,
- synchronized workspaces,
- repository files.

Removing the saved provider credential requires a new Google authorization before cloud operations can resume. Forgetting the provider also removes its AtrisBridge provider metadata; it does not delete Drive data.

## Phase 7 optional client-side encryption

Client-side encryption is opt-in per workspace. It can only be attached before the workspace has an accepted synchronized baseline, and the managed remote root must be empty. Phase 7 deliberately does **not** perform an in-place plaintext-to-ciphertext migration.

When enabled:

- regular file contents are encrypted locally before Drive receives them,
- decrypted bytes are produced locally during restore/sync,
- the encryption master key is represented by an `AB1-...` recovery key,
- the recovery key is stored in the OS credential vault,
- users can explicitly export or import the recovery key,
- local BLAKE3 and remote ciphertext provider evidence stay separate,
- encrypted Drive objects are still addressed by exact provider ID for reviewed Trash operations.

### Metadata limitation

The first encrypted transport intentionally uses rclone crypt with filename encryption disabled. **File contents are encrypted, but filenames and directory structure remain visible to the storage provider.** This preserves AtrisBridge's current exact path, provider-ID, collision, conflict, and deletion evidence model.

A missing/corrupt encrypted namespace or key-verification sentinel is treated as an unsafe provider state, not as a clean empty remote inventory. AtrisBridge fails closed instead of converting that uncertainty into deletion intent.

## Planned roadmap

1. **Phase 0/1 — foundation and local inventory** ✅
2. **Phase 2 — SQLite sync journal and durable file state** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — safe incremental backup** ✅
5. **Phase 5 — safe pull and restore** ✅
6. **Phase 6 — conflict-aware two-way synchronization** ✅
7. **Phase 7 — persistent secure credential storage + optional client-side content encryption** ✅
8. **Phase 8+ — continuous watch mode, tray, additional providers, and release pipeline**

See [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), [docs/security.md](docs/security.md), [docs/rclone-transport.md](docs/rclone-transport.md), [docs/backup-engine.md](docs/backup-engine.md), and [docs/restore-engine.md](docs/restore-engine.md).

## Development

Requirements:

- Node.js LTS
- npm
- Rust stable
- Tauri 2 platform prerequisites for your OS

Prepare the pinned rclone development sidecar:

```bash
npm install
npm run sidecar:prepare
npm run tauri:dev
```

`sidecar:prepare` downloads rclone `v1.74.4` from the official release host, verifies the platform-specific SHA-256 checksum, and places the executable under `src-tauri/binaries/`. The binary is ignored by Git.

Validation:

```bash
npm run build
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo check --manifest-path src-tauri/Cargo.toml
```

## `.atrisbridgeignore`

AtrisBridge supports gitignore-compatible project rules in `.atrisbridgeignore` at the workspace root. Built-in safety exclusions remain active even when the custom file is absent.

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

## Security and project policy

AtrisBridge can reduce accidental leakage and destructive synchronization, but it cannot grant permission to upload proprietary or customer-controlled code to third-party infrastructure. Always follow the policy, DLP, contractual, data-residency, export-control, and authorization requirements that apply to the project being synchronized.

Do not report security vulnerabilities in public issues. See [SECURITY.md](SECURITY.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
