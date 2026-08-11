# Architecture

## Goal

AtrisBridge is a local-first synchronization coordinator for source-code and engineering workspaces. The application owns safety policy, workspace state, conflict decisions, reviewable plans, recovery, and user-visible history. Storage providers are transports rather than synchronization authority.

## Current architecture — Phase 0 through Phase 6

```text
React / TypeScript UI
        |
        | narrow Tauri IPC
        v
Tauri / Rust application core
        |
        +-- workspace + sync-mode management
        +-- safe local scanner
        +-- .atrisbridgeignore + built-in exclusions
        +-- BLAKE3 local fingerprints
        +-- portable path / symlink guards
        +-- SQLite evidence journal
        |     |
        |     +-- local observations
        |     +-- remote observations
        |     +-- synchronized baselines
        |     +-- backup / restore / two-way plans
        |     +-- conflicts + deletion recovery metadata
        |
        +-- backup engine (local -> Drive)
        +-- restore engine (Drive -> local staging -> verified apply)
        +-- Phase 6 two-way planner / executor
              |
              +-- restricted rclone byte transport
              |     +-- inventory / stat
              |     +-- single-file upload
              |     +-- staged single-file download
              |
              +-- narrow Google Drive control plane
                    +-- exact reviewed file ID -> Trash
```

The frontend never receives a generic filesystem, shell, rclone, or Drive API command surface. It calls narrow domain commands such as prepare/execute plan, set workspace mode, and restore a verified local recovery copy.

## Durable state ownership

SQLite is the local source of truth for AtrisBridge coordination state, but it is never proof that the provider or filesystem is still unchanged. Execution therefore re-observes state before mutation and uses evidence-locked SQL completion.

The journal separates:

1. local filesystem observations,
2. remote provider observations,
3. last successfully synchronized baseline,
4. immutable/reviewable plan evidence,
5. conflict/block decisions,
6. deletion/recovery metadata,
7. scan and execution history.

The database lives in OS application data rather than inside the synchronized workspace.

## Scanner boundary

The scanner produces a complete inventory for the journal and only a bounded preview for the UI. It:

- hashes regular files with BLAKE3,
- does not follow symlinks,
- applies built-in generated/secret exclusions,
- applies `.atrisbridgeignore`,
- excludes AtrisBridge `.part`/`.bak` transfer-recovery artifacts.

Unreadable paths are warnings/errors, not implicit deletion authority.

## Planner boundary

The planner compares **current local**, **current remote**, and **last synchronized** evidence. It does not use modification time as a winner rule.

Phase 6 classifies each path into an explicit safe action, conflict, block, or no-op. The executor accepts only persisted `ready` actions and refreshes both inventories again before beginning.

Backup, restore, and two-way execution are mutually exclusive per workspace in the normal command path.

## Provider boundaries

### rclone

rclone is a restricted byte transport and observation adapter. AtrisBridge does not expose generic rclone arguments, `sync`, `bisync`, RC, mount, or provider-wide destructive commands.

### Direct Google Drive control plane

Phase 6 adds one narrow exception: local-deletion propagation moves the **exact reviewed Drive file ID** to Trash through Google Drive `files.update` with `trashed=true`.

This is intentionally not a general Drive API abstraction. The request uses the current in-memory provider session, returns fail-closed on authentication/provider errors, and does not add persistent plaintext credentials.

Permanent remote deletion is not part of the current architecture.

## Local recovery boundary

A remote deletion cannot directly remove a local file. The Phase 6 engine first creates an app-data recovery copy, verifies BLAKE3 + size, flushes it, persists apply evidence, and repeatedly rechecks remote absence before local removal.

Recovery metadata and deletion convergence are transactionally coupled. Verified recovery copies can be restored locally through an explicit user action that refuses overwrite and never changes Drive.

## Process and concurrency boundaries

SQLite uses short-lived connections, foreign keys, WAL journaling, a bounded busy timeout, and conditional updates against the exact planned evidence.

AtrisBridge cannot make the local filesystem and Google Drive one distributed atomic transaction. Safety therefore comes from:

- fresh observation before planning and execution,
- exact plan evidence,
- repeated targeted preflight,
- live absence checks around deletion propagation,
- exact-ID remote Trash,
- staged/recoverable local mutation,
- postflight verification,
- fail-closed conflict/block behavior.

Provider-side races between separate network requests are documented rather than hidden behind last-write-wins.

## Trust boundaries

- **React UI:** untrusted from filesystem/shell/provider perspective; narrow IPC only.
- **Rust core:** validates paths, owns planner/executor/recovery policy and database state.
- **SQLite journal:** durable coordination evidence; never proof that external state is still current.
- **rclone sidecar:** powerful external process constrained to dedicated functions and pinned runtime.
- **Google Drive API:** remote/fallible; exact-ID Trash only, no generic frontend surface.
- **local recovery area:** AtrisBridge-owned app-data content verified by BLAKE3 + size before use.
- **storage provider:** potentially concurrent with other clients; never trusted to make conflict decisions for AtrisBridge.
