import {
  ArrowRight,
  CheckCircle2,
  CloudCog,
  Database,
  FileCode2,
  FolderOpen,
  Link2,
  RefreshCw,
  ScanSearch,
  ShieldCheck,
  Trash2,
} from "lucide-react";
import BackupPanel from "../BackupPanel";
import EncryptionPanel from "../EncryptionPanel";
import type { ProductModel } from "./useProductModel";
import { formatBytes, formatDate, syncModeLabel, workspaceState } from "./useProductModel";

interface WorkspaceViewProps {
  model: ProductModel;
}

export default function WorkspaceView({ model }: WorkspaceViewProps) {
  const {
    selected,
    report,
    remoteReport,
    journal,
    binding,
    googleDrive,
    rcloneStatus,
    loading,
    cloudLoading,
    workspaceSection,
    inspectorOpen,
    remotePathDraft,
    backupReady,
    encryptionReady,
    setWorkspaceSection,
    setInspectorOpen,
    setRemotePathDraft,
    setView,
    setError,
    handleScan,
    handleCreateIgnore,
    handleBindRemote,
    handleRemoteScan,
    handleRemove,
    refreshAfterBackupChange,
  } = model;

  if (!selected) return null;
  const state = workspaceState(journal, binding);

  return (
    <div className="ab-view ab-workspace-view">
      <section className="ab-workspace-context">
        <div className="ab-workspace-context-main">
          <span className="ab-workspace-large-icon"><FolderOpen size={21} /></span>
          <div>
            <div className="ab-workspace-title-line"><strong>{selected.name}</strong><span>{syncModeLabel(selected.syncMode)}</span><span className={`ab-status-pill ${state.tone}`}><i />{state.label}</span></div>
            <p>{selected.localPath}</p>
          </div>
        </div>
        <div className="ab-context-facts">
          <span><small>Last scan</small><strong>{formatDate(journal?.lastScanAt ?? report?.scannedAt ?? selected.lastScanAt)}</strong></span>
          <span><small>Files</small><strong>{journal?.presentFiles.toLocaleString() ?? report?.fileCount.toLocaleString() ?? "—"}</strong></span>
          <span><small>Waiting</small><strong>{journal?.pendingOperations ?? 0}</strong></span>
          <span className={(journal?.conflicts ?? 0) > 0 ? "attention" : ""}><small>Conflicts</small><strong>{journal?.conflicts ?? 0}</strong></span>
        </div>
      </section>

      <div className="ab-workspace-tabs" role="tablist" aria-label="Workspace sections">
        <button className={workspaceSection === "files" ? "active" : ""} onClick={() => setWorkspaceSection("files")}>Files</button>
        <button className={workspaceSection === "protection" ? "active" : ""} onClick={() => setWorkspaceSection("protection")}>Sync &amp; backup</button>
        <span />
        <button className="ab-tab-action" onClick={handleCreateIgnore} disabled={loading}><ShieldCheck size={15} /> Ignore rules</button>
      </div>

      {workspaceSection === "files" ? (
        <div className={`ab-workspace-layout ${inspectorOpen ? "with-inspector" : ""}`}>
          <section className="ab-sheet ab-inventory-sheet">
            <header className="ab-sheet-header">
              <div><span className="ab-kicker">Local files</span><h2>{report ? `${report.fileCount.toLocaleString()} files · ${formatBytes(report.totalBytes)}` : journal?.lastScanAt ? `${journal.presentFiles.toLocaleString()} indexed files` : "Inventory not built yet"}</h2></div>
              <div className="ab-sheet-actions">{report && <span className="ab-subtle-chip">{report.durationMs} ms</span>}<button className="ab-button secondary" onClick={handleScan} disabled={loading}>{loading ? <RefreshCw className="spin" size={15} /> : <ScanSearch size={15} />} Scan now</button></div>
            </header>

            {!report ? (
              <div className="ab-inventory-empty">
                <Database size={25} />
                <div><strong>{journal?.lastScanAt ? "A local inventory is already saved" : "Scan once to establish the local baseline"}</strong><p>{journal?.lastScanAt ? "Run a scan when you want a fresh file preview." : "AtrisBridge builds local evidence before planning any cloud operation."}</p></div>
                <button className="ab-button secondary" onClick={handleScan}>Run scan</button>
              </div>
            ) : (
              <div className="ab-file-table-wrap">
                <table className="ab-file-table">
                  <thead><tr><th>Path</th><th>Size</th><th>Fingerprint</th></tr></thead>
                  <tbody>{report.files.map((file) => (
                    <tr key={file.relativePath}><td><FileCode2 size={14} /><span>{file.relativePath}</span></td><td>{formatBytes(file.size)}</td><td><code>{file.blake3.slice(0, 16)}…</code></td></tr>
                  ))}</tbody>
                </table>
                {report.previewTruncated && <p className="ab-table-note">Preview limited to 250 entries. The complete inventory remains in the local journal.</p>}
              </div>
            )}
          </section>

          {inspectorOpen && (
            <aside className="ab-inspector">
              <div className="ab-inspector-header"><div><span className="ab-kicker">Details</span><h2>Workspace</h2></div><button type="button" onClick={() => setInspectorOpen(false)} aria-label="Close workspace details">×</button></div>

              <section className="ab-inspector-section">
                {!googleDrive ? (
                  <div className="ab-inspector-empty"><span><CloudCog size={21} /></span><strong>Google Drive is not connected</strong><p>Connect an account in Settings before mapping this project.</p><button className="ab-button secondary" onClick={() => setView("settings")}>Open Settings</button></div>
                ) : (
                  <>
                    <div className="ab-inspector-status"><span className={googleDrive.sessionActive ? "online" : "offline"} /><div><strong>{googleDrive.accountLabel ?? googleDrive.displayName ?? "Google Drive"}</strong><small>{googleDrive.sessionActive ? "Connected" : "Reconnect required"}</small></div></div>
                    <label className="ab-field"><span>Remote folder</span><input value={remotePathDraft} onChange={(event) => setRemotePathDraft(event.target.value)} spellCheck={false} /><small>Dedicated Drive folder for this workspace.</small></label>
                    <div className="ab-inspector-actions"><button className="ab-button secondary" onClick={handleBindRemote} disabled={cloudLoading !== null}><Link2 size={15} /> {binding ? "Update" : "Bind"}</button><button className="ab-button primary" onClick={handleRemoteScan} disabled={!binding || !googleDrive.sessionActive || cloudLoading !== null}>{cloudLoading === "scan" ? <RefreshCw className="spin" size={15} /> : <ScanSearch size={15} />} Scan remote</button></div>
                    <div className="ab-mapping-status"><span className={binding ? "healthy" : "neutral"}>{binding ? <CheckCircle2 size={14} /> : <Link2 size={14} />}{binding ? "Mapped" : "Not mapped"}</span><p>{binding?.remotePath ?? "No remote folder selected."}</p><small>{remoteReport ? `${remoteReport.fileCount.toLocaleString()} remote files · ${formatBytes(remoteReport.totalBytes)}` : binding?.lastInventoryAt ? `Last remote scan ${formatDate(binding.lastInventoryAt)}` : "Remote inventory not read yet"}</small></div>
                  </>
                )}
              </section>

              {binding && googleDrive && <div className="ab-inspector-section"><EncryptionPanel workspaceId={selected.id} ready={encryptionReady} onError={(message) => setError(message)} /></div>}
              <div className="ab-inspector-danger"><div><strong>Remove workspace</strong><small>Project files remain untouched.</small></div><button className="ab-danger-link" onClick={handleRemove}><Trash2 size={14} /> Remove</button></div>
            </aside>
          )}
        </div>
      ) : (
        <div className="ab-protection-layout">
          <div className="ab-protection-main"><BackupPanel workspace={selected} ready={backupReady} onChanged={refreshAfterBackupChange} onError={(message) => setError(message)} /></div>
          <aside className="ab-protection-aside">
            <section className="ab-side-panel">
              <header><span className="ab-kicker">Preconditions</span><h2>Sync readiness</h2></header>
              <div className="ab-readiness-row"><span className={rcloneStatus?.available ? "online" : "offline"} /><div><strong>Transfer service</strong><small>{rcloneStatus?.available ? "Ready" : "Unavailable"}</small></div></div>
              <div className="ab-readiness-row"><span className={googleDrive?.sessionActive ? "online" : "offline"} /><div><strong>Google Drive</strong><small>{googleDrive?.sessionActive ? "Connected" : "Not connected"}</small></div></div>
              <div className="ab-readiness-row"><span className={binding ? "online" : "offline"} /><div><strong>Remote mapping</strong><small>{binding ? "Configured" : "Not configured"}</small></div></div>
              <button className="ab-panel-link" onClick={() => { setWorkspaceSection("files"); setInspectorOpen(true); }}>Connection details <ArrowRight size={15} /></button>
            </section>
          </aside>
        </div>
      )}
    </div>
  );
}
