import { useEffect, useState } from "react";
import {
  ArrowDownToLine,
  ArrowLeftRight,
  ArrowUpFromLine,
  CheckCircle2,
  FileWarning,
  RefreshCw,
  RotateCcw,
  ShieldAlert,
  ShieldCheck,
  Trash2,
  TriangleAlert,
} from "lucide-react";
import {
  executeSyncPlan,
  getLatestSyncPlan,
  listSyncRecoveries,
  prepareSyncPlan,
  restoreSyncRecovery,
  setWorkspaceSyncMode,
} from "./lib/bridge";
import type {
  SyncPlan,
  SyncPlanItemAction,
  SyncRecoveryEntry,
  Workspace,
} from "./types";

interface SyncPanelProps {
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

function statusLabel(plan: SyncPlan): string {
  switch (plan.status) {
    case "ready":
      return "Ready for review";
    case "running":
      return "Running";
    case "completed":
      return "Converged";
    case "partial":
      return "Needs attention";
    case "failed":
      return "Failed";
    case "cancelled":
      return "Superseded";
  }
}

function actionLabel(action: SyncPlanItemAction): string {
  switch (action) {
    case "upload_create":
      return "upload new";
    case "upload_update":
      return "upload update";
    case "download_create":
      return "download new";
    case "download_update":
      return "download update";
    case "remote_trash":
      return "Drive → Trash";
    case "local_delete":
      return "recoverable local delete";
    case "acknowledge_delete":
      return "confirm converged delete";
    case "conflict":
      return "conflict";
    case "blocked":
      return "blocked";
  }
}

function ActionIcon({ action }: { action: SyncPlanItemAction }) {
  switch (action) {
    case "upload_create":
    case "upload_update":
      return <ArrowUpFromLine size={14} />;
    case "download_create":
    case "download_update":
      return <ArrowDownToLine size={14} />;
    case "remote_trash":
    case "local_delete":
      return <Trash2 size={14} />;
    case "acknowledge_delete":
      return <CheckCircle2 size={14} />;
    case "conflict":
      return <TriangleAlert size={14} />;
    case "blocked":
      return <FileWarning size={14} />;
  }
}

export default function SyncPanel({ workspace, ready, onChanged, onError }: SyncPanelProps) {
  const [plan, setPlan] = useState<SyncPlan | null>(null);
  const [recoveries, setRecoveries] = useState<SyncRecoveryEntry[]>([]);
  const [busy, setBusy] = useState<"load" | "prepare" | "execute" | "mode" | null>(null);
  const [recoveryBusy, setRecoveryBusy] = useState<string | null>(null);

  useEffect(() => {
    void loadLatest();
  }, [workspace.id]);

  async function loadLatest() {
    try {
      setBusy((current) => current ?? "load");
      const [latestPlan, latestRecoveries] = await Promise.all([
        getLatestSyncPlan(workspace.id),
        listSyncRecoveries(workspace.id),
      ]);
      setPlan(latestPlan);
      setRecoveries(latestRecoveries);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy((current) => (current === "load" ? null : current));
    }
  }

  async function handlePrepare() {
    try {
      setBusy("prepare");
      setPlan(await prepareSyncPlan(workspace.id));
      await onChanged();
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleExecute() {
    if (!plan || plan.status !== "ready") return;
    if (plan.uploadCount + plan.downloadCount + plan.deleteCount === 0) return;

    const confirmed = window.confirm(
      `Run reviewed two-way synchronization for ${workspace.name}?\n\n` +
        `${plan.uploadCount.toLocaleString()} upload(s) · ` +
        `${plan.downloadCount.toLocaleString()} download(s) · ` +
        `${plan.deleteCount.toLocaleString()} deletion/convergence action(s)\n` +
        `${plan.conflictCount.toLocaleString()} conflict(s) · ` +
        `${plan.blockedCount.toLocaleString()} blocked\n\n` +
        "Local deletions move the reviewed Google Drive file ID to Trash only when provider evidence still matches the synchronized baseline. Remote deletions remove a local file only after AtrisBridge creates and verifies a recovery copy. Conflicts remain untouched.",
    );
    if (!confirmed) return;

    try {
      setBusy("execute");
      await executeSyncPlan(plan.id);
      const [latestPlan, latestRecoveries] = await Promise.all([
        getLatestSyncPlan(workspace.id),
        listSyncRecoveries(workspace.id),
      ]);
      setPlan(latestPlan);
      setRecoveries(latestRecoveries);
      await onChanged();
    } catch (error) {
      onError(String(error));
      await loadLatest();
    } finally {
      setBusy(null);
    }
  }

  async function handleRestoreRecovery(entry: SyncRecoveryEntry) {
    const confirmed = window.confirm(
      `Restore ${entry.relativePath} from AtrisBridge's verified local recovery copy?\n\n` +
        "This is a local-only recovery action. It will not overwrite an existing local path and will not modify Google Drive. The recovered file becomes a local-only change for the next reviewed sync plan.",
    );
    if (!confirmed) return;

    try {
      setRecoveryBusy(entry.id);
      await restoreSyncRecovery(entry.id);
      setRecoveries(await listSyncRecoveries(workspace.id));
      await onChanged();
    } catch (error) {
      onError(String(error));
    } finally {
      setRecoveryBusy(null);
    }
  }

  async function handleReturnToBackup() {
    if (
      !window.confirm(
        "Switch this workspace back to Backup mode? Existing synchronized baselines and local recovery copies are preserved, but no two-way plan will run until Two-Way mode is enabled again.",
      )
    ) {
      return;
    }
    try {
      setBusy("mode");
      await setWorkspaceSyncMode(workspace.id, "backup");
      await onChanged();
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  const visibleItems = plan?.items.slice(0, 12) ?? [];
  const executable = Boolean(
    plan?.status === "ready" &&
      plan.uploadCount + plan.downloadCount + plan.deleteCount > 0,
  );
  const availableRecoveries = recoveries.filter((entry) => entry.restoredAt === null).slice(0, 6);

  return (
    <section className="backup-plan-card">
      <div className="backup-plan-header">
        <div>
          <div className="backup-plan-title">
            <ArrowLeftRight size={17} />
            <strong>Conflict-aware two-way plan</strong>
          </div>
          <p>
            Phase 6 compares local and Drive evidence against the last synchronized baseline. It
            never resolves modify/modify or delete/modify conflicts with last-write-wins.
          </p>
        </div>
        <div className="backup-plan-actions">
          <button
            className="secondary-button"
            onClick={handleReturnToBackup}
            disabled={busy !== null || recoveryBusy !== null}
          >
            <ShieldCheck size={14} /> Backup mode
          </button>
          <button
            className="secondary-button"
            onClick={handlePrepare}
            disabled={!ready || busy !== null || recoveryBusy !== null}
          >
            {busy === "prepare" ? (
              <RefreshCw className="spin" size={14} />
            ) : (
              <ArrowLeftRight size={14} />
            )}
            Prepare sync
          </button>
          <button
            className="primary-button"
            onClick={handleExecute}
            disabled={!ready || busy !== null || recoveryBusy !== null || !executable}
          >
            {busy === "execute" ? (
              <RefreshCw className="spin" size={14} />
            ) : (
              <ArrowLeftRight size={14} />
            )}
            Run sync
          </button>
        </div>
      </div>

      {!ready ? (
        <div className="backup-plan-empty">
          <ShieldAlert size={18} />
          <div>
            <strong>Provider preconditions are not ready</strong>
            <span>Reconnect Google Drive and bind an AtrisBridge-managed workspace folder.</span>
          </div>
        </div>
      ) : !plan ? (
        <div className="backup-plan-empty">
          <ShieldCheck size={18} />
          <div>
            <strong>No two-way plan prepared</strong>
            <span>Prepare refreshes both inventories and changes no local or remote file.</span>
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
              <small>Transfers</small>
              <strong>
                {plan.uploadCount.toLocaleString()} ↑ · {plan.downloadCount.toLocaleString()} ↓
              </strong>
              <span>{formatBytes(plan.transferBytes)}</span>
            </div>
            <div className={plan.deleteCount > 0 ? "blocked" : undefined}>
              <small>Deletion convergence</small>
              <strong>{plan.deleteCount.toLocaleString()}</strong>
              <span>Drive Trash / local recovery</span>
            </div>
            <div className={plan.conflictCount + plan.blockedCount > 0 ? "blocked" : undefined}>
              <small>Needs attention</small>
              <strong>{plan.conflictCount.toLocaleString()} conflicts</strong>
              <span>{plan.blockedCount.toLocaleString()} blocked</span>
            </div>
          </div>

          {visibleItems.length > 0 && (
            <div className="backup-plan-items">
              {visibleItems.map((item) => (
                <div key={item.id} className={`backup-plan-item ${item.action}`}>
                  <span className="backup-plan-item-icon">
                    <ActionIcon action={item.action} />
                  </span>
                  <div>
                    <strong>{item.relativePath}</strong>
                    <span>
                      {item.lastError ??
                        item.reason ??
                        `${actionLabel(item.action)} · ${
                          item.size === null ? "no payload" : formatBytes(item.size)
                        }`}
                    </span>
                  </div>
                  <small>{item.status}</small>
                </div>
              ))}
              {(plan.previewTruncated || plan.items.length > visibleItems.length) && (
                <p className="backup-plan-note">
                  Showing the first safety-relevant Phase 6 entries. Execution eligibility uses the
                  complete persisted plan, not this preview.
                </p>
              )}
            </div>
          )}
        </>
      )}

      {availableRecoveries.length > 0 && (
        <div className="backup-plan-items">
          <p className="backup-plan-note">
            Verified local delete recovery copies are kept under AtrisBridge app-data. Restoring one
            recreates it locally only; Drive is not changed.
          </p>
          {availableRecoveries.map((entry) => (
            <div key={entry.id} className="backup-plan-item update">
              <span className="backup-plan-item-icon">
                <RotateCcw size={14} />
              </span>
              <div>
                <strong>{entry.relativePath}</strong>
                <span>
                  Recovery copy · {formatBytes(entry.size)} · {formatDate(entry.createdAt)}
                </span>
              </div>
              <button
                className="secondary-button"
                onClick={() => handleRestoreRecovery(entry)}
                disabled={busy !== null || recoveryBusy !== null}
              >
                {recoveryBusy === entry.id ? (
                  <RefreshCw className="spin" size={13} />
                ) : (
                  <RotateCcw size={13} />
                )}
                Restore locally
              </button>
            </div>
          ))}
        </div>
      )}

      <div className="backup-safety-strip">
        <ShieldCheck size={13} />
        Phase 6 uses explicit review, baseline evidence, exact-ID Drive Trash, verified local
        recovery copies, and conflict blocking. Permanent remote deletion and automatic conflict
        resolution remain unavailable.
      </div>
    </section>
  );
}
