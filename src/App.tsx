import { useEffect, useMemo, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  Activity,
  ArrowUpFromLine,
  Box,
  CheckCircle2,
  ChevronRight,
  FileCode2,
  FolderOpen,
  Gauge,
  HardDrive,
  Plus,
  RefreshCw,
  ScanSearch,
  Settings,
  ShieldCheck,
  Trash2,
  TriangleAlert,
} from "lucide-react";
import {
  addWorkspace,
  initializeIgnoreFile,
  listWorkspaces,
  removeWorkspace,
  scanWorkspace,
} from "./lib/bridge";
import type { ScanReport, Workspace } from "./types";

function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** index).toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

function formatDate(value: string | null): string {
  if (!value) return "Not scanned yet";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

function fileNameFromPath(path: string): string {
  const segments = path.replace(/\\/g, "/").split("/").filter(Boolean);
  return segments.at(-1) ?? "Workspace";
}

export default function App() {
  const [workspaces, setWorkspaces] = useState<Workspace[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [reports, setReports] = useState<Record<string, ScanReport>>({});
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const selected = useMemo(
    () => workspaces.find((workspace) => workspace.id === selectedId) ?? workspaces[0] ?? null,
    [selectedId, workspaces],
  );
  const report = selected ? reports[selected.id] : undefined;

  useEffect(() => {
    void refreshWorkspaces();
  }, []);

  async function refreshWorkspaces() {
    try {
      setError(null);
      const items = await listWorkspaces();
      setWorkspaces(items);
      setSelectedId((current) => current ?? items[0]?.id ?? null);
    } catch (err) {
      setError(String(err));
    }
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
      setWorkspaces((current) => [...current, workspace]);
      setSelectedId(workspace.id);
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
      const nextReport = await scanWorkspace(selected.id);
      setReports((current) => ({ ...current, [selected.id]: nextReport }));
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
      const created = await initializeIgnoreFile(selected.id);
      if (!created) {
        setError(".atrisbridgeignore already exists; no file was changed.");
      }
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  async function handleRemove() {
    if (!selected) return;
    if (!window.confirm(`Remove ${selected.name} from AtrisBridge? No project files will be deleted.`)) return;
    try {
      setLoading(true);
      setError(null);
      await removeWorkspace(selected.id);
      setReports((current) => {
        const next = { ...current };
        delete next[selected.id];
        return next;
      });
      const remaining = workspaces.filter((workspace) => workspace.id !== selected.id);
      setWorkspaces(remaining);
      setSelectedId(remaining[0]?.id ?? null);
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  }

  const totalKnownBytes = Object.values(reports).reduce((sum, item) => sum + item.totalBytes, 0);
  const totalKnownFiles = Object.values(reports).reduce((sum, item) => sum + item.fileCount, 0);

  return (
    <div className="app-shell">
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark" aria-hidden="true"><span /><span /></div>
          <div><strong>AtrisBridge</strong><small>Project continuity</small></div>
        </div>

        <nav className="primary-nav" aria-label="Primary navigation">
          <button className="nav-item active" type="button"><Gauge size={17} /> Overview</button>
          <button className="nav-item" type="button" disabled><ArrowUpFromLine size={17} /> Transfers <span className="soon">Soon</span></button>
          <button className="nav-item" type="button" disabled><TriangleAlert size={17} /> Conflicts <span className="soon">Soon</span></button>
          <button className="nav-item" type="button" disabled><Activity size={17} /> Activity <span className="soon">Soon</span></button>
        </nav>

        <div className="sidebar-section">
          <div className="section-heading">
            <span>Workspaces</span>
            <button className="icon-button" type="button" onClick={handleAddWorkspace} title="Add workspace"><Plus size={16} /></button>
          </div>
          <div className="workspace-nav">
            {workspaces.length === 0 ? <p className="sidebar-empty">No workspaces yet.</p> : workspaces.map((workspace) => (
              <button key={workspace.id} type="button" className={`workspace-nav-item ${selected?.id === workspace.id ? "selected" : ""}`} onClick={() => setSelectedId(workspace.id)}>
                <span className="workspace-dot" />
                <span className="workspace-nav-copy"><strong>{workspace.name}</strong><small>{workspace.syncMode === "backup" ? "Backup" : workspace.syncMode}</small></span>
                <ChevronRight size={14} />
              </button>
            ))}
          </div>
        </div>
        <button className="nav-item settings-link" type="button" disabled><Settings size={17} /> Settings <span className="soon">Soon</span></button>
      </aside>

      <main className="content">
        <header className="topbar">
          <div><p className="eyebrow">Local-first workspace protection</p><h1>Overview</h1></div>
          <button className="primary-button" type="button" onClick={handleAddWorkspace} disabled={loading}><Plus size={16} /> Add workspace</button>
        </header>

        {error && <div className="error-banner" role="alert"><TriangleAlert size={17} /><span>{error}</span><button type="button" onClick={() => setError(null)}>Dismiss</button></div>}

        <section className="metric-grid" aria-label="Workspace summary">
          <article className="metric-card"><span className="metric-icon"><Box size={19} /></span><div><small>Workspaces</small><strong>{workspaces.length}</strong></div></article>
          <article className="metric-card"><span className="metric-icon"><FileCode2 size={19} /></span><div><small>Indexed files</small><strong>{totalKnownFiles.toLocaleString()}</strong></div></article>
          <article className="metric-card"><span className="metric-icon"><HardDrive size={19} /></span><div><small>Indexed size</small><strong>{formatBytes(totalKnownBytes)}</strong></div></article>
          <article className="metric-card safe"><span className="metric-icon"><ShieldCheck size={19} /></span><div><small>Sync safety</small><strong>Protected</strong></div></article>
        </section>

        {selected ? (
          <section className="workspace-panel">
            <div className="workspace-header">
              <div className="workspace-title-block">
                <div className="workspace-avatar"><FolderOpen size={22} /></div>
                <div><div className="title-row"><h2>{selected.name}</h2><span className="status-pill"><CheckCircle2 size={13} /> Local</span></div><p title={selected.localPath}>{selected.localPath}</p></div>
              </div>
              <div className="workspace-actions">
                <button className="secondary-button" type="button" onClick={handleCreateIgnore} disabled={loading}><ShieldCheck size={16} /> Create ignore file</button>
                <button className="primary-button" type="button" onClick={handleScan} disabled={loading}>{loading ? <RefreshCw className="spin" size={16} /> : <ScanSearch size={16} />} Scan workspace</button>
              </div>
            </div>

            <div className="workspace-meta-grid">
              <div><small>Mode</small><strong>Backup</strong><span>Cloud transport lands in Phase 2.</span></div>
              <div><small>Last scan</small><strong>{formatDate(report?.scannedAt ?? selected.lastScanAt)}</strong><span>BLAKE3 inventory</span></div>
              <div><small>Ignored</small><strong>{report?.skippedEntries ?? "—"}</strong><span>Safe defaults + custom rules</span></div>
              <div><small>Warnings</small><strong>{report?.warnings.length ?? 0}</strong><span>Unreadable entries are never hidden</span></div>
            </div>

            <div className="inventory-card">
              <div className="inventory-heading"><div><p className="eyebrow">Inventory preview</p><h3>{report ? `${report.fileCount.toLocaleString()} files · ${formatBytes(report.totalBytes)}` : "Scan to build a local inventory"}</h3></div>{report && <span className="duration">{report.durationMs} ms</span>}</div>
              {!report ? (
                <div className="empty-state"><ScanSearch size={28} /><strong>No inventory yet</strong><p>The scanner never follows symlinks and applies source-code safety ignores before hashing files.</p><button className="secondary-button" type="button" onClick={handleScan} disabled={loading}>Run first scan</button></div>
              ) : (
                <div className="file-table-wrap">
                  <table className="file-table"><thead><tr><th>Path</th><th>Size</th><th>BLAKE3</th></tr></thead><tbody>{report.files.map((file) => <tr key={file.relativePath}><td><FileCode2 size={14} /><span>{file.relativePath}</span></td><td>{formatBytes(file.size)}</td><td><code>{file.blake3.slice(0, 16)}…</code></td></tr>)}</tbody></table>
                  {report.previewTruncated && <p className="table-note">Preview limited to 250 entries. The summary includes the complete scan.</p>}
                </div>
              )}
            </div>

            <div className="danger-row"><div><strong>Remove workspace</strong><span>This removes AtrisBridge metadata only. Project files are never deleted.</span></div><button className="danger-button" type="button" onClick={handleRemove} disabled={loading}><Trash2 size={15} /> Remove</button></div>
          </section>
        ) : (
          <section className="welcome-card"><div className="welcome-icon"><ShieldCheck size={30} /></div><p className="eyebrow">Start safely</p><h2>Add your first project workspace</h2><p>AtrisBridge will index the folder locally, apply conservative source-code ignores, and prepare it for a later cloud transport connection.</p><button className="primary-button" type="button" onClick={handleAddWorkspace} disabled={loading}><Plus size={16} /> Choose project folder</button></section>
        )}
      </main>
    </div>
  );
}
