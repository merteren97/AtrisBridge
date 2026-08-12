import {
  Activity,
  ArrowRight,
  CheckCircle2,
  CloudCog,
  FolderOpen,
  FolderSync,
  Plus,
  TriangleAlert,
} from "lucide-react";
import type { ProductModel } from "./useProductModel";
import { formatBytes, formatDate, syncModeLabel, workspaceState } from "./useProductModel";

interface OverviewViewProps {
  model: ProductModel;
}

export default function OverviewView({ model }: OverviewViewProps) {
  const {
    workspaces,
    summaries,
    bindings,
    googleDrive,
    rcloneStatus,
    totalKnownBytes,
    totalKnownFiles,
    totalPending,
    totalConflicts,
    totalChanged,
    overviewTone,
    overviewTitle,
    overviewDetail,
    handleAddWorkspace,
    openWorkspace,
    setView,
  } = model;

  return (
    <div className="ab-view ab-overview-view">
      <section className={`ab-health-banner ${overviewTone}`}>
        <span className="ab-health-icon">
          {overviewTone === "attention" ? (
            <TriangleAlert size={22} />
          ) : overviewTone === "waiting" ? (
            <Activity size={22} />
          ) : (
            <CheckCircle2 size={22} />
          )}
        </span>
        <div className="ab-health-copy">
          <strong>{overviewTitle}</strong>
          <p>{overviewDetail}</p>
        </div>
        <div className="ab-health-meta">
          <span><b>{workspaces.length}</b> workspace{workspaces.length === 1 ? "" : "s"}</span>
          <span><b>{totalKnownFiles.toLocaleString()}</b> files</span>
          <span><b>{formatBytes(totalKnownBytes)}</b> indexed</span>
        </div>
      </section>

      <div className="ab-overview-layout">
        <section className="ab-sheet ab-workspace-sheet">
          <header className="ab-sheet-header">
            <div><span className="ab-kicker">Projects</span><h2>Your workspaces</h2></div>
            <button className="ab-text-button" onClick={handleAddWorkspace}>
              <Plus size={15} /> Add workspace
            </button>
          </header>

          {workspaces.length === 0 ? (
            <div className="ab-empty-state">
              <span><FolderSync size={28} /></span>
              <div>
                <h3>Protect your first project folder</h3>
                <p>Choose a workspace, scan it locally, then connect cloud transport only if you need it.</p>
              </div>
              <button className="ab-button primary" onClick={handleAddWorkspace}>
                <FolderOpen size={16} /> Choose folder
              </button>
            </div>
          ) : (
            <div className="ab-workspace-list">
              {workspaces.map((workspace) => {
                const summary = summaries[workspace.id];
                const state = workspaceState(summary, bindings[workspace.id]);
                return (
                  <button type="button" key={workspace.id} onClick={() => openWorkspace(workspace)}>
                    <span className="ab-workspace-icon"><FolderOpen size={18} /></span>
                    <span className="ab-workspace-main">
                      <strong>{workspace.name}</strong>
                      <small>{workspace.localPath}</small>
                    </span>
                    <span className="ab-workspace-mode">{syncModeLabel(workspace.syncMode)}</span>
                    <span className="ab-workspace-activity">
                      <small>Last scan</small>
                      <strong>{formatDate(summary?.lastScanAt ?? workspace.lastScanAt)}</strong>
                    </span>
                    <span className={`ab-status-pill ${state.tone}`}><i />{state.label}</span>
                    <ArrowRight size={17} />
                  </button>
                );
              })}
            </div>
          )}
        </section>

        <aside className="ab-overview-side">
          <section className="ab-side-panel">
            <header><span className="ab-kicker">Cloud</span><h2>Connection</h2></header>
            <div className="ab-connection-row">
              <span className={`ab-provider-icon ${googleDrive?.sessionActive ? "connected" : ""}`}><CloudCog size={19} /></span>
              <div>
                <strong>{googleDrive?.accountLabel ?? googleDrive?.displayName ?? "Google Drive"}</strong>
                <small>{googleDrive?.sessionActive ? "Connected and ready" : "Not connected"}</small>
              </div>
              <b>{rcloneStatus?.available ? "Ready" : "Offline"}</b>
            </div>
            <button className="ab-panel-link" onClick={() => setView("settings")}>
              Manage connection <ArrowRight size={15} />
            </button>
          </section>

          <section className="ab-side-panel quiet">
            <header><span className="ab-kicker">Today</span><h2>Current state</h2></header>
            <div className="ab-activity-mini"><span>Changed</span><strong>{totalChanged}</strong></div>
            <div className="ab-activity-mini"><span>Waiting</span><strong>{totalPending}</strong></div>
            <div className="ab-activity-mini"><span>Conflicts</span><strong className={totalConflicts > 0 ? "attention" : ""}>{totalConflicts}</strong></div>
            <button className="ab-panel-link" onClick={() => setView("activity")}>
              Open activity <ArrowRight size={15} />
            </button>
          </section>
        </aside>
      </div>
    </div>
  );
}
