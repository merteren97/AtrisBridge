import { useState } from "react";
import {
  Bell,
  BellOff,
  Bot,
  Cloud,
  CloudCog,
  Database,
  Download,
  FolderSync,
  HardDrive,
  Minimize2,
  Power,
  RefreshCw,
  Rocket,
  ShieldCheck,
  SlidersHorizontal,
  Trash2,
  Unplug,
} from "lucide-react";
import AiGatewayPanel from "../AiGatewayPanel";
import AiPolicyPresetPanel from "../AiPolicyPresetPanel";
import { activityAlertsEnabled, setActivityAlertsEnabled } from "../activity-preferences";
import { useUpdater, type UpdateBehavior, type UpdateStatus } from "../UpdateCenter";
import type { ProductModel } from "./useProductModel";

function updateStatusLabel(status: UpdateStatus) {
  switch (status) {
    case "checking": return "Checking…";
    case "available": return "Update available";
    case "up-to-date": return "Up to date";
    case "downloading": return "Downloading…";
    case "installing": return "Installing…";
    case "error": return "Update error";
    default: return "Ready";
  }
}

const updateOptions: Array<{ value: UpdateBehavior; title: string; description: string; icon: typeof Bell }> = [
  {
    value: "notify",
    title: "Notify before installing",
    description: "Check on startup and show a notification only when a newer signed AtrisBridge release is available.",
    icon: Bell,
  },
  {
    value: "automatic",
    title: "Install automatically",
    description: "Download and install signed updates automatically. AtrisBridge waits for active sync cycles before restarting.",
    icon: Rocket,
  },
];

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
  const updater = useUpdater();
  const [alertsEnabled, setAlertsEnabledState] = useState(activityAlertsEnabled);

  async function setAlerts(next: boolean) {
    if (next && "Notification" in window && Notification.permission === "default") {
      const permission = await Notification.requestPermission();
      if (permission === "denied") return;
    }
    setActivityAlertsEnabled(next);
    setAlertsEnabledState(next);
  }

  const updateBusy = updater.status === "checking" || updater.status === "downloading" || updater.status === "installing";

  return (
    <div className="ab-view ab-settings-layout">
      <aside className="ab-settings-index" aria-label="Settings categories">
        <a href="#general"><SlidersHorizontal size={16} /> General</a>
        <a href="#connections"><CloudCog size={16} /> Connections</a>
        <a href="#ai-clients"><Bot size={16} /> AI clients</a>
        <a href="#updates"><RefreshCw size={16} /> Updates</a>
        <a href="#notifications"><Bell size={16} /> Notifications</a>
        <a href="#security"><ShieldCheck size={16} /> Security</a>
        <a href="#advanced"><HardDrive size={16} /> Advanced</a>
      </aside>

      <div className="ab-settings-sheet ab-settings-sheet-refined">
        <header className="ab-settings-intro">
          <span className="ab-kicker">Application settings</span>
          <h2>AtrisBridge preferences</h2>
          <p>Control window behavior, cloud connections, AI workspace access, notifications, updates and local protection without leaving the desktop workflow.</p>
        </header>

        <section id="general" className="ab-settings-section">
          <header><span className="ab-kicker">General</span><h2>Window &amp; background behavior</h2><p>Choose what the Windows close button should do.</p></header>
          <div className="ab-choice-grid two">
            <button type="button" className={!closeToTray ? "ab-choice-card active" : "ab-choice-card"} onClick={() => setCloseToTray(false)} aria-pressed={!closeToTray}>
              <span className="ab-choice-icon"><Power size={18} /></span>
              <span className="ab-choice-copy"><strong>Quit AtrisBridge</strong><small>Close the window and stop the desktop process completely.</small></span>
              {!closeToTray && <span className="ab-choice-active">Active</span>}
            </button>
            <button type="button" className={closeToTray ? "ab-choice-card active" : "ab-choice-card"} onClick={() => setCloseToTray(true)} aria-pressed={closeToTray}>
              <span className="ab-choice-icon"><Minimize2 size={18} /></span>
              <span className="ab-choice-copy"><strong>Minimize to tray</strong><small>Hide the window while background synchronization keeps running.</small></span>
              {closeToTray && <span className="ab-choice-active">Active</span>}
            </button>
          </div>
        </section>

        <section id="connections" className="ab-settings-section">
          <header><span className="ab-kicker">Connections</span><h2>Google Drive</h2><p>Remote transport is configured independently from your AtrisHub account.</p></header>
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

        <AiGatewayPanel workspaces={model.workspaces} onError={model.setError} />
        <AiPolicyPresetPanel workspaces={model.workspaces} onError={model.setError} />

        <section id="updates" className="ab-settings-section">
          <header className="ab-settings-section-heading-row">
            <div><span className="ab-kicker">Updates</span><h2>Application updates</h2><p>Signed releases are checked through the configured AtrisBridge update channel.</p></div>
            <div className="ab-update-badges"><span>v{updater.runtime?.currentVersion ?? "—"}</span><span className={updater.status === "error" ? "error" : ""}>{updateStatusLabel(updater.status)}</span></div>
          </header>
          <div className="ab-choice-grid two">
            {updateOptions.map((option) => {
              const Icon = option.icon;
              const active = updater.behavior === option.value;
              return (
                <button type="button" key={option.value} className={active ? "ab-choice-card active" : "ab-choice-card"} onClick={() => updater.setBehavior(option.value)} aria-pressed={active}>
                  <span className="ab-choice-icon"><Icon size={18} /></span>
                  <span className="ab-choice-copy"><strong>{option.title}</strong><small>{option.description}</small></span>
                  {active && <span className="ab-choice-active">Active</span>}
                </button>
              );
            })}
          </div>
          <div className="ab-setting-row ab-update-setting-row">
            <div><strong>{updater.update ? `AtrisBridge ${updater.update.version} is available` : "Update channel"}</strong><p>{updater.deferredAutomatic ? "Automatic installation is waiting for active synchronization to become idle." : updater.error ? updater.error : updater.runtime?.configured ? `${updater.runtime.channel} channel · current ${updater.runtime.currentVersion}` : "Updater signing is not configured in this development build."}</p></div>
            <div className="ab-settings-actions">
              <button className="ab-button secondary" disabled={updateBusy || !updater.runtime?.configured} onClick={() => void updater.checkForUpdates(true)}><RefreshCw className={updater.status === "checking" ? "spin" : ""} size={15} /> Check now</button>
              {updater.update && updater.status === "available" && <button className="ab-button primary" onClick={() => void updater.installAvailableUpdate(false)}><Download size={15} /> Update to {updater.update.version}</button>}
            </div>
          </div>
        </section>

        <section id="notifications" className="ab-settings-section">
          <header><span className="ab-kicker">Notifications</span><h2>Activity alerts</h2><p>Live synchronization remains visible in Activity. Desktop notifications are optional.</p></header>
          <div className="ab-setting-row">
            <div className="ab-runtime-copy"><span className={`ab-runtime-icon ${alertsEnabled ? "connected" : ""}`}>{alertsEnabled ? <Bell size={18} /> : <BellOff size={18} />}</span><div><strong>Desktop sync notifications</strong><p>Notify when a sync finishes or when a workspace needs attention while AtrisBridge is in the background.</p></div></div>
            <label className="ab-toggle"><input type="checkbox" checked={alertsEnabled} onChange={(event) => void setAlerts(event.target.checked)} /><span /></label>
          </div>
        </section>

        <section id="security" className="ab-settings-section">
          <header><span className="ab-kicker">Security</span><h2>Protection model</h2></header>
          <div className="ab-security-list">
            <div><span><ShieldCheck size={18} /></span><div><strong>OS secure vault</strong><p>Persisted provider and workspace encryption credentials stay outside frontend storage.</p></div></div>
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
