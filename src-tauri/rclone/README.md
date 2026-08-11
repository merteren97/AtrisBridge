# Packaged rclone resource

Release and native package workflows stage the SHA-256 verified, pinned rclone v1.74.4 executable into this directory before Tauri bundles the application.

The executable is intentionally ignored by Git and must never be committed. Runtime code resolves it from `$RESOURCE/rclone/rclone(.exe)` in packaged builds.
