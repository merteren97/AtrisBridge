# Release and updater pipeline

AtrisBridge ships Windows x64 and Linux x64 packages from an owner-triggered GitHub Actions release workflow. Normal development and pull-request validation never require updater signing secrets.

## Workflows

- `.github/workflows/ci.yml` keeps the required `Frontend build` and `Rust check` gates and adds native Windows/Linux package smoke builds.
- `.github/workflows/release.yml` is manual, main-only, owner-only, and publishes Windows NSIS/MSI plus Linux AppImage/DEB packages together with the updater-specific signed artifacts.

The release workflow stages the pinned rclone v1.74.4 resource by running the same SHA-256 verified downloader used for development, then verifies the executable before packaging. The binary is never committed.

The CI quality gate runs the full Rust unit-test suite once on Linux. Native Windows/Linux matrix jobs deliberately do not repeat all unit tests; they validate release-mode compilation and build a real representative installer (NSIS on Windows, DEB on Linux). This keeps the cross-platform package gate useful without wasting Actions minutes on duplicate test execution.

## Updater trust boundary

Release builds require:

- `TAURI_SIGNING_PRIVATE_KEY`
- `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` when the private key is password protected
- `TAURI_UPDATER_PUBLIC_KEY`

The private updater signing key exists only in the Actions process environment. A release-only Tauri configuration embeds the public key, enables `createUpdaterArtifacts`, and selects the AtrisHub stable or preview manifest endpoint from the release SemVer tag.

Preview tags (`-alpha.N`, `-beta.N`, `-rc.N`) use the preview channel. Plain `vX.Y.Z` tags use stable. The application checks automatically after startup but never installs an update without an explicit user action.

Tauri's distribution package and updater payload are intentionally distinct on Linux. Users receive the normal `.AppImage` and `.deb` packages, while automatic AppImage updates use Tauri's signed `.AppImage.tar.gz` updater archive. Windows automatic updates use the signed NSIS `-setup.exe` artifact.

## Dynamic AtrisHub endpoint

The published GitHub Release also contains a signed static `latest.json` source manifest. AtrisHub reads that manifest only after selecting the eligible preview/stable release and returns the selected platform as Tauri's dynamic-server payload:

- `version`
- optional `notes`
- optional `pub_date`
- `url`
- `signature`

When no newer eligible release exists, the endpoint returns HTTP 204. Package URLs are constrained to `merteren97/AtrisBridge` GitHub Release assets; AtrisHub never becomes the binary download host.

## Reproducible dependency graph

`package-lock.json` and `src-tauri/Cargo.lock` are committed. CI and release use `npm ci` and Cargo `--locked` validation. The manual release version helper updates the root package, npm lockfile root package, Tauri config, Cargo package, and Cargo lockfile package version together before packaging.

## Windows publisher signing

Updater signing and Windows Authenticode signing are separate. If `WINDOWS_CERTIFICATE` and `WINDOWS_CERTIFICATE_PASSWORD` are configured, the release runner imports the base64 PFX into the Current User certificate store and injects its thumbprint into the ephemeral Tauri release config. If they are absent, Windows packages are still updater-signed but Windows may show SmartScreen publisher warnings.

## Release contract

Each GitHub Release contains installer packages, updater artifacts/signatures, and `latest.json`. AtrisHub's desktop release endpoint selects an eligible stable/preview GitHub Release and returns only the matching dynamic Tauri response. This keeps package bytes on GitHub while allowing channel policy, kill switches, and staged rollout to remain server-side later.
