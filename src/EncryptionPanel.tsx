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

  function recoveryImport() {
    return (
      <div className="recovery-import">
        <input
          type="password"
          value={importKey}
          onChange={(event) => setImportKey(event.target.value)}
          placeholder="AB1-… recovery key"
          spellCheck={false}
          autoComplete="off"
        />
        <button
          className="secondary-button"
          onClick={handleImport}
          disabled={!ready || !importKey.trim() || busy !== null}
        >
          {busy === "import" ? <RefreshCw className="spin" size={14} /> : <KeyRound size={14} />}
          Import key
        </button>
      </div>
    );
  }

  const enabled = status?.mode === "content";
  const keyMissing = enabled && status?.keyAvailable === false;

  return (
    <section className="encryption-card">
      <div className="encryption-header">
        <div className="encryption-title">
          <span className="encryption-icon"><LockKeyhole size={16} /></span>
          <div>
            <small>Phase 7 protection</small>
            <strong>Client-side encryption</strong>
            <span>
              {enabled
                ? "File contents are encrypted before Google Drive receives them."
                : "Optional. Enable only before the first synchronized baseline is created."}
            </span>
          </div>
        </div>
        <span className={`encryption-state ${enabled ? "enabled" : ""}`}>
          {enabled ? <ShieldCheck size={12} /> : <KeyRound size={12} />}
          {enabled ? "Content encrypted" : "Disabled"}
        </span>
      </div>

      {enabled ? (
        <div className="encryption-body">
          <div className="encryption-facts">
            <div><small>Key</small><strong>{status?.keyAvailable ? "OS vault available" : "Recovery key required"}</strong></div>
            <div><small>Filenames</small><strong>{status?.filenameEncrypted ? "Encrypted" : "Visible"}</strong></div>
            <div><small>Remote namespace</small><strong>{status?.remoteNamespace ?? "—"}</strong></div>
          </div>
          <div className="encryption-actions">
            {keyMissing ? recoveryImport() : (
              <button className="secondary-button" onClick={handleExport} disabled={busy !== null || !status?.keyAvailable}>
                {busy === "export" ? <RefreshCw className="spin" size={14} /> : <KeyRound size={14} />}
                Export recovery key
              </button>
            )}
          </div>
        </div>
      ) : (
        <div className="encryption-body disabled">
          <div className="encryption-warning">
            <TriangleAlert size={15} />
            <span>Phase 7 encrypts file content, but intentionally leaves filenames and directory structure visible so AtrisBridge can keep exact remote evidence and conflict semantics.</span>
          </div>
          <div className="encryption-actions">
            <button className="secondary-button" onClick={handleEnable} disabled={!ready || busy !== null}>
              {busy === "enable" ? <RefreshCw className="spin" size={14} /> : <LockKeyhole size={14} />}
              Enable encryption
            </button>
            {recoveryImport()}
          </div>
        </div>
      )}

      {keyMissing && (
        <div className="recovery-key-box">
          <div>
            <strong>Recovery key required</strong>
            <span>This workspace is encrypted, but the OS vault key is unavailable. Import the matching AB1 recovery key before cloud data can be decrypted or synchronized.</span>
          </div>
        </div>
      )}

      {revealedKey && (
        <div className="recovery-key-box">
          <div>
            <strong>Recovery key</strong>
            <span>Store this outside the synchronized workspace. Anyone with this key and the encrypted Drive data can decrypt the file contents.</span>
          </div>
          <div className="recovery-key-value">
            <code>{revealedKey}</code>
            <button className="icon-button" onClick={handleCopy} title="Copy recovery key"><Copy size={14} /></button>
          </div>
        </div>
      )}
    </section>
  );
}
