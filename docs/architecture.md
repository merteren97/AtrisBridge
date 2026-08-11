# Architecture

## Goal

AtrisBridge is a local-first synchronization coordinator for source-code and engineering workspaces. The application owns safety policy, workspace state, conflict decisions, reviewable plans, recovery, credential routing, and optional encryption policy. Storage providers are transports rather than synchronization authority.

## Current architecture — Phase 0 through Phase 7

```text
React / TypeScript UI
        |
        | narrow Tauri IPC
        v
Tauri / Rust application core
        |
        +-- workspace + sync-mode management
        +-- safe local scanner + .atrisbridgeignore
        +-- BLAKE3 plaintext fingerprints
        +-- portable path / symlink guards
        +-- SQLite evidence journal (no secrets)
        |     +-- local/remote observations
        |     +-- synchronized baselines
        |     +-- backup/restore/two-way plans
        |     +-- conflicts + recovery metadata
        |     +-- non-secret encryption metadata
        |
        +-- OS credential vault
        |     +-- Google Drive OAuth JSON
        |     +-- encrypted-workspace recovery key
        |
        +-- backup / restore / two-way planner-executors
              |
              +-- optional content-encryption routing
              |     +-- local plaintext <-> rclone crypt
              |     +-- encrypted namespace + key sentinel
              |
              +-- restricted rclone transport
              |     +-- inventory / targeted stat
              |     +-- single-file upload
              |     +-- staged single-file download
              |
              +-- narrow Google Drive control plane
                    +-- exact reviewed Drive file ID -> Trash
```

The frontend never receives generic filesystem, shell, rclone, keyring, or Drive API capability. It receives only domain results and recovery keys when the user explicitly enables/exports encryption.

## Secret ownership

SQLite contains coordination metadata only. OAuth JSON and encryption recovery keys live in the operating-system credential vault and are loaded into Rust memory only when needed.

An encrypted-workspace metadata row stores an opaque key reference, pinned managed remote root, encrypted namespace, key version, and verification timestamps. The master/recovery key itself is not stored in SQLite.

## Durable evidence ownership

SQLite is AtrisBridge's local coordination source of truth, but never proof that provider/filesystem state is still current. Execution re-observes both sides before mutation and uses evidence-locked SQL completion.

Evidence is deliberately typed:

- local plaintext: BLAKE3 + size,
- plaintext Drive object: provider ID + size + MD5,
- encrypted Drive object: logical plaintext size + underlying ciphertext Drive ID + ciphertext MD5 (`RCLONE_CRYPT_MD5`).

Provider hashes are never compared to local BLAKE3 as if they were the same algorithm or representation.

## Encryption boundary

Phase 7 can route a workspace through a dedicated `.atrisbridge-crypt-v1` namespace under its managed Drive root.

The first encrypted format protects regular-file contents while leaving filenames and directory structure visible. This keeps the current logical-path, collision, provider-ID, exact-ID Trash, and conflict model intact.

A reserved encrypted sentinel verifies the recovery key. Missing/corrupt sentinel or ciphertext evidence is an error rather than an empty remote inventory. This prevents wrong-key/corruption states from creating false delete intent.

Encryption can be enabled/imported only before a synchronized baseline exists. AtrisBridge does not automatically migrate an established plaintext workspace to ciphertext or vice versa.

## Planner boundary

Backup, restore, and two-way planners compare current local, current remote, and last accepted baseline evidence. Modification time is never a winner rule.

Phase 7 does not alter conflict semantics: encrypted provider evidence participates in the same baseline comparisons using its distinct checksum type. A file changing on both sides remains a conflict.

## Provider boundaries

### Restricted rclone

rclone remains a constrained observation/byte transport. For encrypted workspaces it additionally performs crypt encryption/decryption with ephemeral process configuration. Generic arguments, `sync`, `bisync`, RC, mount, serve, purge, and arbitrary destructive commands remain unavailable.

### Google Drive exact-ID control plane

Reviewed remote Trash remains the only narrow direct Drive mutation outside rclone. For encrypted workspaces the reviewed ID is the underlying ciphertext Drive object ID, so path reuse cannot redirect the Trash request to another object.

Permanent remote deletion is not implemented.

## Recovery boundary

Remote deletion cannot directly remove a local file. AtrisBridge first creates an app-data recovery copy, verifies BLAKE3 + size, persists applying state, repeats remote-absence checks, and only then removes the local file.

Encryption recovery keys have a separate lifecycle: Phase 7 intentionally does not auto-delete them when workspace metadata is removed, because losing the last key could make remote ciphertext unrecoverable.

## Concurrency boundary

The local filesystem and Google Drive cannot be made one distributed atomic transaction. Safety is built from fresh observations, reviewable immutable plan evidence, repeated targeted preflight, exact-ID remote Trash, staged/recoverable local mutations, postflight verification, and conditional SQLite completion.

Provider-side races are surfaced as failures/conflicts requiring fresh review rather than hidden behind last-write-wins.

## Trust boundaries

- **React UI:** untrusted for direct filesystem/provider/secret operations.
- **Rust core:** owns planner/executor, path safety, secret routing, encryption policy, and recovery.
- **OS credential vault:** persistent secret storage.
- **SQLite:** non-secret durable evidence/history.
- **rclone sidecar:** pinned external transport constrained to dedicated operations.
- **Google Drive:** fallible/concurrent remote store, never synchronization authority.
- **app-data recovery area:** local recovery material verified before use.
