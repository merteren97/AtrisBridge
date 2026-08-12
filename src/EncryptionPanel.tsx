import { useEffect, useState } from "react";
import { Copy, KeyRound, LockKeyhole, RefreshCw, ShieldCheck, TriangleAlert } from "lucide-react";
import {
  enableWorkspaceEncryption,
  exportWorkspaceRecoveryKey,
  getWorkspaceEncryptionStatus,
  importWorkspaceRecoveryKey,
} from "./lib/bridge";
import type { WorkspaceEncryptionStatus } from "./types";

interface EncryptionPanelProps {
  workspaceId: string;
  ready: boolean;
  onError: (message: string) => void;
}

export default function EncryptionPanel({ workspaceId, ready, onError }: EncryptionPanelProps) {
  const [status, setStatus] = useState<WorkspaceEncryptionStatus | null>(null);
  const [busy, setBusy] = useState<"enable" | "import" | "export" | null>(null);
  const [importKey, setImportKey] = useState("");
  const [revealedKey, setRevealedKey] = useState<string | null>(null);

  useEffect(() => {
    setRevealedKey(null);
    setImportKey("");
    void refresh();
  }, [workspaceId]);

  async function refresh() {
    try {
      setStatus(await getWorkspaceEncryptionStatus(workspaceId));
    } catch (error) {
      onError(String(error));
    }
  }

  async function handleEnable() {
    if (!window.confirm(
      "Enable client-side content encryption for this workspace? The managed remote root must be empty and existing plaintext baselines cannot be migrated automatically.",
    )) return;

    try {
      setBusy("enable");
      const result = await enableWorkspaceEncryption(workspaceId);
      setStatus(result.status);
      setRevealedKey(result.recoveryKey);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleImport() {
    if (!importKey.trim()) return;
    try {
      setBusy("import");
      const next = await importWorkspaceRecoveryKey(workspaceId, importKey.trim());
      setStatus(next);
      setRevealedKey(null);
      setImportKey("");
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleExport() {
    try {
      setBusy("export");
      setRevealedKey(await exportWorkspaceRecoveryKey(workspaceId));
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleCopy() {
    if (!revealedKey) return;
    try {
      await navigator.clipboard.writeText(revealedKey);
    } catch {
      onError("Could not copy the recovery key. Select and copy it manually.");
    }
  }

  const enabled = status?.mode === "content";
  const keyMissing = enabled && status?.keyAvailable === false;

  return (
    <section className={`ab-encryption-panel ${enabled ? "enabled" : "disabled"}`}>
      <header className="ab-encryption-heading">
        <span className="ab-encryption-icon"><LockKeyhole size={18} /></span>
        <div>
          <span className="ab-kicker">Protection</span>
          <h3>Client-side encryption</h3>
          <p>{enabled ? "File contents are encrypted locally before Google Drive receives them." : "Optional protection for new remote baselines. Enable it before the first synchronized upload."}</p>
        </div>
        <span className={`ab-encryption-badge ${enabled ? "enabled" : ""}`}>
          {enabled ? <ShieldCheck size={13} /> : <KeyRound size={13} />}
          {enabled ? "Encrypted" : "Off"}
        </span>
      </header>

      {enabled ? (
        <div className="ab-encryption-content">
          <div className="ab-encryption-status-list">
            <div><span>Key</span><strong>{status?.keyAvailable ? "Available in OS vault" : "Recovery key required"}</strong></div>
            <div><span>Filenames</span><strong>{status?.filenameEncrypted ? "Encrypted" : "Visible"}</strong></div>
            <div><span>Remote namespace</span><strong title={status?.remoteNamespace ?? undefined}>{status?.remoteNamespace ?? "—"}</strong></div>
          </div>

          {keyMissing ? (
            <div className="ab-encryption-recovery">
              <div className="ab-encryption-callout warning"><TriangleAlert size={15} /><span>The encryption key is missing from this device. Import the matching recovery key before remote content can be decrypted or synchronized.</span></div>
              <label className="ab-encryption-key-field"><span>Recovery key</span><input type="password" value={importKey} onChange={(event) => setImportKey(event.target.value)} placeholder="AB1-…" spellCheck={false} autoComplete="off" /></label>
              <button className="ab-button secondary ab-encryption-full-button" onClick={handleImport} disabled={!ready || !importKey.trim() || busy !== null}>{busy === "import" ? <RefreshCw className="spin" size={15} /> : <KeyRound size={15} />} Import recovery key</button>
            </div>
          ) : (
            <button className="ab-button secondary ab-encryption-full-button" onClick={handleExport} disabled={busy !== null || !status?.keyAvailable}>{busy === "export" ? <RefreshCw className="spin" size={15} /> : <KeyRound size={15} />} Export recovery key</button>
          )}
        </div>
      ) : (
        <div className="ab-encryption-content">
          <div className="ab-encryption-callout"><ShieldCheck size={15} /><span>Content encryption keeps file data private while filenames and folders remain visible for exact conflict tracking.</span></div>
          <button className="ab-button secondary ab-encryption-full-button" onClick={handleEnable} disabled={!ready || busy !== null}>{busy === "enable" ? <RefreshCw className="spin" size={15} /> : <LockKeyhole size={15} />} Enable encryption</button>
          <div className="ab-encryption-divider"><span>or restore an encrypted workspace</span></div>
          <label className="ab-encryption-key-field"><span>Recovery key</span><input type="password" value={importKey} onChange={(event) => setImportKey(event.target.value)} placeholder="AB1-…" spellCheck={false} autoComplete="off" /></label>
          <button className="ab-button secondary ab-encryption-full-button" onClick={handleImport} disabled={!ready || !importKey.trim() || busy !== null}>{busy === "import" ? <RefreshCw className="spin" size={15} /> : <KeyRound size={15} />} Import recovery key</button>
        </div>
      )}

      {revealedKey && (
        <div className="ab-encryption-revealed-key">
          <div><strong>Recovery key</strong><span>Store this outside the synchronized workspace. Anyone with this key and the encrypted Drive data can decrypt file contents.</span></div>
          <div className="ab-encryption-key-value"><code>{revealedKey}</code><button type="button" onClick={handleCopy} title="Copy recovery key" aria-label="Copy recovery key"><Copy size={15} /></button></div>
        </div>
      )}
    </section>
  );
}
