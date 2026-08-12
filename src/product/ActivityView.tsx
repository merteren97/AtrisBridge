import { Activity, ArrowRight, CheckCircle2, ListChecks, TriangleAlert } from "lucide-react";
import type { ProductModel } from "./useProductModel";
import { formatDate, workspaceState } from "./useProductModel";

export default function ActivityView({ model }: { model: ProductModel }) {
  const { workspaces, summaries, bindings, totalChanged, totalPending, totalConflicts, openWorkspace } = model;

  return (
    <div className="ab-view ab-activity-view">
      <section className="ab-activity-summary-line">
        <div><span>Changed</span><strong>{totalChanged}</strong></div><i />
        <div><span>Waiting</span><strong>{totalPending}</strong></div><i />
        <div className={totalConflicts > 0 ? "attention" : ""}><span>Conflicts</span><strong>{totalConflicts}</strong></div>
        <p>Workspace state is grouped here; live watcher notifications stay in the Activity Center.</p>
      </section>

      <section className="ab-sheet">
        <header className="ab-sheet-header"><div><span className="ab-kicker">Workspaces</span><h2>Current activity</h2></div><ListChecks size={19} /></header>
        {workspaces.length === 0 ? (
          <div className="ab-inventory-empty"><Activity size={25} /><div><strong>No activity yet</strong><p>Add a workspace and AtrisBridge will surface its current state here.</p></div></div>
        ) : (
          <div className="ab-activity-list">
            {workspaces.map((workspace) => {
              const summary = summaries[workspace.id];
              const state = workspaceState(summary, bindings[workspace.id]);
              return (
                <button type="button" key={workspace.id} onClick={() => openWorkspace(workspace)}>
                  <span className={`ab-activity-state ${state.tone}`}>{state.tone === "attention" ? <TriangleAlert size={17} /> : <CheckCircle2 size={17} />}</span>
                  <span className="ab-activity-main"><strong>{workspace.name}</strong><small>{state.detail} · Last scan {formatDate(summary?.lastScanAt ?? workspace.lastScanAt)}</small></span>
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
