import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
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
} from "../lib/bridge";
import type {
  JournalSummary,
  ProviderConnection,
  RcloneStatus,
  RemoteInventoryReport,
  ScanReport,
  Workspace,
  WorkspaceRemoteBinding,
} from "../types";

export type ProductView = "overview" | "workspace" | "activity" | "settings";
export type WorkspaceSection = "files" | "protection";

export interface ProductNotice {
  title: string;
  message: string;
  detail: string;
}

export type ProductTone = "healthy" | "waiting" | "attention" | "neutral";

const CLOSE_TO_TRAY_KEY = "atrisbridge.closeToTray";

export function formatBytes(bytes: number): string {
  if (bytes === 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const index = Math.min(Math.floor(Math.log(bytes) / Math.log(1024)), units.length - 1);
  return `${(bytes / 1024 ** index).toFixed(index === 0 ? 0 : 1)} ${units[index]}`;
}

export function formatDate(value: string | null | undefined): string {
  if (!value) return "Not yet";
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  }).format(new Date(value));
}

export function syncModeLabel(mode: Workspace["syncMode"]): string {
  if (mode === "two_way") return "Two-Way";
  if (mode === "pull") return "Pull";
  return "Backup";
}

export function workspaceState(
  summary: JournalSummary | undefined,
  binding: WorkspaceRemoteBinding | null | undefined,
): { label: string; detail: string; tone: ProductTone } {
  const conflicts = summary?.conflicts ?? 0;
  const waiting = (summary?.pendingOperations ?? 0) + (summary?.changedFiles ?? 0);

  if (conflicts > 0) {
    return {
      label: "Needs attention",
      detail: `${conflicts} conflict${conflicts === 1 ? "" : "s"}`,
      tone: "attention",
    };
  }
  if (waiting > 0) {
    return {
      label: "Changes waiting",
      detail: `${waiting} item${waiting === 1 ? "" : "s"}`,
      tone: "waiting",
    };
  }
  if (binding) {
    return { label: "Cloud mapped", detail: "No conflicts detected", tone: "healthy" };
  }
  return { label: "Local only", detail: "Cloud is optional", tone: "neutral" };
}

function fileNameFromPath(path: string): string {
  return path.replace(/\\/g, "/").split("/").filter(Boolean).at(-1) ?? "Workspace";
}

function remoteSegment(value: string): string {
  return value.replace(/[\\/]/g, "-").trim() || "Workspace";
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
      message:
        "AtrisBridge could not finish verifying the selected Google account. Try connecting again; no workspace files were changed.",
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
    message:
      "Nothing was applied after the operation failed. Review the technical details and try again.",
    detail,
  };
}

export function useProductModel() {
  const [view, setView] = useState<ProductView>("overview");
  const [workspaceSection, setWorkspaceSection] = useState<WorkspaceSection>("files");
  const [inspectorOpen, setInspectorOpen] = useState(true);
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
    setWorkspaceSection("files");
    setInspectorOpen(true);
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
      setWorkspaceSection("files");
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
      const removedId = selected.id;
      const remaining = workspaces.filter((workspace) => workspace.id !== removedId);
      setWorkspaces(remaining);
      setSelectedId(remaining[0]?.id ?? null);
      setReports((current) => Object.fromEntries(Object.entries(current).filter(([id]) => id !== removedId)));
      setRemoteReports((current) => Object.fromEntries(Object.entries(current).filter(([id]) => id !== removedId)));
      setSummaries((current) => Object.fromEntries(Object.entries(current).filter(([id]) => id !== removedId)));
      setBindings((current) => Object.fromEntries(Object.entries(current).filter(([id]) => id !== removedId)));
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

  const overviewTone: ProductTone = totalConflicts > 0
    ? "attention"
    : totalPending + totalChanged > 0
      ? "waiting"
      : "healthy";
  const overviewTitle = totalConflicts > 0
    ? `${totalConflicts} conflict${totalConflicts === 1 ? "" : "s"} need review`
    : totalPending + totalChanged > 0
      ? `${totalPending + totalChanged} change${totalPending + totalChanged === 1 ? "" : "s"} waiting`
      : workspaces.length > 0
        ? "No conflicts need attention"
        : "Add a workspace to get started";
  const overviewDetail = totalConflicts > 0
    ? "AtrisBridge will keep conflicting paths untouched until you review them."
    : totalPending + totalChanged > 0
      ? "Your project state is safe. Review and run synchronization when you are ready."
      : workspaces.length > 0
        ? "Workspace state is clear. Cloud transport remains explicit and reviewable."
        : "AtrisBridge starts locally and connects cloud transport only when you choose to.";

  return {
    view,
    setView,
    workspaceSection,
    setWorkspaceSection,
    inspectorOpen,
    setInspectorOpen,
    workspaces,
    selected,
    reports,
    report,
    remoteReports,
    remoteReport,
    summaries,
    bindings,
    googleDrive,
    rcloneStatus,
    remotePathDraft,
    setRemotePathDraft,
    loading,
    cloudLoading,
    error,
    setError,
    closeToTray,
    setCloseToTray,
    journal,
    binding,
    backupReady,
    encryptionReady,
    notice,
    totalKnownBytes,
    totalKnownFiles,
    totalPending,
    totalConflicts,
    totalChanged,
    overviewTone,
    overviewTitle,
    overviewDetail,
    refreshCloud,
    refreshAfterBackupChange,
    openWorkspace,
    handleAddWorkspace,
    handleScan,
    handleCreateIgnore,
    handleRemove,
    handleConnectGoogleDrive,
    handleDisconnectCloudSession,
    handleForgetCloud,
    handleBindRemote,
    handleRemoteScan,
  };
}

export type ProductModel = ReturnType<typeof useProductModel>;
