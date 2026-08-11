# Architecture

## Goal

AtrisBridge is a local-first synchronization coordinator for source-code and engineering workspaces. The application owns safety policy, workspace state, conflict decisions, and user-visible history. Storage providers are transports rather than the authority for sync semantics.

## Phase 0/1

```text
React / TypeScript UI
        |
        | constrained Tauri IPC
        v
Tauri / Rust application core
        |
        +-- workspace metadata (application-data directory)
        +-- safe local scanner
        +-- .atrisbridgeignore matcher
        +-- BLAKE3 fingerprints
```

The frontend never receives generic filesystem or shell permissions. A native directory dialog returns the user-selected workspace path, then narrow Rust commands own the remaining filesystem access.

## Planned transport architecture

```text
Local workspace
      |
      v
Inventory + durable sync journal
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

## State ownership

The long-term state model will separate:

1. local filesystem observations,
2. last-known remote observations,
3. last-successfully-synchronized state,
4. pending operations,
5. tombstones/deletions,
6. conflicts and user resolutions.

Phase 2 moves this state into SQLite before any bidirectional synchronization is enabled.

## Trust boundaries

- **React UI:** untrusted from a filesystem/shell perspective; uses narrow commands only.
- **Rust core:** validates paths, owns local state and safety rules.
- **rclone sidecar (planned):** powerful external process; arguments and capabilities must be tightly scoped.
- **storage provider (planned):** remote, fallible and potentially stale; never treated as proof that local destructive operations are safe.
