# Security Design

## Security posture through Phase 8

AtrisBridge is local-first and deliberately separates synchronization policy, provider credentials, encryption keys, transport, durable coordination state, and continuous scheduling. No frontend command exposes a generic shell, filesystem, rclone, watcher, or Google Drive API surface.

Phase 7 added OS-backed credential persistence and optional client-side content encryption. Phase 8 adds continuous reconciliation while preserving the same evidence authority and making background automation intentionally more conservative than manual review.

These features do not grant permission to move protected source code to a third-party provider; organizational policy and authorization remain external requirements.

## Operating-system credential vault

Provider OAuth JSON and workspace recovery keys are stored through the Rust `keyring` abstraction backed by the platform credential facility. AtrisBridge keeps only opaque references in its own metadata.

Secrets are not persisted in:

- SQLite,
- `rclone.conf`,
- `.env` files,
- repository files,
- synchronized workspace files,
- provider metadata rows.

Google Drive credentials are loaded lazily into backend memory when required. Removing the saved provider credential removes the OS-vault credential and requires OAuth again; forgetting the provider also removes AtrisBridge provider metadata but does not delete Drive content.

## Continuous-watch authority

Filesystem notifications are not synchronization authority. Phase 8 treats them only as dirty signals that schedule a full scanner/provider evidence refresh.

A local event therefore cannot directly cause a file mutation. The sequence remains scanner → provider observation → persisted planner → automatic-policy gate → guarded executor.

Only one automatic cycle may own a workspace at a time. Event bursts are debounced/coalesced, remote reconciliation is bounded, and repeated failures use bounded backoff.

## Auto-apply safety boundary

`Auto-apply safe transfers` is explicit opt-in and defaults off.

Even when enabled, automatic execution is limited to transfer-only plans whose existing planner reports complete safe evidence:

- no conflicts,
- no blocked paths,
- no scanner uncertainty,
- no provider/encryption uncertainty,
- **no deletion actions**.

Phase 8 never automatically moves a Drive object to Trash and never automatically performs recoverable local deletion. Those operations remain explicit reviewed Phase 6 flows.

When auto-apply is disabled, safe transfer plans surface as review state rather than destructive attention. This distinction prevents a policy toggle from being incorrectly suppressed by unchanged-evidence churn protection.

## IPC race protection

UI disabled states are not treated as a security boundary. While watch mode owns a workspace, guarded Rust commands reject manual mutation that could race the scheduler.

Protected operations include provider binding changes, sync-mode changes, encryption enable/import, evidence-writing scans, backup/restore/two-way prepare or execute, recovery-copy restore, watched-provider disconnect/forget, and workspace removal.

Read-only status and explicit recovery-key export remain available. Users pause watch mode before returning to manual reviewed mutation.

## Scanner and provider uncertainty

Every automatic cycle runs the normal full scanner and fresh provider observation. Scanner warnings or incomplete evidence stop automatic planning/execution rather than being converted into deletions.

Missing provider credentials, unavailable rclone, encryption key loss, sentinel inconsistency, ciphertext mapping errors, or provider transport failures fail closed.

## Encryption recovery keys

An encrypted workspace uses a 32-byte master secret represented for recovery as `AB1-` followed by 64 hexadecimal characters. Two separate rclone crypt secrets are derived from that master using domain-separated BLAKE3 key derivation.

The recovery key is:

- saved in the OS credential vault,
- scoped by verified Google account identity plus managed remote root when deriving the local vault reference,
- shown after initial enablement because the user needs an offline recovery copy,
- readable later only through an explicit **Export recovery key** action,
- accepted through explicit import only after it decrypts the remote verification sentinel exactly.

AtrisBridge does not expose automatic workspace-encryption-key deletion. Removing workspace metadata may therefore leave the recovery key in the OS vault. This is intentional fail-safe retention: silently deleting the only local key could make existing remote ciphertext permanently unrecoverable.

Recovery-key import changes encryption state and is therefore blocked while watch mode owns that workspace; export is read-only and remains available.

## Optional client-side encryption

Encryption can be attached only when:

- the workspace has no accepted synchronized baseline,
- no backup/restore/two-way plan is ready or running,
- the managed remote root is empty, or an existing protected key can safely verify/recreate its empty initialization state,
- the provider is the verified Google Drive connection.

AtrisBridge does not perform in-place plaintext migration or automatic disable/decrypt migration.

Encrypted data lives under a dedicated `.atrisbridge-crypt-v1` namespace beneath the bound managed root. A reserved `.atrisbridge-key-check` logical sentinel proves that the recovery key can decrypt the namespace. The scanner excludes that reserved name from normal workspace content.

If the encrypted namespace, sentinel, ciphertext mapping, provider ID, or ciphertext checksum evidence is missing/inconsistent, AtrisBridge fails closed. It must never turn crypt corruption or a wrong key into synthetic remote-deletion evidence.

## Metadata visibility

AtrisBridge encrypts regular-file **content** but intentionally configures filename and directory-name encryption off in the current encrypted transport.

Therefore Google Drive can still observe:

- filenames,
- directory structure,
- ciphertext object sizes/metadata,
- normal provider account/activity metadata.

This limitation is explicit. It preserves the current exact logical path, case-collision, provider-ID, and conflict/deletion evidence model. Full metadata hiding requires a future evidence model rather than silently weakening synchronization correctness.

## rclone crypt secret handling

AtrisBridge does not write crypt passwords to rclone configuration. Derived secrets are obscured through the pinned rclone runtime with input delivered over stdin, then supplied only to the constrained child process environment for the required operation.

Inherited `RCLONE_*` credential/config/crypt variables are removed before invocation. Generic rclone command execution, `sync`, `bisync`, RC, mount, serve, purge, and permanent remote deletion remain unavailable.

## Evidence separation

Local plaintext evidence remains BLAKE3 + size. Plain Google Drive files use provider ID + size + MD5. Encrypted Drive files use logical plaintext size together with the **underlying ciphertext Drive file ID + ciphertext MD5**, stored under the distinct checksum type `RCLONE_CRYPT_MD5`.

Ciphertext MD5 is never treated as plaintext MD5 or BLAKE3. During encrypted download, rclone crypt authenticates/decrypts the content locally; AtrisBridge revalidates ciphertext provider evidence before/after transfer and checks local size/fingerprint before journal completion.

Because encrypted uploads use randomized ciphertext, an ambiguous rclone process failure is not blindly retried or accepted from reconstructed local ciphertext evidence. The operation fails closed and requires fresh observation/review.

## Deletion protections

Deletion protections remain manual and recoverable:

- local deletion propagation targets the exact reviewed Drive file ID and moves it to Trash rather than permanent delete,
- remote deletion propagation requires repeated remote absence checks and a verified app-data recovery copy before local removal,
- delete/modify races become conflicts,
- postflight and evidence-locked SQLite completion remain required,
- Phase 8 automatic cycles reject every deletion action before executor mutation.

## Startup recovery order

Interrupted backup, restore, or two-way operations are recovered before configured watchers resume. Durable watch settings never authorize a background cycle to race crash recovery from the previous process lifetime.

## Trust boundaries

- **React UI:** presentation/review only; no generic secret/filesystem/provider/watch access.
- **Rust core:** secret access, path validation, planning, execution, watcher/scheduler ownership, guarded IPC, recovery, and encryption policy.
- **OS credential vault:** persistent OAuth/recovery-key storage outside SQLite/workspace data.
- **SQLite:** durable non-secret coordination evidence and watch state; never proof that external state is still current.
- **rclone sidecar:** pinned and constrained byte/encryption transport.
- **Google Drive:** remote and concurrently mutable; never synchronization authority.
- **local recovery area:** app-data recovery material verified by BLAKE3 + size before use.

## Public-repository rule

Examples, screenshots, fixtures, tests, logs, and documentation must use synthetic data. Never commit OAuth JSON, recovery keys, crypt passwords, customer/company secrets, or real protected workspace contents.
