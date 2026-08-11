# Sync Engine Design

AtrisBridge synchronization is evidence-first and review-first. Phase 8 adds continuous reconciliation and conservative scheduling without changing the conflict authority introduced in Phase 6 or the encryption evidence model introduced in Phase 7.

## Evidence model

Every synchronized path is evaluated from three independent snapshots:

1. **current local observation**,
2. **current remote observation**,
3. **last successfully accepted synchronized baseline**.

Modification time is never used as the winner rule.

Local content uses BLAKE3 + size. Plain Drive content uses provider ID + size + MD5. Encrypted Drive content uses logical plaintext size together with the underlying ciphertext Drive ID + ciphertext MD5 under checksum type `RCLONE_CRYPT_MD5`.

The journal never compares local BLAKE3 and provider MD5 as if they were equivalent hashes.

## Local scanner

The scanner validates the workspace root, applies built-in exclusions and `.atrisbridgeignore`, refuses symlink traversal, hashes regular files with BLAKE3, and persists the full inventory while returning only a bounded UI preview.

Built-in exclusions include generated folders, Git/IDE metadata, `.env*`, common private-key/certificate formats, AtrisBridge `.part`/`.bak` recovery artifacts, and the reserved encryption sentinel name `.atrisbridge-key-check`.

Unreadable entries and scan warnings are surfaced rather than interpreted as deletions. Phase 8 automatic cycles fail closed if their full scanner pass is not clean enough to provide trusted evidence.

## Durable journal

SQLite stores non-secret coordination state under OS application data. Relevant state includes:

- workspaces and sync mode,
- local/remote scan history,
- file observations,
- last synchronized baselines,
- backup/restore/two-way plans and item evidence,
- tombstones/conflicts,
- recovery metadata,
- non-secret workspace-encryption metadata,
- continuous-watch configuration and latest cycle state.

OAuth JSON and encryption recovery keys are never stored in SQLite.

## Two-way decision matrix

With a complete shared baseline:

- local changed / remote unchanged → upload update,
- local unchanged / remote changed → download update,
- both changed → conflict,
- local deleted / remote unchanged → remote Trash,
- local deleted / remote changed → conflict,
- remote deleted / local unchanged → recoverable local delete,
- remote deleted / local changed → conflict,
- both absent → acknowledge converged deletion.

Without an accepted baseline, overlapping local+remote paths are blocked rather than guessed. Local-only or remote-only content can be created on the missing side when complete evidence exists.

## Encrypted provider evidence

Encrypted workspaces preserve the same decision matrix. The provider evidence representation is different but conflict authority is not.

A valid encrypted remote observation requires:

- a logical path that decrypts/lists successfully,
- logical plaintext size,
- exactly one mapped underlying ciphertext Drive object,
- ciphertext Drive file ID,
- valid ciphertext MD5 (`RCLONE_CRYPT_MD5`),
- a valid encrypted-workspace sentinel/key state.

If crypt cannot map or authenticate the namespace, remote reconciliation aborts. A wrong key, missing sentinel, or corrupted ciphertext must never appear as a set of remote deletions.

## Manual planning and execution order

All transfer modes follow the same high-level sequence:

1. refresh local evidence,
2. refresh remote evidence,
3. create/persist a reviewable plan,
4. user reviews safe/conflict/blocked items,
5. execution refreshes local and remote evidence again,
6. each item is revalidated immediately before mutation,
7. transfer/apply is staged or recoverable where applicable,
8. provider/local postflight evidence is checked,
9. synchronized baseline is committed only through conditional SQL matching the planned evidence.

Backup, restore, and two-way execution are mutually exclusive per workspace through backend mode/plan checks.

## Phase 8 continuous reconciliation

Phase 8 does **not** create a second synchronization engine. Watcher and polling signals only decide when to run the existing scanner/provider observation/planner stack.

### Local trigger

Native `notify` events are treated as dirty hints. Relevant bursts are coalesced behind a 1.8 second settling window, then the normal full scanner rebuilds local evidence.

### Remote trigger

Because local filesystem events cannot observe edits made by another machine, enabled workspaces periodically refresh provider evidence. The interval is bounded from 30 seconds through 60 minutes; the default is 60 seconds.

### Workspace ownership

Only one automatic cycle may run for a workspace at once. New dirty signals are queued/coalesced. While watch mode is active, manual mutating IPC commands that could rewrite filesystem/provider/journal state are rejected by the Rust command boundary.

## Automatic policy gate

`Auto-apply safe transfers` is a separate opt-in and defaults off.

When it is off, a transfer-only plan is retained as a **review** outcome. This is distinct from destructive or ambiguous `attention` state, so enabling auto-apply later allows unchanged safe evidence to be evaluated again.

When it is on:

### Backup

Automatic execution is allowed only if there is at least one upload and zero blocked paths.

### Pull

Automatic execution is allowed only if there is at least one download and zero blocked paths.

### Two-Way

Automatic execution is allowed only if there is at least one upload/download, zero conflicts, zero blocked paths, and **zero deletion actions**.

Any deletion, conflict, blocked path, scanner warning, provider/encryption uncertainty, or failed transfer prevents automatic mutation.

## Deletion behavior

Phase 8 never automatically applies deletion actions.

Manual local → remote deletion still requires local absence, matching baseline remote evidence, exact reviewed Drive file ID, Trash rather than permanent delete, and postflight checks.

Manual remote → local deletion still requires repeated remote absence, unchanged baseline local evidence, a verified app-data recovery copy, applying state, final checks, and evidence-locked completion.

For encrypted workspaces the exact reviewed remote ID is the ciphertext Drive object ID.

## Upload behavior

Plain and encrypted uploads both require stable local BLAKE3 + size before/after transfer.

For plaintext Drive, accepted remote evidence is provider ID + size + MD5. For encrypted Drive, accepted evidence is logical size + ciphertext provider ID + ciphertext MD5.

An ambiguous encrypted upload process failure is fail-closed because randomized ciphertext means AtrisBridge cannot safely reconstruct the exact provider ciphertext checksum from plaintext after the fact.

## Download behavior

Downloads never target an existing project file directly. Data first lands in an AtrisBridge-owned staging path.

Plain downloads require local staging size + MD5 to match reviewed Drive evidence. Encrypted downloads require ciphertext ID/checksum to remain stable through targeted provider stat while rclone crypt authenticates/decrypts into the local stage; AtrisBridge then fingerprints the plaintext stage with BLAKE3 before recoverable apply.

Existing local targets use `.bak` recovery until SQLite completion succeeds.

## Continuous retry and churn control

Repeated provider/runtime failures use bounded backoff. Successful cycles reset failure count.

The scheduler stores an evidence signature for equivalent outcomes. Repeated no-op or real attention states with unchanged evidence do not continuously generate fresh plans or provider churn. A safe transfer waiting only for user review is intentionally not suppressed as destructive attention, so policy changes can re-evaluate it.

## Startup recovery order

On application startup interrupted backup/restore/two-way recovery runs first. Only after crash recovery completes are durable continuous-watch settings resumed. The watcher must never race an incomplete mutation from a previous process lifetime.

## Recovery-key lifecycle

The recovery key lives in the OS credential vault and can be explicitly exported/imported. Workspace removal does not automatically delete that key; fail-safe retention avoids irrecoverable loss of existing remote ciphertext.
