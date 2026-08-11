# rclone and Google Drive transport boundary

AtrisBridge owns synchronization and encryption policy. rclone and Google Drive are constrained provider/transport adapters; neither decides which copy wins, whether a conflict may be overwritten, or whether a deletion is safe to propagate.

## Runtime resolution

AtrisBridge never searches the system `PATH` for rclone.

- Development: `src-tauri/binaries/rclone(.exe)` prepared by `npm run sidecar:prepare`.
- Packaged application: verified executable under the application resource directory at `rclone/rclone(.exe)`.

The current runtime accepts exactly rclone `v1.74.4`. The preparation script verifies the platform-specific release SHA-256 before use.

## Google Drive authorization and persistence

Google Drive uses browser authorization with `drive.file`. Phase 7 persists the returned OAuth JSON only in the operating-system credential vault. The Rust `ProviderSessionStore` lazily loads it into memory when a cloud operation requires it.

OAuth JSON is not written to SQLite, `rclone.conf`, `.env`, repository files, or synchronized workspaces.

## Invocation model

rclone is launched directly with `std::process::Command`; there is no shell interpolation. Inherited rclone config/credential/crypt environment variables are removed before each invocation.

The frontend cannot supply arbitrary rclone arguments. Dedicated adapters expose only the required operations: runtime/version validation, Drive authorization/user info, inventory/stat, local MD5, reviewed single-file upload, staged single-file download, and the Phase 7 crypt operations described below.

`sync`, `bisync`, purge, move, mount, serve, remote control, and generic command execution remain unavailable.

## Plaintext remote evidence

For a normal workspace, Drive observations contain provider ID, logical size, modification metadata, checksum type `MD5`, and provider MD5. Local BLAKE3 remains separate.

Native Google Docs are skipped by the regular-file adapter because they do not provide the same regular-file content/checksum semantics.

## Phase 7 encrypted transport

An encrypted workspace routes logical content through a dedicated rclone crypt remote backed by:

```text
:drive:<managed-root>/.atrisbridge-crypt-v1
```

The crypt password and second password/salt are derived from the AtrisBridge recovery master key. AtrisBridge calls the pinned rclone `obscure` command with secret input over stdin, then supplies the obscured values only to the constrained crypt child process environment. No crypt config is persisted.

Phase 7 sets filename/directory-name encryption off and data encryption on. As a result:

- file contents are encrypted,
- logical filenames/directories remain visible,
- encrypted regular files use the crypt backend's `.bin` representation,
- the existing AtrisBridge logical path model remains usable.

The reserved logical file `.atrisbridge-key-check` acts as a key-verification sentinel. Its decrypted content must match exactly before an imported recovery key is accepted.

## Encrypted inventory and provider evidence

AtrisBridge does not trust only the logical crypt listing. It reads both:

1. logical decrypted inventory through rclone crypt, and
2. raw underlying Drive ciphertext inventory.

Each logical file must map to exactly one raw ciphertext object. The journal stores:

- logical plaintext relative path,
- logical plaintext size,
- raw ciphertext Drive file ID,
- checksum type `RCLONE_CRYPT_MD5`,
- raw ciphertext MD5.

This evidence is deliberately not compared to local plaintext MD5/BLAKE3.

Missing sentinel, unmapped ciphertext, duplicate mapping, wrong-key decryption failure, or a ciphertext/logical mismatch causes inventory/stat to fail closed. It is never converted into an apparently empty remote inventory that could create deletion intent.

## Encrypted upload

For an approved upload AtrisBridge fingerprints the local plaintext before transfer, uses a single-file crypt `copyto`, fingerprints local plaintext again, then targeted-stats the logical and underlying ciphertext object.

The accepted baseline requires local stability, logical size agreement, a Drive ciphertext ID, and ciphertext MD5.

Crypt ciphertext includes randomized encryption state, so if the rclone upload process itself reports an ambiguous failure AtrisBridge does not reconstruct an expected ciphertext hash and does not blindly accept/retry the mutation. It fails closed and requires fresh evidence/review.

## Encrypted download

A reviewed encrypted download still targets an AtrisBridge-owned hidden staging path rather than the final workspace file.

Before/after transfer the expected ciphertext Drive ID and `RCLONE_CRYPT_MD5` evidence are revalidated by targeted stat. rclone crypt decrypts/authenticates the content into the local stage; AtrisBridge then checks logical size and computes local BLAKE3 for the normal recoverable apply/journal flow.

For plaintext workspaces, the existing local MD5-versus-Drive-MD5 verification remains active.

## Exact-ID Trash

Deletion propagation does not use a path-based rclone delete. AtrisBridge sends one narrow Drive `files.update(..., trashed=true)` request for the exact reviewed provider file ID.

For an encrypted workspace that provider ID is the underlying ciphertext object ID. The same live local-absence checks, preflight evidence, postflight path check, and provider Trash recovery semantics from Phase 6 remain active.

Permanent Drive deletion is not implemented.

## Remaining concurrency boundary

Neither plaintext nor encrypted transport provides a cross-client distributed transaction across separate Drive requests. AtrisBridge narrows the race window with fresh inventory, targeted stat, exact plan evidence, exact-ID Trash, postflight checks, local recovery, and evidence-locked SQLite completion.

Uncertain results fail closed and require a new reviewed plan.

## Capability summary

| Capability | Phase 7 status |
| --- | --- |
| provider inventory / targeted stat | enabled, constrained |
| single-file upload | enabled after reviewed plan |
| staged single-file download | enabled after reviewed plan |
| optional content encryption/decryption | enabled per workspace |
| filename/directory-name encryption | intentionally disabled |
| Drive Trash | exact reviewed provider ID only |
| permanent remote delete | disabled |
| arbitrary rclone command surface | disabled |
| rclone `sync` / `bisync` | disabled |
| mount / serve / RC | disabled |
| automatic conflict resolution | disabled |
| continuous automatic sync | deferred to Phase 8+ |
