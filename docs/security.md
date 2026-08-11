# Security Design

## Security posture through Phase 7

AtrisBridge is local-first and deliberately separates synchronization policy, provider credentials, encryption keys, transport, and durable coordination state. No frontend command exposes a generic shell, filesystem, rclone, or Google Drive API surface.

Phase 7 adds two independent protections:

1. **OS-backed credential persistence** for Google Drive OAuth credentials.
2. **Optional client-side content encryption** for a workspace before bytes reach Google Drive.

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

AtrisBridge previously planned a Stronghold-specific vault. Phase 7 intentionally uses the OS-native credential facility instead, keeping secret ownership outside the webview and avoiding a second application-managed plaintext key hierarchy.

## Encryption recovery keys

An encrypted workspace uses a 32-byte master secret represented for recovery as `AB1-` followed by 64 hexadecimal characters. Two separate rclone crypt secrets are derived from that master using domain-separated BLAKE3 key derivation.

The recovery key is:

- saved in the OS credential vault,
- scoped by verified Google account identity plus managed remote root when deriving the local vault reference,
- shown after initial enablement because the user needs an offline recovery copy,
- readable later only through an explicit **Export recovery key** action,
- accepted through an explicit import action only after it decrypts the remote verification sentinel exactly.

AtrisBridge does not expose an automatic workspace-encryption-key deletion path in Phase 7. Removing workspace metadata may therefore leave the recovery key in the OS vault. This is intentional fail-safe retention: silently deleting the only local key could make existing remote ciphertext permanently unrecoverable.

## Optional client-side encryption

Encryption can be attached only when:

- the workspace has no accepted synchronized baseline,
- no backup/restore/two-way plan is ready or running,
- the managed remote root is empty, or an existing protected key can safely verify/recreate its empty initialization state,
- the provider is the verified Google Drive connection.

Phase 7 does not perform in-place plaintext migration or automatic disable/decrypt migration.

Encrypted data lives under a dedicated `.atrisbridge-crypt-v1` namespace beneath the bound managed root. A reserved `.atrisbridge-key-check` logical sentinel proves that the recovery key can decrypt the namespace. The scanner excludes that reserved name from normal workspace content.

If the encrypted namespace, sentinel, ciphertext mapping, provider ID, or ciphertext checksum evidence is missing/inconsistent, AtrisBridge fails closed. It must never turn crypt corruption or a wrong key into synthetic remote-deletion evidence.

## Metadata visibility

Phase 7 encrypts regular-file **content** but intentionally configures filename and directory-name encryption off.

Therefore Google Drive can still observe:

- filenames,
- directory structure,
- ciphertext object sizes/metadata,
- normal provider account/activity metadata.

This limitation is explicit. It preserves the current exact logical path, case-collision, provider-ID, and conflict/deletion evidence model. Full metadata hiding requires a future evidence model rather than silently weakening synchronization correctness.

## rclone crypt secret handling

AtrisBridge does not write crypt passwords to rclone configuration. Derived secrets are obscured through the pinned rclone runtime with the input delivered over stdin, then supplied only to the constrained child process environment for the required operation.

Inherited `RCLONE_*` credential/config/crypt variables are removed before invocation. Generic rclone command execution, `sync`, `bisync`, RC, mount, serve, purge, and permanent remote deletion remain unavailable.

## Evidence separation

Local plaintext evidence remains BLAKE3 + size. Plain Google Drive files use provider ID + size + MD5. Encrypted Drive files use logical plaintext size together with the **underlying ciphertext Drive file ID + ciphertext MD5**, stored under the distinct checksum type `RCLONE_CRYPT_MD5`.

Ciphertext MD5 is never treated as plaintext MD5 or BLAKE3. During encrypted download, rclone crypt authenticates/decrypts the content locally; AtrisBridge revalidates the ciphertext provider evidence before/after transfer and checks local size/fingerprint before journal completion.

Because encrypted uploads use randomized ciphertext, an ambiguous rclone process failure is not blindly retried or accepted from reconstructed local ciphertext evidence. The operation fails closed and requires fresh observation/review.

## Deletion protections

Phase 6 deletion protections remain active for encrypted workspaces:

- local deletion propagation targets the exact reviewed **ciphertext Drive file ID** and moves it to Trash rather than permanent delete,
- remote deletion propagation requires repeated remote absence checks and a verified app-data recovery copy before local removal,
- delete/modify races become conflicts,
- postflight and evidence-locked SQLite completion remain required.

## Trust boundaries

- **React UI:** presentation/review only; no generic secret/filesystem/provider access.
- **Rust core:** secret access, path validation, planning, execution, recovery, and encryption policy.
- **OS credential vault:** persistent OAuth/recovery-key storage outside SQLite/workspace data.
- **SQLite:** durable non-secret coordination evidence; never proof that external state is still current.
- **rclone sidecar:** pinned and constrained byte/encryption transport.
- **Google Drive:** remote and concurrently mutable; never synchronization authority.
- **local recovery area:** app-data recovery material verified by BLAKE3 + size before use.

## Public-repository rule

Examples, screenshots, fixtures, tests, logs, and documentation must use synthetic data. Never commit OAuth JSON, recovery keys, crypt passwords, customer/company secrets, or real protected workspace contents.
