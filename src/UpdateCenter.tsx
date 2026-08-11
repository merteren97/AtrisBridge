import { useCallback, useEffect, useRef, useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";
import { CheckCircle2, Download, RefreshCw, RotateCcw, X } from "lucide-react";
import "./update.css";

interface UpdateRuntimeInfo {
  configured: boolean;
  currentVersion: string;
  channel: string;
}

interface UpdateMetadata {
  version: string;
  currentVersion: string;
  notes?: string | null;
  pubDate?: string | null;
}

interface DownloadEvent {
  event: "started" | "progress" | "finished";
  contentLength?: number | null;
  chunkLength: number;
}

export default function UpdateCenter() {
  const [runtime, setRuntime] = useState<UpdateRuntimeInfo | null>(null);
  const [update, setUpdate] = useState<UpdateMetadata | null>(null);
  const [checking, setChecking] = useState(false);
  const [installing, setInstalling] = useState(false);
  const [downloaded, setDownloaded] = useState(0);
  const [contentLength, setContentLength] = useState<number | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [dismissed, setDismissed] = useState(false);
  const automaticCheckStarted = useRef(false);

  const check = useCallback(async (automatic = false) => {
    if (checking || installing) return;
    setChecking(true);
    if (!automatic) setMessage(null);
    try {
      const next = await invoke<UpdateMetadata | null>("check_for_updates");
      setUpdate(next);
      setDismissed(false);
      if (!automatic && !next) setMessage("AtrisBridge is up to date.");
    } catch (error) {
      if (!automatic) setMessage(String(error));
    } finally {
      setChecking(false);
    }
  }, [checking, installing]);

  useEffect(() => {
    let cancelled = false;
    void invoke<UpdateRuntimeInfo>("get_update_runtime_info")
      .then((info) => {
        if (cancelled) return;
        setRuntime(info);
        if (info.configured && !automaticCheckStarted.current) {
          automaticCheckStarted.current = true;
          window.setTimeout(() => void check(true), 3500);
        }
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [check]);

  async function install() {
    if (!update || installing) return;
    setInstalling(true);
    setDownloaded(0);
    setContentLength(null);
    setMessage(null);
    const channel = new Channel<DownloadEvent>();
    channel.onmessage = (event) => {
      if (event.event === "started") {
        setContentLength(event.contentLength ?? null);
      } else if (event.event === "progress") {
        setDownloaded((value) => value + event.chunkLength);
      } else if (event.event === "finished") {
        setMessage("Update downloaded. AtrisBridge is restarting…");
      }
    };
    try {
      await invoke<void>("install_update", { onEvent: channel });
    } catch (error) {
      setMessage(String(error));
      setInstalling(false);
    }
  }

  if (!runtime) return null;
  if (update && !dismissed) {
    const progress = contentLength && contentLength > 0
      ? Math.min(100, Math.round((downloaded / contentLength) * 100))
      : null;
    return (
      <aside className="update-card" role="status" aria-live="polite">
        <div className="update-card-heading">
          <span className="update-card-icon"><Download size={16} /></span>
          <div>
            <strong>AtrisBridge {update.version}</strong>
            <small>{runtime.channel === "preview" ? "Preview update" : "Update available"}</small>
          </div>
          {!installing && (
            <button className="update-close" onClick={() => setDismissed(true)} aria-label="Dismiss update">
              <X size={15} />
            </button>
          )}
        </div>
        {update.notes && <p>{update.notes}</p>}
        {installing && (
          <div className="update-progress">
            <div><span style={{ width: `${progress ?? 18}%` }} /></div>
            <small>{progress === null ? "Downloading signed update…" : `Downloading ${progress}%`}</small>
          </div>
        )}
        {message && <small className="update-message">{message}</small>}
        <button className="update-primary" onClick={install} disabled={installing}>
          {installing ? <RefreshCw className="spin" size={14} /> : <RotateCcw size={14} />}
          {installing ? "Installing…" : "Download & install"}
        </button>
      </aside>
    );
  }

  return (
    <aside className="update-runtime-pill" title={`Update channel: ${runtime.channel}`}>
      <CheckCircle2 size={13} />
      <span>v{runtime.currentVersion}</span>
      {runtime.configured && (
        <button onClick={() => void check(false)} disabled={checking || installing}>
          {checking ? <RefreshCw className="spin" size={12} /> : "Check"}
        </button>
      )}
      {message && <em>{message}</em>}
    </aside>
  );
}
