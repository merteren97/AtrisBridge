import {
  Activity,
  ChevronRight,
  LayoutDashboard,
  PanelRightClose,
  PanelRightOpen,
  Plus,
  RefreshCw,
  ScanSearch,
  Settings,
  TriangleAlert,
  X,
} from "lucide-react";
import ActivityView from "./ActivityView";
import OverviewView from "./OverviewView";
import SettingsView from "./SettingsView";
import WorkspaceView from "./WorkspaceView";
import { useProductModel, workspaceState } from "./useProductModel";

export default function ProductApp() {
  const model = useProductModel();
  const {
    view,
    setView,
    workspaces,
    selected,
    summaries,
    bindings,
    googleDrive,
    rcloneStatus,
    totalPending,
    totalConflicts,
    loading,
    notice,
    setError,
    inspectorOpen,
    setInspectorOpen,
    handleAddWorkspace,
    handleScan,
    openWorkspace,
  } = model;

  const pageTitle = view === "workspace"
    ? selected?.name ?? "Workspace"
    : view === "activity"
      ? "Activity"
      : view === "settings"
        ? "Settings"
        : "Overview";

  return (
    <div className="ab-shell">
      <aside className="ab-sidebar">
        <button className="ab-brand" onClick={() => setView("overview")} type="button" aria-label="AtrisBridge overview">
          <img src="/brand/atrisbridge-mark.svg" alt="" aria-hidden="true" />
          <span><strong>AtrisBridge</strong><small>Project continuity</small></span>
        </button>

        <nav className="ab-nav" aria-label="Primary navigation">
          <button className={view === "overview" ? "active" : ""} onClick={() => setView("overview")}>
            <LayoutDashboard size={18} /><span>Overview</span>
          </button>
          <button className={view === "activity" ? "active" : ""} onClick={() => setView("activity")}>
            <Activity size={18} /><span>Activity</span>
            {(totalPending > 0 || totalConflicts > 0) && <b>{totalPending + totalConflicts}</b>}
          </button>
        </nav>

        <section className="ab-sidebar-workspaces" aria-label="Workspaces">
          <div className="ab-sidebar-label">
            <span>Workspaces</span>
            <button type="button" onClick={handleAddWorkspace} aria-label="Add workspace"><Plus size={16} /></button>
          </div>
          <div className="ab-workspace-nav-list">
            {workspaces.length === 0 ? (
              <p className="ab-sidebar-empty">Your protected project folders will appear here.</p>
            ) : workspaces.map((workspace) => {
              const state = workspaceState(summaries[workspace.id], bindings[workspace.id]);
              return (
                <button
                  type="button"
                  key={workspace.id}
                  className={view === "workspace" && selected?.id === workspace.id ? "selected" : ""}
                  onClick={() => openWorkspace(workspace)}
                >
                  <span className={`ab-project-dot ${state.tone}`} />
                  <span className="ab-workspace-nav-copy"><strong>{workspace.name}</strong><small>{state.label}</small></span>
                  <ChevronRight size={14} />
                </button>
              );
            })}
          </div>
        </section>

        <div className="ab-sidebar-footer">
          <div className="ab-sidebar-runtime">
            <span className={rcloneStatus?.available ? "online" : "offline"} />
            <div><strong>{googleDrive?.sessionActive ? "Cloud connected" : "Local mode"}</strong><small>{rcloneStatus?.available ? "Transfer service ready" : "Transfer service offline"}</small></div>
          </div>
          <button className={view === "settings" ? "ab-settings-button active" : "ab-settings-button"} onClick={() => setView("settings")}>
            <Settings size={18} /><span>Settings</span>
          </button>
        </div>
      </aside>

      <main className="ab-main">
        <header className="ab-topbar">
          <div className="ab-page-title"><span>{view === "workspace" ? "Workspace" : "AtrisBridge"}</span><h1>{pageTitle}</h1></div>
          <div className="ab-topbar-actions">
            {view === "workspace" && selected && (
              <>
                <button className="ab-icon-button" type="button" onClick={() => setInspectorOpen((current) => !current)} title={inspectorOpen ? "Hide details" : "Show details"}>
                  {inspectorOpen ? <PanelRightClose size={18} /> : <PanelRightOpen size={18} />}
                </button>
                <button className="ab-button secondary" onClick={handleScan} disabled={loading}>
                  {loading ? <RefreshCw className="spin" size={16} /> : <ScanSearch size={16} />} Scan
                </button>
              </>
            )}
            <button className="ab-button primary" onClick={handleAddWorkspace} disabled={loading}><Plus size={16} /> Add workspace</button>
          </div>
        </header>

        {notice && (
          <section className="ab-notice" role="alert">
            <span><TriangleAlert size={18} /></span>
            <div><strong>{notice.title}</strong><p>{notice.message}</p><details><summary>Technical details</summary><code>{notice.detail}</code></details></div>
            <button type="button" onClick={() => setError(null)} aria-label="Dismiss message"><X size={16} /></button>
          </section>
        )}

        <div className="ab-scroll">
          {view === "overview" && <OverviewView model={model} />}
          {view === "workspace" && selected && <WorkspaceView model={model} />}
          {view === "workspace" && !selected && (
            <div className="ab-view"><div className="ab-empty-state standalone"><span><Plus size={28} /></span><div><h3>No workspace selected</h3><p>Add a project folder to start using AtrisBridge.</p></div><button className="ab-button primary" onClick={handleAddWorkspace}><Plus size={16} /> Add workspace</button></div></div>
          )}
          {view === "activity" && <ActivityView model={model} />}
          {view === "settings" && <SettingsView model={model} />}
        </div>
      </main>
    </div>
  );
}
