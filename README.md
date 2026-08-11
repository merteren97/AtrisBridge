# AtrisBridge

AtrisBridge is a local-first desktop application for keeping engineering and software project workspaces portable, inspectable, and ready for safe synchronization across machines.

> **Status:** early alpha (`0.1.0-alpha.9`). Local inventory, durable SQLite state, restricted Google Drive transport, guarded backup/restore, conflict-aware two-way synchronization, OS-backed credential persistence, optional client-side content encryption, conservative continuous watch, remembered AtrisHub desktop sessions, system-tray runtime, global sync activity, signed updates, and Windows/Linux release packaging are implemented.

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
- continuous reconciliation that treats filesystem events as hints rather than synchronization truth,
- a tray-resident desktop runtime for unattended watch mode,
- provider-independent architecture with Google Drive as the first transport.

## What works today

Phase 0 through Phase 9 currently provide:

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
- native per-workspace filesystem watch with event debounce/coalescing,
- bounded provider reconciliation for remote-side changes,
- optional automatic application of **safe transfer-only** plans,
- automatic fail-closed handoff to manual review for conflicts, blocked paths, scanner uncertainty, or every deletion action,
- backend ownership guards that prevent manual mutating IPC from racing an active watch loop,
- remembered AtrisHub desktop account sessions with refresh credentials held only in the OS vault,
- system tray with explicit open/hide/quit lifecycle,
- close-to-tray behavior so configured watchers can keep running,
- global Activity Center for active cycles, queued operations, conflicts, and per-workspace watcher state,
- opt-in desktop alerts with in-app fallback,
- signed Tauri updater with preview/stable channel support,
- owner-controlled Windows x64 and Linux x64 package/release workflows,
- reproducible npm/Cargo lockfiles and CI validation.

Removing a workspace removes AtrisBridge coordination metadata and never deletes project files. Recovery keys are deliberately not automatically destroyed from the OS credential vault when workspace metadata is removed; silent key destruction could make remote ciphertext unrecoverable.

## Synchronization safety

AtrisBridge never uses modification time as conflict authority and does not run background last-write-wins synchronization.

For two-way synchronization:

- local changed / remote unchanged → upload,
- local unchanged / remote changed → download,
- both changed → conflict,
- local deleted / remote unchanged → exact reviewed Drive object moves to Trash,
- remote deleted / local unchanged → verified local recovery copy, then reviewed local deletion,
- delete/modify overlap → conflict,
- both absent after a shared baseline → converged deletion acknowledgement.

Provider and filesystem state are refreshed before execution and journal completion remains evidence-locked.

## Continuous watch and desktop runtime

Continuous watch mode reduces repeated manual scanning without bypassing AtrisBridge's planner/executor.

- native filesystem events are only **dirty signals**,
- local bursts settle behind a debounce/coalescing window,
- every cycle performs a full scanner pass and fresh provider observation,
- bounded Drive reconciliation detects changes made by another machine while local files are quiet,
- only one automatic cycle may own a workspace at a time,
- `Auto-apply safe transfers` is a separate opt-in and defaults off,
- conflicts, blocked paths, incomplete scanner evidence, encryption/provider uncertainty, and every deletion action fail closed,
- watch mode **never automatically applies deletion actions**,
- manual mutating commands are rejected by the Rust IPC boundary while watch mode owns the workspace.

On desktop, closing the main window hides AtrisBridge to the system tray instead of stopping configured watchers. The tray exposes explicit Open, Hide, and Quit actions. The Activity Center observes the same durable journal/runtime state and does not create a second synchronization authority or bypass review gates.

See [docs/continuous-watch.md](docs/continuous-watch.md) and [docs/desktop-runtime.md](docs/desktop-runtime.md).

## Secure credentials and AtrisHub account

Provider credentials, encryption secrets, and remembered AtrisHub refresh credentials stay out of React, SQLite, `.env`, synchronized workspaces, and repository files whenever they are secret material. OS-backed secure storage is used for persisted credentials.

AtrisHub sign-in remains optional: local AtrisBridge workflows continue to function without an account. Remembered sessions use rotating refresh credentials, while short-lived access credentials remain process-local.

See [docs/security.md](docs/security.md) and [docs/atrishub-account.md](docs/atrishub-account.md).

## Optional client-side encryption

Client-side encryption is opt-in per workspace and is only attached before an accepted synchronized baseline exists and while the managed remote root is empty. AtrisBridge deliberately does not perform an in-place plaintext-to-ciphertext migration.

Regular file contents are encrypted locally before Drive receives them. The first encrypted transport intentionally leaves filename encryption disabled, so **file contents are encrypted while filenames and directory structure remain visible to the storage provider**. Missing/corrupt encrypted namespace or key-verification evidence is treated as unsafe provider state and fails closed.

## Release and updater

The release foundation packages Windows x64 NSIS/MSI and Linux x64 AppImage/DEB artifacts. rclone is pinned and SHA-256 verified before packaging rather than committed as a binary. The Tauri updater uses signed updater artifacts and AtrisHub channel policy while package bytes remain on GitHub Releases.

See [docs/release-updater.md](docs/release-updater.md).

## Roadmap

1. **Phase 0/1 — foundation and local inventory** ✅
2. **Phase 2 — SQLite sync journal and durable file state** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — safe incremental backup** ✅
5. **Phase 5 — safe pull and restore** ✅
6. **Phase 6 — conflict-aware two-way synchronization** ✅
7. **Phase 7 — persistent secure credential storage + optional client-side content encryption** ✅
8. **Phase 8 — continuous watch mode + conservative scheduler** ✅
9. **Phase 9 — tray lifecycle, activity/progress UX, alerts, AtrisHub desktop session, signed Windows/Linux release foundation** ✅
10. **Phase 10+ — additional storage providers, broader platform packaging, and later product integrations**

Architecture and subsystem details live under [`docs/`](docs/architecture.md).

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

Validation:

```bash
npm run build
npm run test:release-contract
cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib
cargo check --locked --manifest-path src-tauri/Cargo.toml
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
