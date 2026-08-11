import { useEffect, useState } from "react";
import {
  Activity,
  CirclePause,
  Play,
  RefreshCw,
  ShieldAlert,
  ShieldCheck,
  TimerReset,
  Waves,
} from "lucide-react";
import {
  getContinuousSyncStatus,
  runContinuousSyncNow,
  setContinuousSyncEnabled,
  updateContinuousSyncSettings,
} from "./lib/bridge";
import type { ContinuousSyncStatus } from "./types";
import "./continuous.css";

interface ContinuousSyncPanelProps {
  workspaceId: string;
  ready: boolean;
  onStatusChange: (status: ContinuousSyncStatus) => void;
  onChanged: () => Promise<void> | void;
  onError: (message: string) => void;
}

const POLL_OPTIONS = [30, 60, 120, 300, 600];

function formatDate(value: string | null): string {
  if (!value) return "Never";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

function stateCopy(status: ContinuousSyncStatus | null) {
  if (!status) return { label: "Loading", tone: "neutral" };
  switch (status.state) {
    case "disabled":
      return { label: "Paused", tone: "neutral" };
    case "idle":
      return { label: status.runtimeActive ? "Watching" : "Reconciliation only", tone: "safe" };
    case "debouncing":
      return { label: "Settling changes", tone: "working" };
    case "running":
      return { label: "Reconciling", tone: "working" };
    case "attention":
      return { label: "Attention required", tone: "attention" };
    case "error":
      return { label: "Retrying safely", tone: "danger" };
  }
}

export default function ContinuousSyncPanel({
  workspaceId,
  ready,
  onStatusChange,
  onChanged,
  onError,
}: ContinuousSyncPanelProps) {
  const [status, setStatus] = useState<ContinuousSyncStatus | null>(null);
  const [busy, setBusy] = useState<"toggle" | "settings" | "run" | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function load() {
      try {
        const next = await getContinuousSyncStatus(workspaceId);
        if (!cancelled) accept(next);
      } catch (error) {
        if (!cancelled) onError(String(error));
      }
    }

    void load();
    const timer = window.setInterval(() => {
      void load();
    }, 2500);

    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [workspaceId]);

  function accept(next: ContinuousSyncStatus) {
    setStatus(next);
    onStatusChange(next);
  }

  async function handleToggle() {
    if (!status) return;
    if (!status.enabled) {
      const confirmed = window.confirm(
        "Enable continuous watch mode for this workspace?\n\nAtrisBridge will debounce local file events, periodically re-check Google Drive, rebuild full local + remote evidence, and use the existing guarded planner. Safe transfers run automatically only if you separately enable Auto-apply safe transfers below. Conflicts, blocked paths, and every deletion always require manual review.",
      );
      if (!confirmed) return;
    }

    try {
      setBusy("toggle");
      const next = await setContinuousSyncEnabled(workspaceId, !status.enabled);
      accept(next);
      await onChanged();
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleAutoApplyChange(nextValue: boolean) {
    if (!status) return;
    if (nextValue) {
      const confirmed = window.confirm(
        "Allow automatic application of safe transfer-only plans?\n\nAtrisBridge still performs full evidence refresh and planner validation first. Any conflict, blocked path, or deletion prevents automatic execution.",
      );
      if (!confirmed) return;
    }
    try {
      setBusy("settings");
      const next = await updateContinuousSyncSettings(
        workspaceId,
        nextValue,
        status.remotePollSeconds,
      );
      accept(next);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handlePollChange(value: number) {
    if (!status) return;
    try {
      setBusy("settings");
      const next = await updateContinuousSyncSettings(
        workspaceId,
        status.autoApplySafe,
        value,
      );
      accept(next);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleRunNow() {
    try {
      setBusy("run");
      const next = await runContinuousSyncNow(workspaceId);
      accept(next);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  const state = stateCopy(status);
  const enabled = Boolean(status?.enabled);
  const attention = status?.state === "attention";
  const reconciliationRunning = status?.state === "running";

  return (
    <section className={`continuous-card ${enabled ? "enabled" : ""} ${attention ? "attention" : ""}`}>
      <div className="continuous-header">
        <div className="continuous-title">
          <span className="continuous-icon"><Waves size={17} /></span>
          <div>
            <small>Phase 8 automation</small>
            <strong>Continuous watch mode</strong>
            <span>Filesystem events trigger a guarded reconciliation, never direct synchronization.</span>
          </div>
        </div>
        <span className={`continuous-state ${state.tone}`}>
          {status?.state === "running" || status?.state === "debouncing"
            ? <RefreshCw className="spin" size={12} />
            : attention
              ? <ShieldAlert size={12} />
              : <Activity size={12} />}
          {state.label}
        </span>
      </div>

      <div className="continuous-body">
        <div className="continuous-facts">
          <div>
            <small>Local watcher</small>
            <strong>{enabled ? (status?.runtimeActive ? "Native watcher active" : "Unavailable") : "Paused"}</strong>
            <span>1.8 s event settling window</span>
          </div>
          <div>
            <small>Remote reconciliation</small>
            <strong>{status ? `Every ${status.remotePollSeconds}s` : "—"}</strong>
            <span>Finds Drive-side changes while local files are quiet</span>
          </div>
          <div>
            <small>Last check</small>
            <strong>{formatDate(status?.lastCycleCompletedAt ?? null)}</strong>
            <span>Last success {formatDate(status?.lastSuccessAt ?? null)}</span>
          </div>
          <div>
            <small>Automatic policy</small>
            <strong>{status?.autoApplySafe ? "Safe transfers only" : "Review all plans"}</strong>
            <span>Conflicts and deletions always require review</span>
          </div>
        </div>

        {status?.lastMessage && (
          <div className={`continuous-message ${attention || status.state === "error" ? "warning" : ""}`}>
            {attention || status.state === "error" ? <ShieldAlert size={15} /> : <ShieldCheck size={15} />}
            <div>
              <strong>{attention ? "Manual review required" : status.state === "error" ? "Cycle failed closed" : "Watch status"}</strong>
              <span>{status.lastMessage}</span>
            </div>
          </div>
        )}

        <div className="continuous-controls">
          <label className="continuous-setting">
            <span>
              <strong>Auto-apply safe transfers</strong>
              <small>Only upload/download plans with zero conflicts, blocks, and deletions.</small>
            </span>
            <input
              type="checkbox"
              checked={status?.autoApplySafe ?? false}
              onChange={(event) => void handleAutoApplyChange(event.target.checked)}
              disabled={!status || busy !== null}
            />
          </label>

          <label className="continuous-poll-setting">
            <TimerReset size={14} />
            <span>Drive check</span>
            <select
              value={status?.remotePollSeconds ?? 60}
              onChange={(event) => void handlePollChange(Number(event.target.value))}
              disabled={!status || busy !== null}
            >
              {POLL_OPTIONS.map((seconds) => (
                <option key={seconds} value={seconds}>
                  {seconds < 60 ? `${seconds} sec` : `${seconds / 60} min`}
                </option>
              ))}
            </select>
          </label>

          <div className="continuous-actions">
            {enabled && (
              <button
                className="secondary-button"
                onClick={handleRunNow}
                disabled={busy !== null || reconciliationRunning || status?.state === "debouncing"}
              >
                {busy === "run" ? <RefreshCw className="spin" size={14} /> : <RefreshCw size={14} />}
                Check now
              </button>
            )}
            <button
              className={enabled ? "secondary-button" : "primary-button"}
              onClick={handleToggle}
              disabled={!status || busy !== null || reconciliationRunning || (!enabled && !ready)}
              title={reconciliationRunning ? "Wait for the current reconciliation to finish before pausing" : undefined}
            >
              {busy === "toggle"
                ? <RefreshCw className="spin" size={14} />
                : enabled
                  ? <CirclePause size={14} />
                  : <Play size={14} />}
              {enabled ? "Pause watch" : "Enable watch"}
            </button>
          </div>
        </div>
      </div>

      <div className="continuous-safety-strip">
        <ShieldCheck size={13} />
        Native watcher events are hints only. Every cycle re-scans the workspace, re-reads provider evidence, then uses the existing evidence-locked planner/executor. Phase 8 never auto-applies deletion actions.
      </div>
    </section>
  );
}
