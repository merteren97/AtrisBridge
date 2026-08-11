# Release and updater pipeline

AtrisBridge ships Windows x64 and Linux x64 packages from an owner-triggered GitHub Actions release workflow. Normal development and pull-request validation never require updater signing secrets.

## Workflows

- `.github/workflows/ci.yml` keeps the required `Frontend build` and `Rust check` gates and adds native Windows/Linux package smoke builds.
- `.github/workflows/release.yml` is manual, main-only, owner-only, and publishes Windows NSIS/MSI plus Linux AppImage/DEB artifacts.

The release workflow stages the pinned rclone v1.74.4 resource by running the same SHA-256 verified downloader used for development, then verifies the executable before packaging. The binary is never committed.

## Updater trust boundary

Release builds require:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` when the private key is password protected
- `TAURI_UPDATER_PUBLIC_KEY`

The private updater signing key exists only in the Actions process environment. A release-only Tauri configuration embeds the public key, enables `createUpdaterArtifacts`, and selects the AtrisHub stable or preview manifest endpoint from the release SemVer tag.

Preview tags (`-alpha.N`, `-beta.N`, `-rc.N`) use the preview channel. Plain `vX.Y.Z` tags use stable. The application checks automatically after startup but never installs an update without an explicit user action.

## Windows publisher signing

Updater signing and Windows Authenticode signing are separate. If `WINDOWS_CERTIFICATE` and `WINDOWS_CERTIFICATE_PASSWORD` are configured, the release runner imports the base64 PFX into the Current User certificate store and injects its thumbprint into the ephemeral Tauri release config. If they are absent, Windows packages are still updater-signed but Windows may show SmartScreen publisher warnings.

## Release contract

Each GitHub Release contains installer packages, Tauri updater signatures, and `latest.json`. AtrisHub's desktop release endpoint selects an eligible stable/preview GitHub Release and returns that release manifest. This keeps package bytes on GitHub while allowing channel policy, kill switches, and staged rollout to remain server-side later.
