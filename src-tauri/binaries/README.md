# AtrisBridge rclone sidecar

AtrisBridge does not execute an arbitrary `rclone` from the system `PATH`.

For local development, run:

```bash
npm run sidecar:prepare
```

The preparation script downloads the pinned official rclone release, verifies its SHA-256 checksum against the release checksum, and writes only the executable into this directory. The executable is intentionally ignored by Git and must never be committed.

Phase 3 accepts exactly rclone **v1.74.4** and uses only a fixed read-only command allowlist for provider authorization, account verification, quota checks, and remote inventory. Transfer/delete commands are not exposed yet.

Release packaging will copy the verified executable into the application resource directory in a later release phase.
