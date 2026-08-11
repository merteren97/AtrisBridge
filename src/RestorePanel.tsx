import { useEffect, useState } from "react";
import {
  ArrowDownToLine,
  CheckCircle2,
  FileWarning,
  ListChecks,
  RefreshCw,
  ShieldAlert,
  ShieldCheck,
} from "lucide-react";
import {
  executeRestorePlan,
  getLatestRestorePlan,
  prepareRestorePlan,
} from "./lib/bridge";
import type { RestorePlan, Workspace } from "./types";

interface RestorePanelProps {
  workspace: Workspace;
  ready: boolean;
  disabled?: boolean;
  onBusyChange?: (busy: boolean) => void;
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

function statusLabel(plan: RestorePlan): string {
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

export default function RestorePanel({
  workspace,
  ready,
  disabled = false,
  onBusyChange,
  onChanged,
  onError,
}: RestorePanelProps) {
  const [plan, setPlan] = useState<RestorePlan | null>(null);
  const [busy, setBusy] = useState<"load" | "prepare" | "execute" | null>(null);

  useEffect(() => {
    void loadLatest();
  }, [workspace.id]);

  useEffect(() => {
    onBusyChange?.(busy !== null);
  }, [busy, onBusyChange]);

  async function loadLatest() {
    try {
      setBusy((current) => current ?? "load");
      setPlan(await getLatestRestorePlan(workspace.id));
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy((current) => (current === "load" ? null : current));
    }
  }

  async function handlePrepare() {
    try {
      setBusy("prepare");
      const next = await prepareRestorePlan(workspace.id);
      setPlan(next);
      await onChanged();
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleExecute() {
    if (!plan || plan.status !== "ready" || plan.restoreCount === 0) return;

    const confirmed = window.confirm(
      `Restore ${plan.restoreCount.toLocaleString()} file${plan.restoreCount === 1 ? "" : "s"} (${formatBytes(plan.restoreBytes)}) from ${plan.remotePath} into this workspace?\n\nAtrisBridge will only create missing local files or replace files that still match a verified synchronized baseline. Local-only, locally modified, conflicting, and unsafe paths remain untouched.`,
    );
    if (!confirmed) return;

    try {
      setBusy("execute");
      await executeRestorePlan(plan.id);
      setPlan(await getLatestRestorePlan(workspace.id));
      await onChanged();
    } catch (error) {
      onError(String(error));
      await loadLatest();
    } finally {
      setBusy(null);
    }
  }

  const running = busy !== null || disabled;
  const visibleItems = plan?.items.slice(0, 8) ?? [];

  return (
    <section className="backup-plan-card">
      <div className="backup-plan-header">
        <div>
          <div className="backup-plan-title">
            <ArrowDownToLine size={17} />
            <strong>Safe restore plan</strong>
          </div>
          <p>
            Restore downloads into a verified staging file first. Local files are changed only after
            remote checksum and local preconditions still match the reviewed plan.
          </p>
        </div>
        <div className="backup-plan-actions">
          <button
            className="secondary-button"
            onClick={handlePrepare}
            disabled={!ready || running}
          >
            {busy === "prepare" ? (
              <RefreshCw className="spin" size={14} />
            ) : (
              <ListChecks size={14} />
            )}
            Prepare restore
          </button>
          <button
            className="primary-button"
            onClick={handleExecute}
            disabled={
              !ready ||
              running ||
              plan?.status !== "ready" ||
              plan.restoreCount === 0
            }
          >
            {busy === "execute" ? (
              <RefreshCw className="spin" size={14} />
            ) : (
              <ArrowDownToLine size={14} />
            )}
            Run restore
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
            <strong>No restore plan prepared</strong>
            <span>Preparing a plan refreshes both inventories and never writes local files.</span>
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
              <small>Safe restores</small>
              <strong>{plan.restoreCount.toLocaleString()}</strong>
              <span>{formatBytes(plan.restoreBytes)}</span>
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
                      <ArrowDownToLine size={14} />
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
                  Showing the first safety-relevant restore entries.
                </p>
              )}
            </div>
          )}
        </>
      )}

      <div className="backup-safety-strip">
        <ShieldCheck size={13} />
        Phase 5 is explicit Drive → local restore only. Remote absence never deletes local files,
        remote content is staged and verified before apply, and uncertain overlaps remain blocked.
      </div>
    </section>
  );
}
