# Security Design

## Current controls

Phase 0/1 deliberately keeps the attack surface small:

- no cloud credentials,
- no provider SDK,
- no external sidecar,
- no shell command capability,
- no generic frontend filesystem capability,
- directory access starts from explicit native user selection,
- symlinks are skipped,
- sensitive filename patterns are excluded before hashing/inventory exposure,
- workspace removal changes AtrisBridge metadata only.

## Tauri capability model

The desktop capability grants core defaults and the native directory-open dialog. Filesystem scanning is performed by application-owned Rust commands so that the UI cannot arbitrarily invoke a generic read/write API.

## Provider phase requirements

Before Google Drive support is merged:

1. provider authentication must not leak tokens into logs or the repository,
2. rclone configuration must be encrypted at rest,
3. the key/password protecting provider configuration must live outside plaintext app config,
4. sidecar command arguments must be narrow and validated,
5. no unauthenticated rclone RC endpoint may be exposed,
6. cloud operations must be represented in the durable journal before execution,
7. upload retry must be idempotent or safely detectable,
8. remote deletion must be recoverable by default.

## Public-repository rule

Examples, screenshots, fixtures, tests, logs, and documentation must use synthetic data. Customer/company names should not be used as sample workspace content unless explicitly approved for public disclosure.
