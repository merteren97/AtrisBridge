# AtrisHub desktop account

AtrisBridge integrates AtrisHub identity without weakening its local-first data boundary.

## Credential boundary

The React webview sends an email/username and password to one Tauri IPC login command. Rust sends those credentials directly over HTTPS to the dedicated AtrisHub desktop-session endpoint. The password is never written to SQLite, application logs, localStorage, environment files, or the OS credential vault.

AtrisHub issues a short-lived access token and a rotating refresh credential. Access tokens remain process-memory only. When `Remember this device` is enabled, only the refresh credential is stored under `com.atrishub.atrisbridge / auth.atrishub.refresh` in the operating system credential store. When remembering is disabled, the refresh credential remains memory-only and disappears when AtrisBridge exits.

The frontend receives only a non-secret user/membership snapshot; access and refresh credentials are never returned from Rust IPC.

## Startup restore

On startup AtrisBridge reads a remembered refresh credential, calls the AtrisHub refresh endpoint, receives a rotated credential, and replaces the secure-vault value before exposing the refreshed profile to the UI. Authentication errors revoke local remembered state. Network or AtrisHub 5xx failures keep the remembered credential and display the cached non-secret identity as offline rather than deleting a valid device session.

## Local-first behavior

AtrisHub identity is not a filesystem authorization gate. If AtrisHub is unavailable, local workspace scanning, journals, recovery, and explicitly configured synchronization safety state remain available. A signed-out user may choose `Continue with local AtrisBridge`. This prevents a server outage from locking a user out of their own project data.

## Device identity

AtrisBridge stores one random UUID in the local `app_meta` table and uses it as the stable desktop device identifier. It contains no hardware fingerprint and no secret. Reinstalling/removing app data creates a new device identity.
