# Architecture

## Goal

AtrisBridge is a local-first synchronization coordinator for source-code and engineering workspaces. The application owns safety policy, workspace state, conflict decisions, and user-visible history. Storage providers are transports rather than the authority for sync semantics.

## Current architecture — Phase 0/1/2

```text
React / TypeScript UI
        |
        | constrained Tauri IPC
        v
Tauri / Rust application core
        |
        +-- safe local scanner
        +-- .atrisbridgeignore matcher
        +-- BLAKE3 fingerprints
        +-- SQLite durable sync journal
              |
              +-- workspaces
              +-- scan_runs
              +-- file_entries
              +-- pending_operations
```

The frontend never receives generic filesystem or shell permissions. A native directory dialog returns the user-selected workspace path, then narrow Rust commands own the remaining filesystem access.

Workspace metadata created by the Phase 0/1 JSON implementation is imported into SQLite once on first launch after the Phase 2 migration. The legacy file is left untouched as a recovery aid; `app_meta` prevents repeated imports.

## Durable state ownership

SQLite is the local source of truth for AtrisBridge coordination state. The journal separates:

1. local filesystem observations,
2. last-known remote observations,
3. last-successfully-synchronized state,
4. pending operations,
5. tombstones/deletions,
6. conflicts and user resolutions,
7. scan history.

The database is stored in the operating system's application-data directory rather than inside a workspace, so AtrisBridge metadata is never accidentally synchronized as project content.

## Planned transport architecture

```text
Local workspace
      |
      v
Inventory + SQLite sync journal
      |
      v
Sync planner / conflict engine
      |
      v
Restricted rclone sidecar
      |
      +-- Google Drive
      +-- OneDrive
      +-- S3-compatible storage
      +-- WebDAV
```

AtrisBridge will not be a general-purpose rclone GUI. rclone is planned only as a provider transport layer; AtrisBridge remains responsible for deciding *what* operation is safe to perform.

## Process boundaries

The current database API opens short-lived SQLite connections with a busy timeout and WAL enabled. This keeps Phase 2 simple while allowing future background workers and UI commands to coexist without turning the frontend into a database client.

The scanner produces a complete in-memory inventory for the journal and only a bounded 250-file preview for the UI. If very large workspace profiling later shows this is too memory-heavy, the scanner/journal boundary can move to batched streaming without changing the database schema.

## Trust boundaries

- **React UI:** untrusted from a filesystem/shell perspective; uses narrow commands only.
- **Rust core:** validates paths, owns local state, database migrations, and safety rules.
- **SQLite journal:** local coordination state only; never treated as proof that remote content is still unchanged.
- **rclone sidecar (planned):** powerful external process; arguments and capabilities must be tightly scoped.
- **storage provider (planned):** remote, fallible and potentially stale; never treated as proof that local destructive operations are safe.
