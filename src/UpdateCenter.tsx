import { Channel, invoke } from "@tauri-apps/api/core";
import { createContext, ReactNode, useCallback, useContext, useEffect, useRef, useState } from "react";
import { Download, LoaderCircle, RefreshCw, Rocket, X } from "lucide-react";
import { getContinuousSyncStatus, listWorkspaces } from "./lib/bridge";
import "./update.css";

export type UpdateBehavior = "notify" | "automatic";
export type UpdateStatus = "idle" | "checking" | "available" | "up-to-date" | "downloading" | "installing" | "error";

export interface UpdateRuntimeInfo {
  configured: boolean;
  currentVersion: string;
  channel: string;
}

export interface UpdateMetadata {
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

interface UpdateContextValue {
  runtime: UpdateRuntimeInfo | null;
  update: UpdateMetadata | null;
  status: UpdateStatus;
  behavior: UpdateBehavior;
  downloaded: number;
  contentLength: number | null;
  error: string | null;
  deferredAutomatic: boolean;
  setBehavior: (behavior: UpdateBehavior) => void;
  checkForUpdates: (manual?: boolean) => Promise<UpdateMetadata | null>;
  installAvailableUpdate: (automatic?: boolean) => Promise<void>;
}

const UPDATE_BEHAVIOR_KEY = "atrisbridge.updateBehavior";
const UpdateContext = createContext<UpdateContextValue | null>(null);

function initialBehavior(): UpdateBehavior {
  return localStorage.getItem(UPDATE_BEHAVIOR_KEY) === "automatic" ? "automatic" : "notify";
}

function errorMessage(error: unknown) {
  if (error instanceof Error && error.message) return error.message;
  return String(error);
}

async function hasActiveSync() {
  const workspaces = await listWorkspaces();
  const statuses = await Promise.all(workspaces.map((workspace) => getContinuousSyncStatus(workspace.id)));
  return statuses.some((status) => status.state === "running" || status.state === "debouncing");
}

export function UpdateProvider({ children }: { children: ReactNode }) {
  const [runtime, setRuntime] = useState<UpdateRuntimeInfo | null>(null);
  const [update, setUpdate] = useState<UpdateMetadata | null>(null);
  const [status, setStatus] = useState<UpdateStatus>("idle");
  const [behavior, setBehaviorState] = useState<UpdateBehavior>(initialBehavior);
  const [downloaded, setDownloaded] = useState(0);
  const [contentLength, setContentLength] = useState<number | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [dismissedVersion, setDismissedVersion] = useState<string | null>(null);
  const [deferredAutomatic, setDeferredAutomatic] = useState(false);
  const installing = useRef(false);
  const behaviorRef = useRef(behavior);
  const checkRef = useRef<(manual?: boolean) => Promise<UpdateMetadata | null>>(async () => null);

  useEffect(() => {
    behaviorRef.current = behavior;
  }, [behavior]);

  const setBehavior = useCallback((next: UpdateBehavior) => {
    localStorage.setItem(UPDATE_BEHAVIOR_KEY, next);
    behaviorRef.current = next;
    setBehaviorState(next);
    setDeferredAutomatic(next === "automatic" && Boolean(update));
  }, [update]);

  const installAvailableUpdate = useCallback(async (automatic = false) => {
    if (installing.current || !update) return;
    if (automatic) {
      try {
        if (await hasActiveSync()) {
          setDeferredAutomatic(true);
          return;
        }
      } catch {
        // If live activity cannot be inspected, fail safe and defer the automatic restart.
        setDeferredAutomatic(true);
        return;
      }
    }

    installing.current = true;
    setDeferredAutomatic(false);
    setStatus("downloading");
    setDownloaded(0);
    setContentLength(null);
    setError(null);
    const channel = new Channel<DownloadEvent>();
    channel.onmessage = (event) => {
      if (event.event === "started") {
        setStatus("downloading");
        setContentLength(event.contentLength ?? null);
      } else if (event.event === "progress") {
        setDownloaded((value) => value + event.chunkLength);
      } else if (event.event === "finished") {
        setStatus("installing");
      }
    };

    try {
      await invoke<void>("install_update", { onEvent: channel });
    } catch (reason) {
      installing.current = false;
      setStatus("error");
      setError(errorMessage(reason));
    }
  }, [update]);

  const checkForUpdates = useCallback(async (manual = false) => {
    if (installing.current || status === "checking" || status === "downloading" || status === "installing") return update;
    let currentRuntime = runtime;
    try {
      if (!currentRuntime) {
        currentRuntime = await invoke<UpdateRuntimeInfo>("get_update_runtime_info");
        setRuntime(currentRuntime);
      }
      if (!currentRuntime.configured) {
        if (manual) setError("Updates are available only in signed AtrisBridge release builds.");
        return null;
      }

      setStatus("checking");
      setError(null);
      const next = await invoke<UpdateMetadata | null>("check_for_updates");
      setUpdate(next);
      setDismissedVersion((current) => current === next?.version ? current : null);
      if (!next) {
        setStatus("up-to-date");
        setDeferredAutomatic(false);
        return null;
      }

      setStatus("available");
      if (behaviorRef.current === "automatic") setDeferredAutomatic(true);
      return next;
    } catch (reason) {
      setStatus("error");
      setError(errorMessage(reason));
      return null;
    }
  }, [runtime, status, update]);

  useEffect(() => {
    checkRef.current = checkForUpdates;
  }, [checkForUpdates]);

  useEffect(() => {
    let cancelled = false;
    let timer: number | null = null;
    void invoke<UpdateRuntimeInfo>("get_update_runtime_info")
      .then((info) => {
        if (cancelled) return;
        setRuntime(info);
        if (info.configured) {
          timer = window.setTimeout(() => {
            if (!cancelled) void checkRef.current(false);
          }, 2200);
        }
      })
      .catch((reason) => {
        if (!cancelled) setError(errorMessage(reason));
      });
    return () => {
      cancelled = true;
      if (timer !== null) window.clearTimeout(timer);
    };
  }, []);

  useEffect(() => {
    if (!deferredAutomatic || behavior !== "automatic" || !update || status !== "available") return undefined;
    void installAvailableUpdate(true);
    const timer = window.setInterval(() => void installAvailableUpdate(true), 5000);
    return () => window.clearInterval(timer);
  }, [behavior, deferredAutomatic, installAvailableUpdate, status, update]);

  const progress = contentLength && contentLength > 0
    ? Math.min(100, Math.round((downloaded / contentLength) * 100))
    : null;
  const visible = Boolean(
    update && (
      status === "downloading" || status === "installing" || status === "error" ||
      (status === "available" && behavior === "notify" && dismissedVersion !== update.version)
    ),
  );

  return (
    <UpdateContext.Provider value={{
      runtime,
      update,
      status,
      behavior,
      downloaded,
      contentLength,
      error,
      deferredAutomatic,
      setBehavior,
      checkForUpdates,
      installAvailableUpdate,
    }}>
      {children}
      {visible && update && (
        <aside className="update-card ab-update-notification" role="status" aria-live="polite">
          <div className="update-card-heading">
            <span className="update-card-icon">{status === "downloading" || status === "installing" ? <LoaderCircle className="spin" size={16} /> : <Rocket size={16} />}</span>
            <div><strong>AtrisBridge {update.version}</strong><small>{status === "available" ? "Update available" : status === "downloading" ? "Downloading signed update" : status === "installing" ? "Installing update" : "Update needs attention"}</small></div>
            {status === "available" && <button className="update-close" onClick={() => setDismissedVersion(update.version)} aria-label="Dismiss update"><X size={15} /></button>}
          </div>
          {status === "available" && update.notes && <p>{update.notes}</p>}
          {status === "downloading" && (
            <div className="update-progress"><div><span style={{ width: `${progress ?? 22}%` }} /></div><small>{progress === null ? "Downloading…" : `Downloading ${progress}%`}</small></div>
          )}
          {status === "installing" && <small className="update-message">Download verified. AtrisBridge will restart when installation completes.</small>}
          {status === "error" && error && <small className="update-message error">{error}</small>}
          {status === "available" && (
            <button className="update-primary" onClick={() => void installAvailableUpdate(false)}><Download size={14} /> Download &amp; install</button>
          )}
          {status === "error" && (
            <button className="update-primary secondary" onClick={() => void checkForUpdates(true)}><RefreshCw size={14} /> Retry from updater</button>
          )}
        </aside>
      )}
    </UpdateContext.Provider>
  );
}

export function useUpdater() {
  const value = useContext(UpdateContext);
  if (!value) throw new Error("useUpdater must be used inside UpdateProvider");
  return value;
}
