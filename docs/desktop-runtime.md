# Desktop runtime UX

AtrisBridge is designed to keep conservative continuous synchronization available even when its main window is not in the foreground.

## System tray lifecycle

On desktop builds AtrisBridge creates a Tauri system tray entry with these actions:

- **Open AtrisBridge** restores, shows, and focuses the main window.
- **Hide to tray** hides the main window without stopping the process.
- **Quit AtrisBridge** explicitly exits the application.

Closing the main window is treated as **hide to tray**, not process termination. This keeps configured continuous-watch workers alive. The application only exits through the explicit tray quit action or an operating-system termination path.

The tray uses Tauri's built-in `tray-icon` feature and the packaged application icon. No separate tray dependency or background service is introduced.

## Activity Center

The frontend exposes a compact global Activity Center independent of the currently selected workspace. It refreshes durable journal summaries and continuous-sync runtime status without creating a second synchronization authority.

It surfaces:

- number of active/debouncing cycles,
- pending journal operations,
- conflicts and runtime states requiring attention,
- per-workspace watcher state and latest runtime message,
- indeterminate activity for running/debouncing work.

AtrisBridge intentionally does not fabricate transfer percentages when the backend does not expose byte-level progress for a continuous cycle.

## Alerts

Activity transitions always produce an in-app toast. Users may opt in to desktop alerts from the Activity Center. When the embedded webview supports the Web Notification API and permission is granted, AtrisBridge also emits a system notification while the window is hidden/backgrounded.

Notification preference is non-sensitive UI state stored in local browser storage. Sync credentials, AtrisHub credentials, encryption keys, and provider tokens are not involved.

Warning alerts are raised when a workspace newly enters `attention` or `error`. A success alert is raised when an observed running/debouncing cycle returns to idle with a new `lastSuccessAt` value. Duplicate transition keys are suppressed for the lifetime of the frontend session.

If system notification permission is unavailable or denied, the synchronization engine is unaffected and in-app alerts remain available.

## Safety boundary

The Activity Center is observational. It does not mutate transfer plans, bypass conflict review, apply deletions, or relax Phase 8 ownership guards. Continuous synchronization continues to use the existing planner/executor and fail-closed policy documented in [continuous-watch.md](continuous-watch.md).
