# Security Policy

AtrisBridge operates on source-code workspaces and is therefore treated as security-sensitive software.

## Supported versions

AtrisBridge is currently pre-release software. Security fixes are applied to the latest development line only until the first stable release policy is published.

## Reporting a vulnerability

Please do **not** open a public GitHub issue containing exploit details, credentials, customer information, or proprietary project data.

Until a dedicated security contact is published, use GitHub's private vulnerability reporting feature for this repository when available. Include:

- affected version or commit,
- reproduction steps with non-sensitive test data,
- expected and observed behavior,
- impact assessment,
- any suggested mitigation.

## Security principles

- Secrets must not be committed to the repository.
- Workspace removal must never delete project content.
- Synchronization and deletion features must default to recoverable behavior.
- Symlinks are not traversed during local scans.
- Generated output, environment files, and common private-key/certificate formats are excluded by default.
- Future provider credentials must be stored through OS-backed/Stronghold-style secret storage rather than plaintext configuration.
- Future external binaries such as rclone must be invoked with narrow Tauri capabilities; an unauthenticated remote-control server must not be exposed.
- Cloud synchronization must remain an explicit user action until the transport, journal, conflict, and recovery layers are validated together.

## Data classification

AtrisBridge cannot determine whether a user is contractually or legally allowed to upload a workspace. Organizations and users remain responsible for following project-specific data handling requirements.
