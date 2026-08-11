import { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  ArrowUpFromLine, Box, CheckCircle2, ChevronRight, Cloud, CloudCog, FileCode2,
  FolderOpen, Gauge, HardDrive, Link2, Plus, RefreshCw, ScanSearch, Settings,
  ShieldCheck, Trash2, TriangleAlert, Unplug,
} from "lucide-react";
import {
  addWorkspace, bindWorkspaceRemote, connectGoogleDrive, disconnectProviderSession,
  forgetProvider, getRcloneStatus, getWorkspaceRemoteBinding, initializeIgnoreFile,
  listJournalSummaries, listProviderConnections, listWorkspaces, removeWorkspace,
  scanRemoteInventory, scanWorkspace,
} from "./lib/bridge";
import type {
  JournalSummary, ProviderConnection, RcloneStatus, RemoteInventoryReport,
  ScanReport, Workspace, WorkspaceRemoteBinding,
} from "./types";

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** index).toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

function formatDate(value: string | null | undefined): string {
  if (!value) return "Not scanned yet";
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" }).format(new Date(value));
}

function fileNameFromPath(path: string): string {
  return path.replace(/\\/g, "/").split("/").filter(Boolean).at(-1) ?? "Workspace";
}

function remoteSegment(value: string): string {
  return value.replace(/[\\/]/g, "-").trim() || "Workspace";
}

export default function App() {
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

  const selected = useMemo(
    () => workspaces.find((workspace) => workspace.id === selectedId) ?? workspaces[0] ?? null,
    [selectedId, workspaces],
  );
  const report = selected ? reports[selected.id] : undefined;
  const remoteReport = selected ? remoteReports[selected.id] : undefined;
  const journal = selected ? summaries[selected.id] : undefined;
  const binding = selected ? bindings[selected.id] : null;
  const googleDrive = providers.find((provider) => provider.providerType === "google_drive") ?? null;

  useEffect(() => { void refreshWorkspaces(); void refreshCloud(); }, []);
  useEffect(() => { if (selected) void refreshBinding(selected); }, [selected?.id]);

  async function refreshWorkspaces() {
    try {
      const [items, journalItems] = await Promise.all([listWorkspaces(), listJournalSummaries()]);
      setWorkspaces(items);
      setSummaries(Object.fromEntries(journalItems.map((item) => [item.workspaceId, item])));
      setSelectedId((current) => current ?? items[0]?.id ?? null);
    } catch (err) { setError(String(err)); }
  }

  async function refreshCloud() {
    try {
      const [runtime, connections] = await Promise.all([getRcloneStatus(), listProviderConnections()]);
      setRcloneStatus(runtime);
      setProviders(connections);
    } catch (err) { setError(String(err)); }
  }

  async function refreshBinding(workspace: Workspace) {
    try {
      const next = await getWorkspaceRemoteBinding(workspace.id);
      setBindings((current) => ({ ...current, [workspace.id]: next }));
      setRemotePathDraft(next?.remotePath ?? `AtrisBridge/${remoteSegment(workspace.name)}-${workspace.id.slice(0, 8)}`);
    } catch (err) { setError(String(err)); }
  }

  async function handleAddWorkspace() {
    const path = await open({ directory: true, multiple: false, title: "Choose a project folder" });
    if (!path || Array.isArray(path)) return;
    try {
      setLoading(true); setError(null);
      const workspace = await addWorkspace(fileNameFromPath(path), path);
      await refreshWorkspaces(); setSelectedId(workspace.id);
    } catch (err) { setError(String(err)); } finally { setLoading(false); }
  }

  async function handleScan() {
    if (!selected) return;
    try {
      setLoading(true); setError(null);
      const next = await scanWorkspace(selected.id);
      setReports((current) => ({ ...current, [selected.id]: next }));
      await refreshWorkspaces(); setSelectedId(selected.id);
    } catch (err) { setError(String(err)); } finally { setLoading(false); }
  }

  async function handleCreateIgnore() {
    if (!selected) return;
    try {
      setLoading(true); setError(null);
      if (!(await initializeIgnoreFile(selected.id))) setError(".atrisbridgeignore already exists; no file was changed.");
    } catch (err) { setError(String(err)); } finally { setLoading(false); }
  }

  async function handleRemove() {
    if (!selected || !window.confirm(`Remove ${selected.name} from AtrisBridge? No project files will be deleted.`)) return;
    try {
      setLoading(true); setError(null); await removeWorkspace(selected.id);
      const remaining = workspaces.filter((workspace) => workspace.id !== selected.id);
      setWorkspaces(remaining); setSelectedId(remaining[0]?.id ?? null);
      setReports(({ [selected.id]: _, ...rest }) => rest);
      setRemoteReports(({ [selected.id]: _, ...rest }) => rest);
      setSummaries(({ [selected.id]: _, ...rest }) => rest);
      setBindings(({ [selected.id]: _, ...rest }) => rest);
    } catch (err) { setError(String(err)); } finally { setLoading(false); }
  }

  async function handleConnectGoogleDrive() {
    try {
      setCloudLoading("connect"); setError(null);
      const provider = await connectGoogleDrive();
      setProviders((current) => [provider, ...current.filter((item) => item.id !== provider.id)]);
    } catch (err) { setError(String(err)); } finally { setCloudLoading(null); }
  }

  async function handleDisconnectCloudSession() {
    if (!googleDrive) return;
    try { setCloudLoading("disconnect"); await disconnectProviderSession(googleDrive.id); await refreshCloud(); }
    catch (err) { setError(String(err)); } finally { setCloudLoading(null); }
  }

  async function handleForgetCloud() {
    if (!googleDrive || !window.confirm("Forget Google Drive metadata? Nothing will be deleted from Drive.")) return;
    try {
      setCloudLoading("forget"); await forgetProvider(googleDrive.id);
      setProviders([]); setBindings({});
      if (selected) setRemotePathDraft(`AtrisBridge/${remoteSegment(selected.name)}-${selected.id.slice(0, 8)}`);
    } catch (err) { setError(String(err)); } finally { setCloudLoading(null); }
  }

  async function handleBindRemote() {
    if (!selected || !googleDrive) return;
    try {
      setCloudLoading("bind"); setError(null);
      const next = await bindWorkspaceRemote(selected.id, googleDrive.id, remotePathDraft);
      setBindings((current) => ({ ...current, [selected.id]: next })); setRemotePathDraft(next.remotePath);
    } catch (err) { setError(String(err)); } finally { setCloudLoading(null); }
  }

  async function handleRemoteScan() {
    if (!selected) return;
    try {
      setCloudLoading("scan"); setError(null);
      const next = await scanRemoteInventory(selected.id);
      setRemoteReports((current) => ({ ...current, [selected.id]: next }));
      await Promise.all([refreshWorkspaces(), refreshBinding(selected)]); setSelectedId(selected.id);
    } catch (err) { setError(String(err)); } finally { setCloudLoading(null); }
  }

  const totalKnownBytes = Object.values(summaries).reduce((sum, item) => sum + item.presentBytes, 0);
  const totalKnownFiles = Object.values(summaries).reduce((sum, item) => sum + item.presentFiles, 0);

  return <div className="app-shell">
    <aside className="sidebar">
      <div className="brand"><div className="brand-mark" aria-hidden="true"><span/><span/></div><div><strong>AtrisBridge</strong><small>Project continuity</small></div></div>
      <nav className="primary-nav"><button className="nav-item active"><Gauge size={17}/> Overview</button><button className="nav-item" disabled><ArrowUpFromLine size={17}/> Transfers <span className="soon">Soon</span></button><button className="nav-item" disabled><TriangleAlert size={17}/> Conflicts <span className="soon">Soon</span></button></nav>
      <div className="sidebar-section"><div className="section-heading"><span>Workspaces</span><button className="icon-button" onClick={handleAddWorkspace}><Plus size={16}/></button></div><div className="workspace-nav">
        {workspaces.length === 0 ? <p className="sidebar-empty">No workspaces yet.</p> : workspaces.map((workspace) => <button key={workspace.id} className={`workspace-nav-item ${selected?.id === workspace.id ? "selected" : ""}`} onClick={() => setSelectedId(workspace.id)}><span className="workspace-dot"/><span className="workspace-nav-copy"><strong>{workspace.name}</strong><small>{workspace.syncMode}</small></span><ChevronRight size={14}/></button>)}
      </div></div>
      <button className="nav-item settings-link" disabled><Settings size={17}/> Settings <span className="soon">Soon</span></button>
    </aside>

    <main className="content">
      <header className="topbar"><div><p className="eyebrow">Local-first workspace protection</p><h1>Overview</h1></div><button className="primary-button" onClick={handleAddWorkspace} disabled={loading}><Plus size={16}/> Add workspace</button></header>
      {error && <div className="error-banner"><TriangleAlert size={17}/><span>{error}</span><button onClick={() => setError(null)}>Dismiss</button></div>}
      <section className="metric-grid"><article className="metric-card"><span className="metric-icon"><Box size={19}/></span><div><small>Workspaces</small><strong>{workspaces.length}</strong></div></article><article className="metric-card"><span className="metric-icon"><FileCode2 size={19}/></span><div><small>Indexed files</small><strong>{totalKnownFiles.toLocaleString()}</strong></div></article><article className="metric-card"><span className="metric-icon"><HardDrive size={19}/></span><div><small>Indexed size</small><strong>{formatBytes(totalKnownBytes)}</strong></div></article><article className="metric-card safe"><span className="metric-icon"><ShieldCheck size={19}/></span><div><small>Journal safety</small><strong>Durable</strong></div></article></section>

      <section className="cloud-card">
        <div className="cloud-card-header"><div className="cloud-title"><span className="cloud-icon"><CloudCog size={19}/></span><div><p className="eyebrow">Phase 3 transport</p><h2>Google Drive</h2></div></div><span className="read-only-pill"><ShieldCheck size={12}/> Observation only</span></div>
        <div className="cloud-grid">
          <div className="cloud-runtime"><small>rclone sidecar</small><strong>{rcloneStatus?.available ? `v${rcloneStatus.version}` : "Not prepared"}</strong><span>{rcloneStatus?.available ? `${rcloneStatus.source} · pinned v${rcloneStatus.requiredVersion}` : rcloneStatus?.message ?? "Checking runtime…"}</span></div>
          <div className="cloud-provider"><div><small>Provider session</small><strong>{googleDrive?.accountLabel ?? googleDrive?.displayName ?? "Not connected"}</strong><span>{googleDrive?.sessionActive ? "OAuth session active in memory" : googleDrive ? "Reconnect after app restart" : "Restricted drive.file scope"}</span></div><div className="cloud-provider-actions"><button className="secondary-button" onClick={handleConnectGoogleDrive} disabled={!rcloneStatus?.available || cloudLoading !== null}>{cloudLoading === "connect" ? <RefreshCw className="spin" size={15}/> : <Cloud size={15}/>} {googleDrive ? "Reconnect" : "Connect"}</button>{googleDrive?.sessionActive && <button className="icon-button cloud-action" onClick={handleDisconnectCloudSession}><Unplug size={15}/></button>}{googleDrive && <button className="icon-button cloud-action danger-icon" onClick={handleForgetCloud}><Trash2 size={15}/></button>}</div></div>
        </div>
        {selected && googleDrive && <div className="remote-binding-row"><div className="remote-binding-copy"><Link2 size={16}/><div><strong>{selected.name} remote folder</strong><span>Phase 3 never creates the folder or transfers files.</span></div></div><div className="remote-binding-controls"><input value={remotePathDraft} onChange={(event) => setRemotePathDraft(event.target.value)} spellCheck={false}/><button className="secondary-button" onClick={handleBindRemote} disabled={cloudLoading !== null}><Link2 size={14}/> {binding ? "Update binding" : "Bind folder"}</button><button className="primary-button" onClick={handleRemoteScan} disabled={!binding || !googleDrive.sessionActive || cloudLoading !== null}>{cloudLoading === "scan" ? <RefreshCw className="spin" size={14}/> : <ScanSearch size={14}/>} Scan remote</button></div><div className="remote-binding-status"><span>{binding ? `Bound to ${binding.remotePath}` : "Save a dedicated folder mapping first."}</span><span>{remoteReport ? `${remoteReport.fileCount.toLocaleString()} remote files · ${formatBytes(remoteReport.totalBytes)}` : binding?.lastInventoryAt ? `Last inventory ${formatDate(binding.lastInventoryAt)}` : "Remote inventory not read yet"}</span></div></div>}
        <p className="cloud-security-note"><ShieldCheck size={13}/> OAuth tokens stay in process memory only; SQLite and rclone.conf never receive them in Phase 3.</p>
      </section>

      {selected ? <section className="workspace-panel">
        <div className="workspace-header"><div className="workspace-title-block"><div className="workspace-avatar"><FolderOpen size={22}/></div><div><div className="title-row"><h2>{selected.name}</h2><span className="status-pill"><CheckCircle2 size={13}/> Local</span></div><p>{selected.localPath}</p></div></div><div className="workspace-actions"><button className="secondary-button" onClick={handleCreateIgnore} disabled={loading}><ShieldCheck size={16}/> Create ignore file</button><button className="primary-button" onClick={handleScan} disabled={loading}>{loading ? <RefreshCw className="spin" size={16}/> : <ScanSearch size={16}/>} Scan workspace</button></div></div>
        <div className="workspace-meta-grid"><div><small>Mode</small><strong>Backup</strong><span>Transfers unlock in Phase 4.</span></div><div><small>Last scan</small><strong>{formatDate(journal?.lastScanAt ?? report?.scannedAt ?? selected.lastScanAt)}</strong><span>BLAKE3 local inventory</span></div><div><small>Journal</small><strong>{journal?.presentFiles.toLocaleString() ?? "—"}</strong><span>{journal?.changedFiles ?? 0} changed · SQLite state</span></div><div><small>Safety queue</small><strong>{journal?.tombstones ?? 0} tombstones</strong><span>{journal?.conflicts ?? 0} conflicts · {journal?.pendingOperations ?? 0} queued</span></div></div>
        <div className="inventory-card"><div className="inventory-heading"><div><p className="eyebrow">Inventory preview</p><h3>{report ? `${report.fileCount.toLocaleString()} files · ${formatBytes(report.totalBytes)}` : journal?.lastScanAt ? `${journal.presentFiles.toLocaleString()} files persisted` : "Scan to build local inventory"}</h3></div>{report && <span className="duration">{report.durationMs} ms</span>}</div>{!report ? <div className="empty-state"><ScanSearch size={28}/><strong>{journal?.lastScanAt ? "Inventory is persisted" : "No inventory yet"}</strong><p>Local scans are journaled before any future cloud operation is allowed.</p><button className="secondary-button" onClick={handleScan}>Run scan</button></div> : <div className="file-table-wrap"><table className="file-table"><thead><tr><th>Path</th><th>Size</th><th>BLAKE3</th></tr></thead><tbody>{report.files.map((file) => <tr key={file.relativePath}><td><FileCode2 size={14}/><span>{file.relativePath}</span></td><td>{formatBytes(file.size)}</td><td><code>{file.blake3.slice(0, 16)}…</code></td></tr>)}</tbody></table>{report.previewTruncated && <p className="table-note">Preview limited to 250 entries; SQLite contains the complete inventory.</p>}</div>}</div>
        <div className="danger-row"><div><strong>Remove workspace</strong><span>Only AtrisBridge metadata is removed; project files are untouched.</span></div><button className="danger-button" onClick={handleRemove}><Trash2 size={15}/> Remove</button></div>
      </section> : <section className="welcome-card"><div className="welcome-icon"><ShieldCheck size={30}/></div><p className="eyebrow">Start safely</p><h2>Add your first project workspace</h2><p>Build a local BLAKE3 inventory before connecting any transport provider.</p><button className="primary-button" onClick={handleAddWorkspace}><Plus size={16}/> Choose project folder</button></section>}
    </main>
  </div>;
}
