# Phase 8 continuous watch mode

## Goal

Continuous watch mode reduces repeated manual scanning without replacing AtrisBridge's evidence model. Filesystem notifications and provider polling only decide **when to reconcile**. They never decide which copy wins and they never mutate a project or provider object directly.

## Local watcher boundary

AtrisBridge uses the native `notify` watcher recursively at the workspace root.

A watcher event is treated as a dirty signal only:

1. ignored paths and pure access events are discarded when possible,
2. relevant event bursts are coalesced behind a 1.8 second settling window,
3. the scheduler runs the normal full workspace scanner,
4. the scanner rebuilds BLAKE3 local evidence,
5. scan warnings fail closed before the continuous cycle is allowed to plan from that scan.

This avoids interpreting editor temporary-file/rename sequences as authoritative synchronization operations.

The scanner remains the source of local inventory truth and retains all existing built-in ignore, custom `.atrisbridgeignore`, regular-file, and symlink rules.

## Remote reconciliation

A local filesystem watcher cannot observe edits made by another computer or directly in Google Drive. Enabled workspaces therefore also run a bounded provider reconciliation poll.

- default: 60 seconds,
- configurable: 30 seconds through 60 minutes,
- real attention-required workspaces stop repeated equivalent provider-plan churn until evidence or user action changes,
- repeated provider/runtime errors use a bounded exponential retry delay,
- a successful cycle resets the failure count.

Every provider reconciliation uses the same restricted transport and encryption evidence rules as manual planning.

## Scheduler ownership

Only one automatic cycle may be in flight for a workspace at a time. New local events are coalesced and queued rather than starting overlapping transfers.

While watch mode is enabled, the Tauri command boundary rejects manual mutations that could race scheduler ownership, including:

- provider binding changes,
- sync-mode changes,
- encryption enablement or recovery-key import,
- local/remote scans that rewrite evidence,
- backup/restore/two-way prepare or execute commands,
- recovery-copy restore,
- provider disconnect/forget while that provider has watched workspaces,
- workspace removal.

Read-only status and recovery-key export remain available. Pause watch mode before switching back to manual reviewed controls. This rule is enforced in Rust rather than relying only on disabled frontend buttons.

## Automatic apply policy

`Auto-apply safe transfers` is a separate user-controlled option and defaults off. Even when enabled, it does not bypass the planner.

When auto-apply is disabled, a transfer-only plan is recorded as a **review** decision. This is intentionally different from a destructive/ambiguous `attention` decision: unchanged safe evidence may be evaluated again if the user later opts into automatic safe transfers.

### Backup

Automatic execution is allowed only when:

- the planner reports at least one upload,
- there are zero blocked paths.

### Pull

Automatic execution is allowed only when:

- the planner reports at least one download,
- there are zero blocked paths.

### Two-Way

Automatic execution is allowed only when:

- there is at least one upload or download,
- there are zero conflicts,
- there are zero blocked paths,
- there are **zero deletion actions**.

## Deletion rule

Phase 8 never automatically applies a deletion, including provider Trash or recoverable local deletion.

A plan containing any deletion action moves the workspace into `attention` state. The user must pause watch mode and use the existing reviewed Phase 6 flow. The exact-ID remote Trash and local recovery-copy protections remain unchanged.

This is intentionally more conservative than treating deletion as an ordinary synchronization event.

## Conflict and uncertain evidence

The following conditions stop automatic application and surface attention/error state:

- planner conflicts,
- blocked paths,
- any deletion action,
- scanner warnings/incomplete local evidence,
- unavailable provider credentials,
- missing encryption recovery key,
- encrypted sentinel/namespace inconsistency,
- transport or provider evidence failure,
- a transfer that does not complete cleanly.

AtrisBridge does not choose a newer file by modification time and does not retry ambiguous destructive outcomes blindly.

## Durable state and restart behavior

Per-workspace watch settings and the latest cycle state are stored in SQLite. They include:

- enabled/paused state,
- safe auto-apply preference,
- provider poll interval,
- latest dirty reason/event time,
- latest cycle start/completion/success times,
- current state/message,
- consecutive failure count,
- an evidence signature used to avoid repeatedly creating equivalent attention/no-op plans.

Review-only safe transfer outcomes are not suppressed as destructive attention so a later policy change can re-evaluate the same evidence.

On application startup AtrisBridge first runs the existing interrupted-transfer recovery routines. Only after those routines complete does Phase 8 resume configured watchers.

## State model

The UI exposes these states:

- `disabled` — watch mode paused,
- `idle` — waiting/reconciled,
- `debouncing` — local change burst is settling,
- `running` — full evidence refresh/planning/execution in progress,
- `attention` — manual review is required; the internal cycle decision distinguishes safe `review` from destructive/ambiguous `attention`,
- `error` — cycle failed closed; retry uses bounded backoff.

## Security inheritance

Phase 8 does not introduce a new provider authority or credential path.

- OAuth credentials remain in the OS-native secure credential vault.
- Encrypted workspaces still require their protected recovery key.
- rclone remains a restricted byte transport.
- filename visibility for encrypted workspaces is unchanged.
- local BLAKE3 and remote provider/ciphertext evidence remain type-separated.
- existing planner/executor preflight and evidence-locked journal completion remain authoritative.
- every deletion remains manual.
