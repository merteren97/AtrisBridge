import { Cloud, CloudCog, Database, FolderSync, HardDrive, RefreshCw, ShieldCheck, SlidersHorizontal, Trash2, Unplug } from "lucide-react";
import type { ProductModel } from "./useProductModel";

export default function SettingsView({ model }: { model: ProductModel }) {
  const {
    closeToTray,
    setCloseToTray,
    googleDrive,
    rcloneStatus,
    cloudLoading,
    handleConnectGoogleDrive,
    handleDisconnectCloudSession,
    handleForgetCloud,
    refreshCloud,
  } = model;

  return (
    <div className="ab-view ab-settings-layout">
      <aside className="ab-settings-index" aria-label="Settings categories">
        <a href="#general"><SlidersHorizontal size={16} /> General</a>
        <a href="#connections"><CloudCog size={16} /> Connections</a>
        <a href="#security"><ShieldCheck size={16} /> Security</a>
        <a href="#advanced"><HardDrive size={16} /> Advanced</a>
      </aside>

      <div className="ab-settings-sheet">
        <section className="ab-settings-brand">
          <img src="/brand/atrisbridge-mark.svg" alt="AtrisBridge" />
          <div><span className="ab-kicker">AtrisBridge</span><h2>Project continuity, without the noise.</h2><p>Desktop behavior, cloud transport and safety controls live here. Technical diagnostics stay out of the primary workspace experience.</p></div>
        </section>

        <section id="general" className="ab-settings-section">
          <header><span className="ab-kicker">General</span><h2>Desktop behavior</h2></header>
          <div className="ab-setting-row">
            <div><strong>Keep AtrisBridge running in the tray</strong><p>Continue background synchronization after closing the main window. Leave this off if closing the window should fully quit AtrisBridge.</p></div>
            <label className="ab-toggle"><input type="checkbox" checked={closeToTray} onChange={(event) => setCloseToTray(event.target.checked)} /><span /></label>
          </div>
        </section>

        <section id="connections" className="ab-settings-section">
          <header><span className="ab-kicker">Connections</span><h2>Google Drive</h2></header>
          <div className="ab-setting-row connection">
            <span className={`ab-settings-provider ${googleDrive?.sessionActive ? "connected" : ""}`}><Cloud size={20} /></span>
            <div><strong>{googleDrive?.accountLabel ?? googleDrive?.displayName ?? "Google Drive"}</strong><p>{googleDrive?.credentialPersisted ? "Credential is protected by the operating-system secure vault." : googleDrive?.sessionActive ? "Connected for this session. Secure persistence is unavailable." : "Connect only when you want remote transport for a workspace."}</p></div>
            <div className="ab-settings-actions">
              <button className={googleDrive?.sessionActive ? "ab-button secondary" : "ab-button primary"} onClick={handleConnectGoogleDrive} disabled={!rcloneStatus?.available || cloudLoading !== null}>{cloudLoading === "connect" ? <RefreshCw className="spin" size={15} /> : <Cloud size={15} />}{googleDrive ? "Reconnect" : "Connect"}</button>
              {googleDrive?.sessionActive && <button className="ab-icon-button" onClick={handleDisconnectCloudSession} aria-label="Disconnect Google Drive" title="Disconnect"><Unplug size={16} /></button>}
              {googleDrive && <button className="ab-icon-button danger" onClick={handleForgetCloud} aria-label="Forget Google Drive connection" title="Forget connection"><Trash2 size={16} /></button>}
            </div>
          </div>
        </section>

        <section id="security" className="ab-settings-section">
          <header><span className="ab-kicker">Security</span><h2>Protection model</h2></header>
          <div className="ab-security-list">
            <div><span><ShieldCheck size={18} /></span><div><strong>OS secure vault</strong><p>Persisted provider credentials stay outside frontend storage.</p></div></div>
            <div><span><Database size={18} /></span><div><strong>Durable local journal</strong><p>Changes and synchronization decisions remain inspectable before remote execution.</p></div></div>
            <div><span><FolderSync size={18} /></span><div><strong>Workspace-scoped transport</strong><p>Each project uses an explicit remote folder mapping instead of an implicit global destination.</p></div></div>
          </div>
        </section>

        <section id="advanced" className="ab-settings-section">
          <header><span className="ab-kicker">Advanced</span><h2>Runtime diagnostics</h2></header>
          <div className="ab-setting-row">
            <div className="ab-runtime-copy"><span className={`ab-runtime-icon ${rcloneStatus?.available ? "connected" : "offline"}`}><HardDrive size={18} /></span><div><strong>{rcloneStatus?.available ? `rclone v${rcloneStatus.version}` : "Transfer runtime unavailable"}</strong><p>{rcloneStatus?.available ? `${rcloneStatus.source} runtime · required v${rcloneStatus.requiredVersion}` : rcloneStatus?.message ?? "Runtime status is still loading."}</p></div></div>
            <button className="ab-button secondary" onClick={() => void refreshCloud()} disabled={cloudLoading !== null}><RefreshCw size={15} /> Refresh</button>
          </div>
        </section>
      </div>
    </div>
  );
}
