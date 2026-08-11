# AtrisBridge

AtrisBridge is a local-first desktop application for keeping engineering and software project workspaces portable, inspectable, and ready for safe synchronization across machines.

> **Status:** early alpha (`0.1.0-alpha.5`). Local inventory, durable SQLite state, restricted Google Drive transport, guarded backup, verified restore, and explicit conflict-aware two-way synchronization are implemented. Continuous background sync and automatic conflict resolution remain disabled.

[Türkçe README](README_TR.md)

## Why AtrisBridge?

Moving active projects between development machines often turns into manual ZIP archives, ad-hoc cloud folders, stale copies, and uncertainty about which version is current. AtrisBridge puts a conservative coordination layer between a local project and a storage provider:

- local-first workspace metadata and scanning,
- durable SQLite file state across restarts,
- BLAKE3 local fingerprints,
- separate remote provider ID/size/checksum evidence,
- explicit last-synchronized baselines,
- conservative secret/generated-file exclusions and `.atrisbridgeignore`,
- no symlink traversal for synchronized paths,
- review-first backup, restore, and two-way plans,
- recoverable deletion semantics instead of blind permanent deletes.

## What works today

Phase 0 through Phase 6 provide the first complete reviewed synchronization loop:

- Tauri 2 + React + TypeScript desktop shell,
- workspace management and native directory picker,
- Rust scanner with BLAKE3 fingerprints,
- SQLite state under the OS application-data directory,
- persistent local/remote/last-synchronized evidence,
- exact pinned rclone `v1.74.4` runtime validation,
- Google Drive OAuth with `drive.file`, with OAuth session data held in process memory only,
- workspace → managed Drive folder binding,
- guarded local → Drive backup,
- verified Drive → local restore,
- explicit **Two-Way** workspace mode,
- fresh local + remote observations before planning and again before execution,
- baseline-based upload/download decisions,
- modify/modify and delete/modify conflicts surfaced without last-write-wins,
- reviewed local deletion → exact reviewed Google Drive file ID moved to Trash,
- reviewed remote deletion → verified local recovery copy before local removal,
- user-visible recovery copies that can be restored locally without changing Drive,
- live local/remote absence checks immediately around deletion propagation,
- startup recovery for interrupted transfer/apply states,
- two-step desktop flow: **Prepare → review → Run**,
- Linux CI for frontend and Rust validation.

Removing a workspace deletes only AtrisBridge metadata, never the project directory. Forgetting a provider removes local provider metadata and the in-memory session; it does not delete Drive data.

## Phase 6 — conflict-aware two-way synchronization

Two-way behavior must be enabled explicitly per workspace. Enabling the mode starts no transfer by itself. **Prepare sync** refreshes both inventories and persists a reviewable plan; only **Run sync** executes still-valid safe items.

AtrisBridge compares current local and remote evidence with the last successfully synchronized baseline:

| Local | Remote | Baseline interpretation | Decision |
| --- | --- | --- | --- |
| new | absent | no baseline | upload create |
| absent | new | no baseline | download create |
| present | present | no baseline | block unverified overlap |
| changed | unchanged | verified | upload update |
| unchanged | changed | verified | download update |
| changed | changed | verified | conflict; touch neither side |
| deleted | unchanged | verified | move reviewed Drive file ID to Trash |
| deleted | changed | verified | delete/modify conflict |
| unchanged | deleted | verified | recoverable local delete |
| changed | deleted | verified | delete/modify conflict |
| deleted | deleted | verified | acknowledge converged deletion |

Modification time is not used as conflict authority. If evidence is incomplete, ambiguous, ignored, unsafe, or colliding on a case-insensitive filesystem, AtrisBridge blocks the item instead of guessing.

### Deletion safety

**Local deletion → Drive:** the current remote ID, size, and MD5 must still match the synchronized evidence. AtrisBridge also rechecks that the local path is still absent immediately before the provider mutation. The reviewed Google Drive object is moved to **Trash by exact file ID**; permanent delete is not exposed. A postflight path check prevents a newly created object at the same path from being mistaken for the trashed object.

**Remote deletion → local:** the local BLAKE3 + size must still match baseline and targeted Drive checks must continue to prove that the remote path is absent. Before the local file is removed, AtrisBridge copies it under its application-data recovery area, verifies BLAKE3 + size, flushes the recovery file, persists an `applying` state, and only then removes the workspace file. Recovery metadata, deletion convergence, and operation completion are committed together in SQLite.

Available recovery copies are visible in the Two-Way panel. **Restore locally** verifies the app-data recovery file again, refuses to overwrite an existing path, recreates the file as a local-only change, and never modifies Drive. The next reviewed sync plan decides what to do with that recreated file.

### Provider race boundary

AtrisBridge deliberately does not claim provider-native atomic compare-and-swap for content writes or Trash. Fresh inventories, targeted ID/checksum preflight, live absence checks, exact-ID Trash, postflight verification, and recoverable local mutations narrow the race windows, but another Drive client can still change an object between separate provider requests. Because Trash is recoverable and conflicts are never resolved automatically, uncertain states fail closed and require a fresh reviewed plan.

The direct Drive Trash control-plane request uses the current in-memory OAuth access token. If that access token is no longer valid, the operation fails safely and the provider can be reconnected; AtrisBridge still does not persist plaintext OAuth credentials.

See [docs/sync-engine.md](docs/sync-engine.md) and [docs/rclone-transport.md](docs/rclone-transport.md).

## Phase 4/5 one-way safety

Backup and restore remain available as explicit one-way workflows when the workspace is not in Two-Way mode.

- **Backup:** local → Drive only; local deletion never implies remote deletion.
- **Restore:** Drive → local only; remote absence never implies local deletion.
- Restore downloads go to hidden staging first and are verified before local apply.
- Existing local restore targets use a temporary `.bak` recovery copy until journal completion.

See [docs/backup-engine.md](docs/backup-engine.md) and [docs/restore-engine.md](docs/restore-engine.md).

## Roadmap

1. **Phase 0/1 — foundation and local inventory** ✅
2. **Phase 2 — SQLite sync journal and durable file state** ✅
3. **Phase 3 — restricted rclone transport + Google Drive observation** ✅
4. **Phase 4 — safe incremental backup** ✅
5. **Phase 5 — safe pull and restore** ✅
6. **Phase 6 — conflict-aware two-way synchronization** ✅
7. **Phase 7 — persistent secure credential storage + optional client-side encryption**
8. **Phase 8+ — continuous watch mode, tray, additional providers, and release pipeline**

See [docs/architecture.md](docs/architecture.md), [docs/sync-engine.md](docs/sync-engine.md), [docs/security.md](docs/security.md), [docs/rclone-transport.md](docs/rclone-transport.md), [docs/backup-engine.md](docs/backup-engine.md), and [docs/restore-engine.md](docs/restore-engine.md).

## Development

### Requirements

- Node.js LTS
- npm
- Rust stable
- Tauri 2 platform prerequisites for your OS

AtrisBridge does not execute an arbitrary `rclone` from the system `PATH`:

```bash
npm install
npm run sidecar:prepare
npm run tauri:dev
```

`sidecar:prepare` downloads rclone `v1.74.4` from the official release host, verifies the platform-specific SHA-256 digest, and places the executable under `src-tauri/binaries/`. The binary is ignored by Git.

Validation:

```bash
npm run build
cargo test --manifest-path src-tauri/Cargo.toml --lib
cargo check --manifest-path src-tauri/Cargo.toml
```

## Google Drive behavior

Google Drive uses browser-based OAuth with `drive.file`. Provider operations are restricted to an `AtrisBridge/...` managed workspace path. Regular transfer bytes remain behind narrow rclone operations; Phase 6 uses a narrow direct Drive API request only for moving the exact reviewed file ID to Trash.

Remote provider checksums such as MD5 are stored as provider evidence and never treated as BLAKE3. Native Google Docs are skipped by the current regular-file adapter.

The OAuth token is held only in process memory. Restarting AtrisBridge requires reconnecting until the Phase 7 secure credential layer is introduced.

## `.atrisbridgeignore`

AtrisBridge supports gitignore-compatible project rules in `.atrisbridgeignore`. Built-in rules remain active even if the custom file is absent.

```gitignore
artifacts/
cache/
customer-dumps/
*.bak
```

Built-in rules exclude Git metadata, common generated/IDE directories, `.env*`, common private-key/certificate formats, and AtrisBridge `.part`/`.bak` transfer-recovery artifacts.

## Durable sync journal

AtrisBridge stores coordination state in `atrisbridge.db` under the OS application-data directory. SQLite uses foreign keys, WAL journaling, and a bounded busy timeout. Local observations, remote observations, synchronized baselines, plans, conflicts, and recovery metadata remain separate evidence classes.

Phase 5 and Phase 6 add feature-owned tables idempotently without destructively rewriting the existing Phase 4 core tables. Interrupted operations are never silently retried or declared synchronized; startup recovery retires or safely rolls back only states for which stored fingerprints prove the mutation.

## Security and project policy

AtrisBridge can reduce accidental leakage, but it cannot grant permission to upload proprietary or customer-controlled code to third-party infrastructure. Always follow the policy, DLP, contractual, data-residency, export-control, and authorization requirements that apply to the project you are synchronizing.

Do not report security vulnerabilities in public issues. See [SECURITY.md](SECURITY.md).

## License

Licensed under the [Apache License 2.0](LICENSE).
