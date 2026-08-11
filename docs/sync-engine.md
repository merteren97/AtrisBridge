# Sync Engine Design

AtrisBridge synchronization is evidence-first and review-first. Phase 7 adds secure credential persistence and optional encrypted transport without changing the conflict authority introduced in Phase 6.

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

Unreadable entries are surfaced rather than interpreted as deletions.

## Durable journal

SQLite stores non-secret coordination state under OS application data. Relevant state includes:

- workspaces and sync mode,
- local/remote scan history,
- file observations,
- last synchronized baselines,
- backup/restore/two-way plans and item evidence,
- tombstones/conflicts,
- recovery metadata,
- non-secret workspace-encryption metadata.

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

Phase 7 preserves the same decision matrix for encrypted workspaces. The only difference is the provider evidence representation.

A valid encrypted remote observation requires:

- a logical path that decrypts/lists successfully,
- logical plaintext size,
- exactly one mapped underlying ciphertext Drive object,
- ciphertext Drive file ID,
- valid ciphertext MD5 (`RCLONE_CRYPT_MD5`),
- a valid encrypted-workspace sentinel/key state.

If crypt cannot map or authenticate the namespace, remote reconciliation aborts. A wrong key, missing sentinel, or corrupted ciphertext must never appear as a set of remote deletions.

## Planning and execution order

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

## Upload behavior

Plain and encrypted uploads both require stable local BLAKE3 + size before/after transfer.

For plaintext Drive, accepted remote evidence is provider ID + size + MD5. For encrypted Drive, accepted evidence is logical size + ciphertext provider ID + ciphertext MD5.

An ambiguous encrypted upload process failure is fail-closed because randomized ciphertext means AtrisBridge cannot safely reconstruct the exact provider ciphertext checksum from plaintext after the fact.

## Download behavior

Downloads never target an existing project file directly. Data first lands in an AtrisBridge-owned staging path.

Plain downloads require local staging size + MD5 to match reviewed Drive evidence. Encrypted downloads require the ciphertext ID/checksum to remain stable through targeted provider stat while rclone crypt authenticates/decrypts into the local stage; AtrisBridge then fingerprints the plaintext stage with BLAKE3 before recoverable apply.

Existing local targets use `.bak` recovery until SQLite completion succeeds.

## Deletion behavior

A missing observation is not automatically deletion authority.

Local → remote deletion:

- local path must remain absent,
- remote provider evidence must still equal baseline,
- exact reviewed Drive file ID is moved to Trash,
- path is checked again after Trash,
- uncertain replacement/race state does not converge the baseline.

Remote → local deletion:

- remote path is repeatedly checked absent,
- local BLAKE3 + size must still equal baseline,
- an app-data recovery copy is written, fingerprinted, and flushed,
- applying state is persisted,
- local evidence/remote absence are rechecked,
- only then can the local file be removed and deletion convergence committed.

For encrypted workspaces, the exact reviewed remote ID is the ciphertext Drive object ID.

## Encryption enable/import rules

Optional client-side encryption is an explicit workspace mode layered below the normal sync modes. It can be attached only before any synchronized baseline exists and while no transfer plan is ready/running.

Initial enablement requires an empty managed remote root. Importing an existing recovery key requires exact sentinel verification. Phase 7 does not automatically migrate established plaintext data to ciphertext or disable an encrypted workspace by rewriting remote content.

## Recovery-key lifecycle

The recovery key lives in the OS credential vault and can be explicitly exported/imported. Workspace removal does not automatically delete that key in Phase 7; fail-safe retention avoids irrecoverable loss of existing remote ciphertext.

## Phase 8 boundary

Phase 7 still requires explicit planning/execution. Continuous filesystem watching, background scheduling, tray automation, and unattended synchronization remain Phase 8+ work and must preserve the same conflict/evidence rules rather than bypass them.
