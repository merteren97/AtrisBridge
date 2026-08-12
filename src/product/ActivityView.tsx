import { Activity, AlertTriangle, ArrowRight, CheckCircle2, LoaderCircle, PauseCircle, Radio } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { getContinuousSyncStatus } from "../lib/bridge";
import type { ContinuousSyncStatus } from "../types";
import type { ProductModel } from "./useProductModel";
import { formatDate, workspaceState } from "./useProductModel";

function liveLabel(status: ContinuousSyncStatus | undefined) {
  switch (status?.state) {
    case "debouncing": return "Changes queued";
    case "running": return "Syncing";
    case "attention": return "Needs attention";
    case "error": return "Sync error";
    case "disabled": return "Paused";
    default: return "Watching";
  }
}

function liveTone(status: ContinuousSyncStatus | undefined) {
  if (!status) return "neutral";
  if (status.state === "attention" || status.state === "error") return "attention";
  if (status.state === "running" || status.state === "debouncing") return "waiting";
  if (status.state === "disabled") return "neutral";
  return "healthy";
}

function liveIcon(status: ContinuousSyncStatus | undefined) {
  if (status?.state === "running" || status?.state === "debouncing") return <LoaderCircle className="spin" size={17} />;
  if (status?.state === "attention" || status?.state === "error") return <AlertTriangle size={17} />;
  if (status?.state === "disabled") return <PauseCircle size={17} />;
  return <Radio size={17} />;
}

function liveDetail(status: ContinuousSyncStatus | undefined) {
  if (!status) return "Live watcher state is loading.";
  if (status.lastMessage) return status.lastMessage;
  if (status.state === "disabled") return "Continuous sync is paused for this workspace.";
  if (status.state === "debouncing") return "Waiting for local changes to settle before the next cycle.";
  if (status.state === "running") return "A synchronization cycle is currently running.";
  return "Monitoring local and remote changes.";
}

export default function ActivityView({ model }: { model: ProductModel }) {
  const { workspaces, summaries, bindings, totalChanged, totalPending, totalConflicts, openWorkspace } = model;
  const [liveStatuses, setLiveStatuses] = useState<Record<string, ContinuousSyncStatus>>({});
  const [liveError, setLiveError] = useState<string | null>(null);

  useEffect(() => {
    let disposed = false;
    const refresh = async () => {
      try {
        const rows = await Promise.all(
          workspaces.map(async (workspace) => [workspace.id, await getContinuousSyncStatus(workspace.id)] as const),
        );
        if (disposed) return;
        setLiveStatuses(Object.fromEntries(rows));
        setLiveError(null);
      } catch (error) {
        if (!disposed) setLiveError(error instanceof Error ? error.message : String(error));
      }
    };
    void refresh();
    const timer = window.setInterval(() => void refresh(), 3000);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, [workspaces]);

  const activeCount = useMemo(
    () => Object.values(liveStatuses).filter((status) => status.state === "running" || status.state === "debouncing").length,
    [liveStatuses],
  );

  return (
    <div className="ab-view ab-activity-view">
      <section className="ab-activity-summary-line">
        <div><span>Live</span><strong>{activeCount}</strong></div><i />
        <div><span>Changed</span><strong>{totalChanged}</strong></div><i />
        <div><span>Waiting</span><strong>{totalPending}</strong></div><i />
        <div className={totalConflicts > 0 ? "attention" : ""}><span>Conflicts</span><strong>{totalConflicts}</strong></div>
        <p>Live watcher state and durable workspace activity are grouped in one place.</p>
      </section>

      {liveError && <div className="ab-activity-inline-error"><AlertTriangle size={15} /> Live sync status could not be refreshed: {liveError}</div>}

      <section className="ab-sheet">
        <header className="ab-sheet-header"><div><span className="ab-kicker">Workspaces</span><h2>Current activity</h2></div><Activity size={19} /></header>
        {workspaces.length === 0 ? (
          <div className="ab-inventory-empty"><Activity size={25} /><div><strong>No activity yet</strong><p>Add a workspace and AtrisBridge will surface its current state here.</p></div></div>
        ) : (
          <div className="ab-activity-list ab-activity-list-live">
            {workspaces.map((workspace) => {
              const summary = summaries[workspace.id];
              const state = workspaceState(summary, bindings[workspace.id]);
              const live = liveStatuses[workspace.id];
              const tone = liveTone(live);
              return (
                <button type="button" key={workspace.id} onClick={() => openWorkspace(workspace)}>
                  <span className={`ab-activity-state ${tone}`}>{live ? liveIcon(live) : <CheckCircle2 size={17} />}</span>
                  <span className="ab-activity-main">
                    <span className="ab-activity-main-title"><strong>{workspace.name}</strong><em className={`ab-live-state ${tone}`}>{liveLabel(live)}</em></span>
                    <small>{liveDetail(live)}</small>
                    <small className="ab-activity-durable">{state.detail} · Last scan {formatDate(summary?.lastScanAt ?? workspace.lastScanAt)}</small>
                  </span>
                  <span><small>Changed</small><strong>{summary?.changedFiles ?? 0}</strong></span>
                  <span><small>Waiting</small><strong>{summary?.pendingOperations ?? 0}</strong></span>
                  <span><small>Conflicts</small><strong>{summary?.conflicts ?? 0}</strong></span>
                  <ArrowRight size={17} />
                </button>
              );
            })}
          </div>
        )}
      </section>
    </div>
  );
}
