import { useEffect, useState } from "react";
import {
  ArrowLeftRight,
  ArrowUpFromLine,
  CheckCircle2,
  FileWarning,
  ListChecks,
  RefreshCw,
  ShieldAlert,
  ShieldCheck,
} from "lucide-react";
import {
  executeBackupPlan,
  getLatestBackupPlan,
  prepareBackupPlan,
  setWorkspaceSyncMode,
} from "./lib/bridge";
import RestorePanel from "./RestorePanel";
import SyncPanel from "./SyncPanel";
import type { BackupPlan, Workspace } from "./types";

interface BackupPanelProps {
  workspace: Workspace;
  ready: boolean;
  onChanged: () => Promise<void> | void;
  onError: (message: string) => void;
}

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** index).toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

function formatDate(value: string): string {
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

function statusLabel(plan: BackupPlan): string {
  switch (plan.status) {
    case "ready":
      return "Ready for review";
    case "running":
      return "Running";
    case "completed":
      return "Completed";
    case "partial":
      return "Completed with blocks";
    case "failed":
      return "Failed";
    case "cancelled":
      return "Superseded";
  }
}

export default function BackupPanel({
  workspace,
  ready,
  onChanged,
  onError,
}: BackupPanelProps) {
  const [plan, setPlan] = useState<BackupPlan | null>(null);
  const [busy, setBusy] = useState<"load" | "prepare" | "execute" | "mode" | null>(null);
  const [restoreBusy, setRestoreBusy] = useState(false);

  useEffect(() => {
    if (workspace.syncMode !== "two_way") void loadLatest();
  }, [workspace.id, workspace.syncMode]);

  async function loadLatest() {
    try {
      setBusy((current) => current ?? "load");
      setPlan(await getLatestBackupPlan(workspace.id));
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy((current) => (current === "load" ? null : current));
    }
  }

  async function handlePrepare() {
    try {
      setBusy("prepare");
      const next = await prepareBackupPlan(workspace.id);
      setPlan(next);
      await onChanged();
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleExecute() {
    if (!plan || plan.status !== "ready" || plan.uploadCount === 0) return;
    const confirmed = window.confirm(
      `Upload ${plan.uploadCount.toLocaleString()} file${plan.uploadCount === 1 ? "" : "s"} (${formatBytes(plan.uploadBytes)}) to ${plan.remotePath}?\n\nAtrisBridge will not delete or download any remote files. Blocked/conflicting paths will remain untouched.`,
    );
    if (!confirmed) return;

    try {
      setBusy("execute");
      await executeBackupPlan(plan.id);
      setPlan(await getLatestBackupPlan(workspace.id));
      await onChanged();
    } catch (error) {
      onError(String(error));
      await loadLatest();
    } finally {
      setBusy(null);
    }
  }

  async function handleEnableTwoWay() {
    const confirmed = window.confirm(
      "Enable conflict-aware Two-Way mode for this workspace?\n\nNothing synchronizes immediately. AtrisBridge will require a fresh Prepare sync → review → Run sync flow. Local/remote changes are compared against synchronized baselines and conflicts remain blocked.",
    );
    if (!confirmed) return;
    try {
      setBusy("mode");
      await setWorkspaceSyncMode(workspace.id, "two_way");
      await onChanged();
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  if (workspace.syncMode === "two_way") {
    return (
      <SyncPanel
        workspace={workspace}
        ready={ready}
        onChanged={onChanged}
        onError={onError}
      />
    );
  }

  const running = busy !== null || restoreBusy;
  const visibleItems = plan?.items.slice(0, 8) ?? [];

  return (
    <>
      <section className="backup-plan-card">
        <div className="backup-plan-header">
          <div>
            <div className="backup-plan-title">
              <ListChecks size={17} />
              <strong>Safe backup plan</strong>
            </div>
            <p>
              Prepare creates a fresh local + remote evidence snapshot. Uploads only run after a
              separate review step.
            </p>
          </div>
          <div className="backup-plan-actions">
            <button
              className="secondary-button"
              onClick={handleEnableTwoWay}
              disabled={!ready || running}
              title="Switch this workspace to conflict-aware two-way synchronization"
            >
              {busy === "mode" ? (
                <RefreshCw className="spin" size={14} />
              ) : (
                <ArrowLeftRight size={14} />
              )}
              Two-Way mode
            </button>
            <button
              className="secondary-button"
              onClick={handlePrepare}
              disabled={!ready || running || workspace.syncMode !== "backup"}
            >
              {busy === "prepare" ? (
                <RefreshCw className="spin" size={14} />
              ) : (
                <ListChecks size={14} />
              )}
              Prepare plan
            </button>
            <button
              className="primary-button"
              onClick={handleExecute}
              disabled={
                !ready ||
                running ||
                workspace.syncMode !== "backup" ||
                plan?.status !== "ready" ||
                plan.uploadCount === 0
              }
            >
              {busy === "execute" ? (
                <RefreshCw className="spin" size={14} />
              ) : (
                <ArrowUpFromLine size={14} />
              )}
              Run backup
            </button>
          </div>
        </div>

        {!ready ? (
          <div className="backup-plan-empty">
            <ShieldAlert size={18} />
            <div>
              <strong>Provider preconditions are not ready</strong>
              <span>Connect Google Drive and bind a dedicated workspace folder first.</span>
            </div>
          </div>
        ) : !plan ? (
          <div className="backup-plan-empty">
            <ShieldCheck size={18} />
            <div>
              <strong>No backup plan prepared</strong>
              <span>Preparing a plan refreshes both inventories; it does not upload anything.</span>
            </div>
          </div>
        ) : (
          <>
            <div className="backup-plan-metrics">
              <div>
                <small>Status</small>
                <strong>{statusLabel(plan)}</strong>
                <span>{formatDate(plan.createdAt)}</span>
              </div>
              <div>
                <small>Safe uploads</small>
                <strong>{plan.uploadCount.toLocaleString()}</strong>
                <span>{formatBytes(plan.uploadBytes)}</span>
              </div>
              <div className={plan.blockedCount > 0 ? "blocked" : undefined}>
                <small>Blocked</small>
                <strong>{plan.blockedCount.toLocaleString()}</strong>
                <span>Never overwritten automatically</span>
              </div>
              <div>
                <small>Completed</small>
                <strong>{plan.completedCount.toLocaleString()}</strong>
                <span>{plan.failedCount.toLocaleString()} failed</span>
              </div>
            </div>

            {visibleItems.length > 0 && (
              <div className="backup-plan-items">
                {visibleItems.map((item) => (
                  <div key={item.id} className={`backup-plan-item ${item.action}`}>
                    <span className="backup-plan-item-icon">
                      {item.action === "blocked" ? (
                        <FileWarning size={14} />
                      ) : item.status === "completed" ? (
                        <CheckCircle2 size={14} />
                      ) : (
                        <ArrowUpFromLine size={14} />
                      )}
                    </span>
                    <div>
                      <strong>{item.relativePath}</strong>
                      <span>
                        {item.lastError ??
                          item.blockReason ??
                          `${item.action} · ${
                            item.size === null ? "unknown size" : formatBytes(item.size)
                          }`}
                      </span>
                    </div>
                    <small>{item.status}</small>
                  </div>
                ))}
                {(plan.previewTruncated || plan.items.length > visibleItems.length) && (
                  <p className="backup-plan-note">
                    Showing the first safety-relevant plan entries.
                  </p>
                )}
              </div>
            )}
          </>
        )}

        <div className="backup-safety-strip">
          <ShieldCheck size={13} />
          Backup mode remains local → Drive only. Enable Two-Way mode explicitly to plan downloads,
          recoverable deletion propagation, and conflict-aware convergence.
        </div>
      </section>

      <RestorePanel
        workspace={workspace}
        ready={ready}
        disabled={busy !== null}
        onBusyChange={setRestoreBusy}
        onChanged={onChanged}
        onError={onError}
      />
    </>
  );
}
