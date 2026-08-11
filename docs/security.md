# Security Design

## Current trust model

AtrisBridge is local-first and treats both the filesystem and storage provider as mutable external systems. SQLite stores durable coordination evidence, but execution never assumes a previous observation is still current.

Current controls through Phase 6 include:

- no generic frontend filesystem, shell, rclone, or Google Drive API surface,
- local directory access begins from explicit user-selected workspaces,
- symlink traversal is rejected for synchronized/recovery paths,
- built-in generated/secret exclusions plus `.atrisbridgeignore`,
- BLAKE3 local content evidence and separate provider ID/size/checksum evidence,
- explicit synchronized baselines rather than timestamp-based winner rules,
- fresh local/remote inventory before planning and execution,
- review-required transfer/deletion plans,
- conflicts and ambiguous evidence fail closed,
- permanent Google Drive deletion is not exposed,
- remote-deletion → local-delete propagation requires a verified local recovery copy,
- local-deletion → Drive propagation uses provider Trash and the exact reviewed file ID,
- interrupted apply states are never silently accepted as synchronized.

## Credential handling

Google Drive OAuth uses `drive.file`. The rclone-compatible OAuth JSON is held only in the Tauri-managed in-memory provider session store.

AtrisBridge does not write the OAuth token to:

- SQLite,
- repository files,
- `.env`,
- `rclone.conf`,
- provider metadata.

Restarting the application therefore requires reconnecting Google Drive until Phase 7 introduces persistent secure credential storage.

Phase 6's exact-ID Drive Trash request extracts the current access token from that same in-memory OAuth JSON. This creates no additional credential store. If the token is expired/rejected, the operation fails safely; the application does not persist plaintext refresh credentials as an automatic workaround.

Logs/error messages must not include access or refresh tokens. rclone child processes are launched without inherited rclone credential/config environment variables except for the narrowly supplied current session.

## Tauri / command surface

The frontend calls domain-specific Rust commands. It cannot supply arbitrary:

- filesystem paths for generic reads/writes,
- shell commands,
- rclone arguments,
- Drive API endpoints/file IDs for arbitrary provider mutation.

The Phase 6 Drive control-plane code receives a file ID only from the persisted, reviewed provider observation belonging to the plan item.

## Provider mutation rules

All provider writes are restricted to an `AtrisBridge/...` managed workspace root and require a persisted reviewed plan.

### Content writes

Uploads use fresh local fingerprints, targeted remote evidence, and post-transfer verification before the baseline is accepted. AtrisBridge does not claim provider-native atomic compare-and-swap for existing Drive content.

### Drive Trash

A local deletion can move a remote object to Trash only when:

1. the item had a complete synchronized baseline,
2. current remote ID/size/MD5 still match the reviewed evidence,
3. local live preflight still proves the path absent,
4. the provider mutation targets the exact reviewed Drive file ID,
5. postflight does not confuse a replacement object at the same path with the trashed object.

Trash is recoverable; permanent remote delete is intentionally unavailable.

The same Drive object can still be modified by another provider client between separate preflight and Trash requests. That race boundary is documented and never presented as an atomic guarantee.

## Local destructive-looking operations

Remote absence alone is not sufficient permission to delete local content.

Phase 6 requires:

- complete synchronized baseline,
- unchanged local BLAKE3 + size,
- repeated targeted remote-absence checks,
- app-data recovery copy,
- recovery BLAKE3 + size verification,
- file flush before local removal,
- persisted apply state,
- evidence-locked SQLite completion.

Recovery metadata and deletion convergence are committed transactionally. A user can restore a verified recovery copy locally; the restore refuses an occupied/unsafe/ignored destination and does not modify Drive.

## Recovery artifact policy

AtrisBridge `.part` and `.bak` transfer artifacts are excluded by the scanner so crash/recovery mechanics cannot accidentally become synchronized project data.

App-data recovery copies are kept outside the workspace and must canonicalize under AtrisBridge's recovery root before they can be used. They are re-fingerprinted before local restore.

## Public-repository rule

Examples, screenshots, fixtures, tests, logs, and documentation must use synthetic data. Customer/company names and real proprietary content must not be committed as samples unless explicitly approved for public disclosure.

AtrisBridge can reduce accidental leakage risk but does not grant authorization to move company/customer data to Google Drive or any other provider. Organizational policy, DLP, contractual, data-residency, export-control, and authorization requirements still apply.
