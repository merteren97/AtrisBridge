import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Activity,
  ArrowRight,
  CheckCircle2,
  Cloud,
  CloudCog,
  Database,
  FileCode2,
  FolderOpen,
  FolderSync,
  HardDrive,
  LayoutDashboard,
  Link2,
  ListChecks,
  MonitorDot,
  Plus,
  RefreshCw,
  ScanSearch,
  Settings,
  ShieldCheck,
  SlidersHorizontal,
  Trash2,
  TriangleAlert,
  Unplug,
  X,
} from "lucide-react";
import BackupPanel from "./BackupPanel";
import EncryptionPanel from "./EncryptionPanel";
import {
  addWorkspace,
  bindWorkspaceRemote,
  connectGoogleDrive,
  disconnectProviderSession,
  forgetProvider,
  getRcloneStatus,
  getWorkspaceRemoteBinding,
  initializeIgnoreFile,
  listJournalSummaries,
  listProviderConnections,
  listWorkspaces,
  removeWorkspace,
  scanRemoteInventory,
  scanWorkspace,
} from "./lib/bridge";
import type {
  JournalSummary,
  ProviderConnection,
  RcloneStatus,
  RemoteInventoryReport,
  ScanReport,
  Workspace,
  WorkspaceRemoteBinding,
} from "./types";

type AppView = "overview" | "workspace" | "activity" | "settings";

interface ProductNotice {
  title: string;
  message: string;
  detail: string;
}

const CLOSE_TO_TRAY_KEY = "atrisbridge.closeToTray";

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** index).toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

function formatDate(value: string | null | undefined): string {
  if (!value) return "Not yet";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

function fileNameFromPath(path: string): string {
  return path.replace(/\\/g, "/").split("/").filter(Boolean).at(-1) ?? "Workspace";
}

function remoteSegment(value: string): string {
  return value.replace(/[\\/]/g, "-").trim() || "Workspace";
}

function syncModeLabel(mode: Workspace["syncMode"]): string {
  if (mode === "two_way") return "Two-Way";
  if (mode === "pull") return "Pull";
  return "Backup";
}

function productNotice(value: string): ProductNotice {
  const detail = value.replace(/^Error:\s*/i, "").trim();
  const lower = detail.toLowerCase();

  if (
    lower.includes("google drive") &&
    (lower.includes("userinfo") ||
      lower.includes("account check") ||
      lower.includes("authorization") ||
      lower.includes("verification"))
  ) {
    return {
      title: "Google Drive connection could not be completed",
      message: "AtrisBridge could not finish verifying the selected Google account. Try connecting again; no workspace files were changed.",
      detail,
    };
  }

  if (lower.includes(".atrisbridgeignore already exists")) {
    return {
      title: "Ignore file already exists",
      message: "AtrisBridge left the existing .atrisbridgeignore file unchanged.",
      detail,
    };
  }

  return {
    title: "AtrisBridge could not complete that action",
    message: "Nothing was applied after the operation failed. Review the technical details and try again.",
    detail,
  };
}

export default function App() {
  const [view, setView] = useState<AppView>("overview");
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [reports, setReports] = useState<Record<string, ScanReport>>({});
  const [remoteReports, setRemoteReports] = useState<Record<string, RemoteInventoryReport>>({});
  const [summaries, setSummaries] = useState<Record<string, JournalSummary>>({});
  const [bindings, setBindings] = useState<Record<string, WorkspaceRemoteBinding | null>>({});
  const [providers, setProviders] = useState<ProviderConnection[]>([]);
  const [rcloneStatus, setRcloneStatus] = useState<RcloneStatus | null>(null);
  const [remotePathDraft, setRemotePathDraft] = useState("");
  const [loading, setLoading] = useState(false);
  const [cloudLoading, setCloudLoading] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [closeToTray, setCloseToTray] = useState(
    () => localStorage.getItem(CLOSE_TO_TRAY_KEY) === "1",
  );

  const selected = useMemo(
    () => workspaces.find((workspace) => workspace.id === selectedId) ?? workspaces[0] ?? null,
    [selectedId, workspaces],
  );
  const report = selected ? reports[selected.id] : undefined;
  const remoteReport = selected ? remoteReports[selected.id] : undefined;
  const journal = selected ? summaries[selected.id] : undefined;
  const binding = selected ? bindings[selected.id] : null;
  const googleDrive = providers.find((provider) => provider.providerType === "google_drive") ?? null;
  const backupReady = Boolean(
    selected && binding && googleDrive?.sessionActive && rcloneStatus?.available,
  );
  const encryptionReady = Boolean(backupReady && googleDrive?.credentialPersisted);
  const notice = error ? productNotice(error) : null;

  useEffect(() => {
    void refreshWorkspaces();
    void refreshCloud();
  }, []);

  useEffect(() => {
    if (selected) void refreshBinding(selected);
  }, [selected?.id]);

  useEffect(() => {
    localStorage.setItem(CLOSE_TO_TRAY_KEY, closeToTray ? "1" : "0");
    void invoke("set_close_to_tray", { enabled: closeToTray }).catch((reason) => {
      setError(`Could not update close behavior: ${String(reason)}`);
    });
  }, [closeToTray]);

  async function refreshWorkspaces() {
    try {
      const [items, journalItems] = await Promise.all([
        listWorkspaces(),
        listJournalSummaries(),
      ]);
      setWorkspaces(items);
      setSummaries(Object.fromEntries(journalItems.map((item) => [item.workspaceId, item])));
      setSelectedId((current) => current ?? items[0]?.id ?? null);
    } catch (err) {
      setError(String(err));
    }
  }

  async function refreshCloud() {
    try {
      const [runtime, connections] = await Promise.all([
        getRcloneStatus(),
        listProviderConnections(),
      ]);
      setRcloneStatus(runtime);
      setProviders(connections);
    } catch (err) {
      setError(String(err));
    }
  }

  async function refreshBinding(workspace: Workspace) {
    try {
      const next = await getWorkspaceRemoteBinding(workspace.id);
      setBindings((current) => ({ ...current, [workspace.id]: next }));
      setRemotePathDraft(
        next?.remotePath ?? `AtrisBridge/${remoteSegment(workspace.name)}-${workspace.id.slice(0, 8)}`,
      );
    } catch (err) {
      setError(String(err));
    }
  }

  async function refreshAfterBackupChange() {
    await refreshWorkspaces();
    if (selected) await refreshBinding(selected);
  }

  function openWorkspace(workspace: Workspace) {
    setSelectedId(workspace.id);
    setView("workspace");
  }

  async function handleAddWorkspace() {
    const path = await open({
      directory: true,
      multiple: false,
      title: "Choose a project folder",
    });
    if (!path || Array.isArray(path)) return;

    try {
      setLoading(true);
      setError(null);
      const workspace = await addWorkspace(fileNameFromPath(path), path);
      await refreshWorkspaces();
      setSelectedId(workspace.id);
      setView("workspace");
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  async function handleScan() {
    if (!selected) return;
    try {
      setLoading(true);
      setError(null);
      const next = await scanWorkspace(selected.id);
      setReports((current) => ({ ...current, [selected.id]: next }));
      await refreshWorkspaces();
      setSelectedId(selected.id);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  async function handleCreateIgnore() {
    if (!selected) return;
    try {
      setLoading(true);
      setError(null);
      if (!(await initializeIgnoreFile(selected.id))) {
        setError(".atrisbridgeignore already exists; no file was changed.");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  async function handleRemove() {
    if (
      !selected ||
      !window.confirm(`Remove ${selected.name} from AtrisBridge? No project files will be deleted.`)
    ) {
      return;
    }

    try {
      setLoading(true);
      setError(null);
      await removeWorkspace(selected.id);
      const remaining = workspaces.filter((workspace) => workspace.id !== selected.id);
      setWorkspaces(remaining);
      setSelectedId(remaining[0]?.id ?? null);
      setReports(({ [selected.id]: _, ...rest }) => rest);
      setRemoteReports(({ [selected.id]: _, ...rest }) => rest);
      setSummaries(({ [selected.id]: _, ...rest }) => rest);
      setBindings(({ [selected.id]: _, ...rest }) => rest);
      setView("overview");
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  async function handleConnectGoogleDrive() {
    try {
      setCloudLoading("connect");
      setError(null);
      const provider = await connectGoogleDrive();
      setProviders((current) => [provider, ...current.filter((item) => item.id !== provider.id)]);
    } catch (err) {
      setError(String(err));
    } finally {
      setCloudLoading(null);
    }
  }

  async function handleDisconnectCloudSession() {
    if (!googleDrive) return;
    if (!window.confirm(
      "Remove the saved Google Drive credential from this device? Cloud operations will require Google authorization again. Drive data is not deleted.",
    )) {
      return;
    }
    try {
      setCloudLoading("disconnect");
      await disconnectProviderSession(googleDrive.id);
      await refreshCloud();
    } catch (err) {
      setError(String(err));
    } finally {
      setCloudLoading(null);
    }
  }

  async function handleForgetCloud() {
    if (
      !googleDrive ||
      !window.confirm("Forget Google Drive metadata? Nothing will be deleted from Drive.")
    ) {
      return;
    }

    try {
      setCloudLoading("forget");
      await forgetProvider(googleDrive.id);
      setProviders([]);
      setBindings({});
      if (selected) {
        setRemotePathDraft(`AtrisBridge/${remoteSegment(selected.name)}-${selected.id.slice(0, 8)}`);
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setCloudLoading(null);
    }
  }

  async function handleBindRemote() {
    if (!selected || !googleDrive) return;
    try {
      setCloudLoading("bind");
      setError(null);
      const next = await bindWorkspaceRemote(selected.id, googleDrive.id, remotePathDraft);
      setBindings((current) => ({ ...current, [selected.id]: next }));
      setRemotePathDraft(next.remotePath);
    } catch (err) {
      setError(String(err));
    } finally {
      setCloudLoading(null);
    }
  }

  async function handleRemoteScan() {
    if (!selected) return;
    try {
      setCloudLoading("scan");
      setError(null);
      const next = await scanRemoteInventory(selected.id);
      setRemoteReports((current) => ({ ...current, [selected.id]: next }));
      await Promise.all([refreshWorkspaces(), refreshBinding(selected)]);
      setSelectedId(selected.id);
    } catch (err) {
      setError(String(err));
    } finally {
      setCloudLoading(null);
    }
  }

  const totalKnownBytes = Object.values(summaries).reduce(
    (sum, item) => sum + item.presentBytes,
    0,
  );
  const totalKnownFiles = Object.values(summaries).reduce(
    (sum, item) => sum + item.presentFiles,
    0,
  );
  const totalPending = Object.values(summaries).reduce(
    (sum, item) => sum + item.pendingOperations,
    0,
  );
  const totalConflicts = Object.values(summaries).reduce(
    (sum, item) => sum + item.conflicts,
    0,
  );
  const totalChanged = Object.values(summaries).reduce(
    (sum, item) => sum + item.changedFiles,
    0,
  );

  const pageTitle = view === "workspace"
    ? selected?.name ?? "Workspace"
    : view === "activity"
      ? "Activity"
      : view === "settings"
        ? "Settings"
        : "Overview";
  const pageDescription = view === "workspace"
    ? "Inspect local evidence, remote mapping, backup and protection state."
    : view === "activity"
      ? "Review pending work and workspace state without digging through raw logs."
      : view === "settings"
        ? "Configure desktop behavior, transport and security preferences."
        : "A focused view of project continuity across every protected workspace.";

  return (
    <div className="product-shell">
      <aside className="product-sidebar">
        <button className="product-brand" onClick={() => setView("overview")} type="button">
          <span className="brand-symbol" aria-hidden="true"><i /><i /></span>
          <span className="brand-copy"><strong>AtrisBridge</strong><small>Project continuity</small></span>
        </button>

        <nav className="sidebar-nav" aria-label="Primary navigation">
          <button className={view === "overview" ? "active" : ""} onClick={() => setView("overview")}>
            <LayoutDashboard size={17} /><span>Overview</span>
          </button>
          <button className={view === "activity" ? "active" : ""} onClick={() => setView("activity")}>
            <Activity size={17} /><span>Activity</span>
            {(totalPending > 0 || totalConflicts > 0) && <b>{totalPending + totalConflicts}</b>}
          </button>
        </nav>

        <section className="sidebar-workspaces" aria-label="Workspaces">
          <div className="sidebar-section-title">
            <span>Workspaces</span>
            <button type="button" onClick={handleAddWorkspace} aria-label="Add workspace"><Plus size={15} /></button>
          </div>
          <div className="workspace-list">
            {workspaces.length === 0 ? (
              <div className="sidebar-empty">No protected folders yet.</div>
            ) : workspaces.map((workspace) => {
              const summary = summaries[workspace.id];
              const needsAttention = (summary?.conflicts ?? 0) > 0;
              return (
                <button
                  type="button"
                  key={workspace.id}
                  className={view === "workspace" && selected?.id === workspace.id ? "selected" : ""}
                  onClick={() => openWorkspace(workspace)}
                >
                  <span className={`workspace-state-dot ${needsAttention ? "attention" : ""}`} />
                  <span className="workspace-list-copy">
                    <strong>{workspace.name}</strong>
                    <small>{syncModeLabel(workspace.syncMode)} · {summary?.presentFiles?.toLocaleString() ?? 0} files</small>
                  </span>
                  {needsAttention && <span className="workspace-alert-mark">!</span>}
                </button>
              );
            })}
          </div>
        </section>

        <div className="sidebar-footer">
          <div className="runtime-mini-status">
            <span className={rcloneStatus?.available ? "online" : "offline"} />
            <div>
              <strong>{rcloneStatus?.available ? "Transport ready" : "Transport offline"}</strong>
              <small>{googleDrive?.sessionActive ? "Google Drive connected" : "Local mode"}</small>
            </div>
          </div>
          <button className={view === "settings" ? "settings-nav active" : "settings-nav"} onClick={() => setView("settings")}>
            <Settings size={17} /><span>Settings</span>
          </button>
        </div>
      </aside>

      <main className="product-main">
        <header className="product-topbar">
          <div className="page-heading">
            <span className="page-kicker">{view === "workspace" ? "Workspace" : "AtrisBridge"}</span>
            <h1>{pageTitle}</h1>
            <p>{pageDescription}</p>
          </div>
          <div className="topbar-actions">
            {view === "workspace" && selected && (
              <button className="button secondary" onClick={handleScan} disabled={loading}>
                {loading ? <RefreshCw className="spin" size={15} /> : <ScanSearch size={15} />}
                Scan
              </button>
            )}
            <button className="button primary" onClick={handleAddWorkspace} disabled={loading}>
              <Plus size={15} /> Add workspace
            </button>
          </div>
        </header>

        {notice && (
          <section className="product-notice" role="alert">
            <span className="notice-icon"><TriangleAlert size={16} /></span>
            <div>
              <strong>{notice.title}</strong>
              <p>{notice.message}</p>
              <details><summary>Technical details</summary><code>{notice.detail}</code></details>
            </div>
            <button type="button" onClick={() => setError(null)} aria-label="Dismiss message"><X size={15} /></button>
          </section>
        )}

        <div className="page-scroll">
          {view === "overview" && (
            <div className="page-stack overview-page">
              <section className="summary-strip" aria-label="AtrisBridge summary">
                <div><small>Protected workspaces</small><strong>{workspaces.length}</strong><span>{workspaces.length === 1 ? "workspace" : "workspaces"}</span></div>
                <div><small>Indexed content</small><strong>{totalKnownFiles.toLocaleString()}</strong><span>{formatBytes(totalKnownBytes)}</span></div>
                <div><small>Pending operations</small><strong>{totalPending}</strong><span>{totalChanged} changed files</span></div>
                <div className={totalConflicts > 0 ? "attention" : "healthy"}>
                  <small>Continuity state</small>
                  <strong>{totalConflicts > 0 ? `${totalConflicts} attention` : "Healthy"}</strong>
                  <span>{totalConflicts > 0 ? "Conflicts require review" : "No unresolved conflicts"}</span>
                </div>
              </section>

              <section className="content-section">
                <div className="section-header">
                  <div><span className="section-kicker">Protected projects</span><h2>Workspaces</h2></div>
                  <button className="text-action" onClick={handleAddWorkspace}><Plus size={14} /> Add workspace</button>
                </div>

                {workspaces.length === 0 ? (
                  <div className="product-empty-state">
                    <span className="empty-icon"><FolderSync size={24} /></span>
                    <div><h3>Add the first project you want to protect</h3><p>AtrisBridge starts locally, builds an inventory, then lets you attach cloud transport when you are ready.</p></div>
                    <button className="button primary" onClick={handleAddWorkspace}><FolderOpen size={15} /> Choose project folder</button>
                  </div>
                ) : (
                  <div className="workspace-table" role="list">
                    {workspaces.map((workspace) => {
                      const summary = summaries[workspace.id];
                      const workspaceBinding = bindings[workspace.id];
                      const issueCount = summary?.conflicts ?? 0;
                      return (
                        <button key={workspace.id} type="button" className="workspace-row" onClick={() => openWorkspace(workspace)} role="listitem">
                          <span className="workspace-row-icon"><FolderOpen size={17} /></span>
                          <span className="workspace-row-main"><strong>{workspace.name}</strong><small>{workspace.localPath}</small></span>
                          <span className="workspace-row-stat"><small>Mode</small><strong>{syncModeLabel(workspace.syncMode)}</strong></span>
                          <span className="workspace-row-stat"><small>Files</small><strong>{summary?.presentFiles?.toLocaleString() ?? "—"}</strong></span>
                          <span className="workspace-row-stat"><small>Last scan</small><strong>{formatDate(summary?.lastScanAt ?? workspace.lastScanAt)}</strong></span>
                          <span className={`workspace-row-state ${issueCount > 0 ? "attention" : ""}`}>
                            {issueCount > 0 ? <TriangleAlert size={14} /> : <CheckCircle2 size={14} />}
                            {issueCount > 0 ? `${issueCount} conflicts` : workspaceBinding ? "Cloud mapped" : "Local only"}
                          </span>
                          <ArrowRight size={15} />
                        </button>
                      );
                    })}
                  </div>
                )}
              </section>

              <div className="overview-grid">
                <section className="content-section compact-section">
                  <div className="section-header">
                    <div><span className="section-kicker">Transport</span><h2>Cloud readiness</h2></div>
                    <CloudCog size={18} />
                  </div>
                  <div className="status-list">
                    <div><span className={`status-dot ${rcloneStatus?.available ? "success" : "danger"}`} /><div><strong>Transfer engine</strong><small>{rcloneStatus?.available ? `rclone v${rcloneStatus.version} · ${rcloneStatus.source}` : rcloneStatus?.message ?? "Checking runtime"}</small></div><b>{rcloneStatus?.available ? "Ready" : "Offline"}</b></div>
                    <div><span className={`status-dot ${googleDrive?.sessionActive ? "success" : "neutral"}`} /><div><strong>Google Drive</strong><small>{googleDrive?.accountLabel ?? googleDrive?.displayName ?? "No account connected"}</small></div><b>{googleDrive?.sessionActive ? "Connected" : "Optional"}</b></div>
                  </div>
                  <button className="section-link" onClick={() => setView("settings")}>Manage integrations <ArrowRight size={14} /></button>
                </section>

                <section className="content-section compact-section">
                  <div className="section-header">
                    <div><span className="section-kicker">Safety</span><h2>Local evidence</h2></div>
                    <ShieldCheck size={18} />
                  </div>
                  <div className="evidence-copy">
                    <strong>Local state stays authoritative until you approve remote operations.</strong>
                    <p>Workspace scans are journaled before cloud writes are planned. Conflicts and pending changes stay visible instead of being silently overwritten.</p>
                  </div>
                  <div className="inline-metrics"><span><b>{totalChanged}</b> changed</span><span><b>{totalPending}</b> queued</span><span><b>{totalConflicts}</b> conflicts</span></div>
                </section>
              </div>
            </div>
          )}

          {view === "workspace" && selected && (
            <div className="page-stack workspace-page">
              <section className="workspace-identity">
                <div className="workspace-identity-main">
                  <span className="workspace-avatar"><FolderOpen size={20} /></span>
                  <div><div className="workspace-name-line"><h2>{selected.name}</h2><span className="subtle-pill">{syncModeLabel(selected.syncMode)}</span></div><p>{selected.localPath}</p></div>
                </div>
                <div className="workspace-command-group">
                  <button className="button secondary" onClick={handleCreateIgnore} disabled={loading}><ShieldCheck size={15} /> Ignore file</button>
                  <button className="button primary" onClick={handleScan} disabled={loading}>{loading ? <RefreshCw className="spin" size={15} /> : <ScanSearch size={15} />} Scan workspace</button>
                </div>
              </section>

              <section className="workspace-facts">
                <div><small>Last scan</small><strong>{formatDate(journal?.lastScanAt ?? report?.scannedAt ?? selected.lastScanAt)}</strong><span>BLAKE3 local inventory</span></div>
                <div><small>Journal</small><strong>{journal?.presentFiles.toLocaleString() ?? "—"} files</strong><span>{journal?.changedFiles ?? 0} changed</span></div>
                <div><small>Operations</small><strong>{journal?.pendingOperations ?? 0} pending</strong><span>{journal?.tombstones ?? 0} tombstones</span></div>
                <div className={(journal?.conflicts ?? 0) > 0 ? "attention" : ""}><small>Conflicts</small><strong>{journal?.conflicts ?? 0}</strong><span>{(journal?.conflicts ?? 0) > 0 ? "Review required" : "Clear"}</span></div>
              </section>

              <div className="workspace-layout">
                <div className="workspace-primary-column">
                  <section className="content-section">
                    <div className="section-header inventory-header">
                      <div><span className="section-kicker">Local inventory</span><h2>{report ? `${report.fileCount.toLocaleString()} files · ${formatBytes(report.totalBytes)}` : journal?.lastScanAt ? `${journal.presentFiles.toLocaleString()} persisted files` : "Inventory not built yet"}</h2></div>
                      {report && <span className="duration-pill">{report.durationMs} ms</span>}
                    </div>

                    {!report ? (
                      <div className="inventory-empty">
                        <Database size={22} />
                        <div><strong>{journal?.lastScanAt ? "A durable inventory already exists" : "Scan this workspace to establish the local baseline"}</strong><p>{journal?.lastScanAt ? "Run another scan whenever you want a fresh preview of the persisted journal." : "No remote write is planned until local evidence exists."}</p></div>
                        <button className="button secondary" onClick={handleScan}>Run scan</button>
                      </div>
                    ) : (
                      <div className="file-table-wrap">
                        <table className="file-table">
                          <thead><tr><th>Path</th><th>Size</th><th>BLAKE3</th></tr></thead>
                          <tbody>{report.files.map((file) => (
                            <tr key={file.relativePath}><td><FileCode2 size={13} /><span>{file.relativePath}</span></td><td>{formatBytes(file.size)}</td><td><code>{file.blake3.slice(0, 16)}…</code></td></tr>
                          ))}</tbody>
                        </table>
                        {report.previewTruncated && <p className="table-note">Preview limited to 250 entries. The complete inventory remains in the local journal.</p>}
                      </div>
                    )}
                  </section>

                  <BackupPanel
                    workspace={selected}
                    ready={backupReady}
                    onChanged={refreshAfterBackupChange}
                    onError={(message) => setError(message)}
                  />
                </div>

                <aside className="workspace-side-column">
                  <section className="content-section remote-section">
                    <div className="section-header">
                      <div><span className="section-kicker">Workspace transport</span><h2>Remote mapping</h2></div>
                      <Cloud size={18} />
                    </div>
                    {!googleDrive ? (
                      <div className="side-state"><CloudCog size={20} /><strong>Google Drive is not connected</strong><p>Connect a transport account in Settings before mapping this workspace.</p><button className="button secondary" onClick={() => setView("settings")}>Open Settings</button></div>
                    ) : (
                      <>
                        <label className="field-group"><span>Remote folder</span><input value={remotePathDraft} onChange={(event) => setRemotePathDraft(event.target.value)} spellCheck={false} /><small>Dedicated folder used only for this workspace.</small></label>
                        <div className="button-row">
                          <button className="button secondary" onClick={handleBindRemote} disabled={cloudLoading !== null}><Link2 size={14} /> {binding ? "Update mapping" : "Bind folder"}</button>
                          <button className="button primary" onClick={handleRemoteScan} disabled={!binding || !googleDrive.sessionActive || cloudLoading !== null}>{cloudLoading === "scan" ? <RefreshCw className="spin" size={14} /> : <ScanSearch size={14} />} Scan remote</button>
                        </div>
                        <div className="mapping-status">
                          <span className={binding ? "success" : "neutral"}>{binding ? <CheckCircle2 size={13} /> : <Link2 size={13} />}{binding ? "Mapped" : "Not mapped"}</span>
                          <p>{binding?.remotePath ?? "Save a dedicated Drive folder mapping first."}</p>
                          <small>{remoteReport ? `${remoteReport.fileCount.toLocaleString()} remote files · ${formatBytes(remoteReport.totalBytes)}` : binding?.lastInventoryAt ? `Last remote scan ${formatDate(binding.lastInventoryAt)}` : "Remote inventory not read yet"}</small>
                        </div>
                      </>
                    )}
                  </section>

                  {binding && googleDrive && (
                    <EncryptionPanel
                      workspaceId={selected.id}
                      ready={encryptionReady}
                      onError={(message) => setError(message)}
                    />
                  )}

                  <section className="danger-section">
                    <div><strong>Remove workspace</strong><p>Only AtrisBridge metadata is removed. Project files stay untouched.</p></div>
                    <button className="button danger" onClick={handleRemove}><Trash2 size={14} /> Remove</button>
                  </section>
                </aside>
              </div>
            </div>
          )}

          {view === "workspace" && !selected && (
            <div className="product-empty-state standalone"><FolderOpen size={26} /><div><h3>No workspace selected</h3><p>Add a project folder to start building local continuity evidence.</p></div><button className="button primary" onClick={handleAddWorkspace}><Plus size={15} /> Add workspace</button></div>
          )}

          {view === "activity" && (
            <div className="page-stack activity-page">
              <section className="summary-strip activity-summary">
                <div><small>Changed files</small><strong>{totalChanged}</strong><span>across all workspaces</span></div>
                <div><small>Pending operations</small><strong>{totalPending}</strong><span>waiting in journal</span></div>
                <div className={totalConflicts > 0 ? "attention" : "healthy"}><small>Conflicts</small><strong>{totalConflicts}</strong><span>{totalConflicts > 0 ? "Need review" : "No unresolved conflicts"}</span></div>
              </section>

              <section className="content-section">
                <div className="section-header"><div><span className="section-kicker">Workspace state</span><h2>Current activity</h2></div><ListChecks size={18} /></div>
                {workspaces.length === 0 ? (
                  <div className="inventory-empty"><Activity size={22} /><div><strong>No workspace activity yet</strong><p>Add a workspace first; AtrisBridge will surface journal and synchronization state here.</p></div></div>
                ) : (
                  <div className="activity-table">
                    {workspaces.map((workspace) => {
                      const summary = summaries[workspace.id];
                      const issues = summary?.conflicts ?? 0;
                      return (
                        <button type="button" key={workspace.id} onClick={() => openWorkspace(workspace)}>
                          <span className={`activity-state-icon ${issues > 0 ? "attention" : ""}`}>{issues > 0 ? <TriangleAlert size={15} /> : <CheckCircle2 size={15} />}</span>
                          <span className="activity-main"><strong>{workspace.name}</strong><small>Last scan {formatDate(summary?.lastScanAt ?? workspace.lastScanAt)}</small></span>
                          <span><small>Changed</small><strong>{summary?.changedFiles ?? 0}</strong></span>
                          <span><small>Queued</small><strong>{summary?.pendingOperations ?? 0}</strong></span>
                          <span><small>Conflicts</small><strong>{issues}</strong></span>
                          <ArrowRight size={15} />
                        </button>
                      );
                    })}
                  </div>
                )}
              </section>

              <section className="activity-help">
                <MonitorDot size={18} />
                <div><strong>Background synchronization status remains available from the compact activity control.</strong><p>Use this page for workspace-level state; the activity control surfaces live watcher and completion notifications.</p></div>
              </section>
            </div>
          )}

          {view === "settings" && (
            <div className="settings-layout">
              <aside className="settings-index" aria-label="Settings categories">
                <a href="#general"><SlidersHorizontal size={15} /> General</a>
                <a href="#integrations"><CloudCog size={15} /> Integrations</a>
                <a href="#security"><ShieldCheck size={15} /> Security</a>
                <a href="#about"><HardDrive size={15} /> Runtime</a>
              </aside>

              <div className="settings-content">
                <section id="general" className="settings-section">
                  <div className="settings-heading"><div><span className="section-kicker">General</span><h2>Desktop behavior</h2><p>Choose what happens when the main AtrisBridge window is closed.</p></div></div>
                  <div className="setting-row">
                    <div><strong>Keep AtrisBridge running in the tray</strong><p>When enabled, closing the window hides AtrisBridge so background synchronization can continue. When disabled, closing the window quits the application.</p></div>
                    <label className="toggle-control"><input type="checkbox" checked={closeToTray} onChange={(event) => setCloseToTray(event.target.checked)} /><span aria-hidden="true" /></label>
                  </div>
                  <div className="settings-note"><CheckCircle2 size={14} /><span>Current behavior: <strong>{closeToTray ? "Close to tray" : "Quit on close"}</strong>. This preference is saved on this device.</span></div>
                </section>

                <section id="integrations" className="settings-section">
                  <div className="settings-heading"><div><span className="section-kicker">Integrations</span><h2>Google Drive</h2><p>Manage the transport account separately from per-workspace folder mappings.</p></div><span className={`settings-status ${googleDrive?.sessionActive ? "success" : "neutral"}`}>{googleDrive?.sessionActive ? <CheckCircle2 size={13} /> : <Cloud size={13} />}{googleDrive?.sessionActive ? "Connected" : "Not connected"}</span></div>

                  <div className="integration-account">
                    <span className="integration-icon"><Cloud size={18} /></span>
                    <div><strong>{googleDrive?.accountLabel ?? googleDrive?.displayName ?? "Google Drive"}</strong><p>{googleDrive?.credentialPersisted ? "Credential is protected by the operating-system secure vault." : googleDrive?.sessionActive ? "Connected for this session; secure persistence is unavailable." : "Connect only when you are ready to map workspace folders."}</p></div>
                    <div className="integration-actions">
                      <button className={googleDrive?.sessionActive ? "button secondary" : "button primary"} onClick={handleConnectGoogleDrive} disabled={!rcloneStatus?.available || cloudLoading !== null}>{cloudLoading === "connect" ? <RefreshCw className="spin" size={14} /> : <Cloud size={14} />}{googleDrive ? "Reconnect" : "Connect"}</button>
                      {googleDrive?.sessionActive && <button className="icon-action" onClick={handleDisconnectCloudSession} aria-label="Disconnect Google Drive session" title="Disconnect session"><Unplug size={15} /></button>}
                      {googleDrive && <button className="icon-action danger" onClick={handleForgetCloud} aria-label="Forget Google Drive connection" title="Forget connection"><Trash2 size={15} /></button>}
                    </div>
                  </div>
                </section>

                <section id="security" className="settings-section">
                  <div className="settings-heading"><div><span className="section-kicker">Security</span><h2>Protection model</h2><p>AtrisBridge keeps transport credentials and synchronization evidence separated.</p></div></div>
                  <div className="security-grid">
                    <div><span><ShieldCheck size={16} /></span><strong>OS secure vault</strong><p>Persisted provider credentials are stored through the operating system credential store, not in frontend storage.</p></div>
                    <div><span><Database size={16} /></span><strong>Durable local journal</strong><p>Inventory and operation evidence remain local and reviewable before remote changes are accepted.</p></div>
                    <div><span><FolderSync size={16} /></span><strong>Workspace-scoped transport</strong><p>Each project maps to a dedicated remote folder instead of sharing an implicit global destination.</p></div>
                  </div>
                </section>

                <section id="about" className="settings-section">
                  <div className="settings-heading"><div><span className="section-kicker">Runtime</span><h2>Transfer engine</h2><p>Runtime diagnostics for the pinned rclone transport bundled with AtrisBridge.</p></div></div>
                  <div className="runtime-setting-row">
                    <div><span className={`runtime-indicator ${rcloneStatus?.available ? "success" : "danger"}`}><HardDrive size={16} /></span><div><strong>{rcloneStatus?.available ? `rclone v${rcloneStatus.version}` : "rclone unavailable"}</strong><p>{rcloneStatus?.available ? `${rcloneStatus.source} runtime · required v${rcloneStatus.requiredVersion}` : rcloneStatus?.message ?? "Runtime status is still loading."}</p></div></div>
                    <button className="button secondary" onClick={() => void refreshCloud()} disabled={cloudLoading !== null}><RefreshCw size={14} /> Refresh</button>
                  </div>
                </section>
              </div>
            </div>
          )}
        </div>
      </main>
    </div>
  );
}
