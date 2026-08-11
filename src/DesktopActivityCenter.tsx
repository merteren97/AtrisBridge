import {
  Activity,
  AlertTriangle,
  Bell,
  BellOff,
  CheckCircle2,
  ChevronDown,
  ChevronUp,
  LoaderCircle,
  ShieldAlert,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  getContinuousSyncStatus,
  listJournalSummaries,
  listWorkspaces,
} from "./lib/bridge";
import type { ContinuousSyncStatus, JournalSummary, Workspace } from "./types";
import "./desktop-activity.css";

type ActivityRow = {
  workspace: Workspace;
  status: ContinuousSyncStatus;
};

type Toast = {
  kind: "success" | "warning";
  title: string;
  message: string;
};

const ALERTS_STORAGE_KEY = "atrisbridge.desktopAlerts";

function stateLabel(status: ContinuousSyncStatus) {
  switch (status.state) {
    case "debouncing":
      return "Changes queued";
    case "running":
      return "Syncing";
    case "attention":
      return "Needs attention";
    case "error":
      return "Sync error";
    case "disabled":
      return "Paused";
    default:
      return "Watching";
  }
}

function stateIcon(status: ContinuousSyncStatus) {
  if (status.state === "running" || status.state === "debouncing") {
    return <LoaderCircle size={14} className="activity-spin" />;
  }
  if (status.state === "attention") return <ShieldAlert size={14} />;
  if (status.state === "error") return <AlertTriangle size={14} />;
  return <CheckCircle2 size={14} />;
}

function latestMessage(status: ContinuousSyncStatus) {
  if (status.lastMessage) return status.lastMessage;
  if (status.state === "disabled") return "Continuous sync is paused for this workspace.";
  if (status.state === "debouncing") return "Waiting briefly for local changes to settle.";
  if (status.state === "running") return "A continuous synchronization cycle is active.";
  return "Continuous sync is ready for new local or remote changes.";
}

export default function DesktopActivityCenter() {
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<ActivityRow[]>([]);
  const [journals, setJournals] = useState<JournalSummary[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [toast, setToast] = useState<Toast | null>(null);
  const [alertsEnabled, setAlertsEnabled] = useState(
    () => localStorage.getItem(ALERTS_STORAGE_KEY) === "1",
  );
  const previousStatuses = useRef<Map<string, ContinuousSyncStatus>>(new Map());
  const notificationKeys = useRef<Set<string>>(new Set());
  const initialized = useRef(false);
  const alertsEnabledRef = useRef(alertsEnabled);

  useEffect(() => {
    alertsEnabledRef.current = alertsEnabled;
  }, [alertsEnabled]);

  useEffect(() => {
    let disposed = false;

    const deliverAlert = (title: string, message: string, kind: Toast["kind"], key: string) => {
      if (notificationKeys.current.has(key)) return;
      notificationKeys.current.add(key);
      setToast({ title, message, kind });

      if (
        alertsEnabledRef.current &&
        document.hidden &&
        "Notification" in window &&
        Notification.permission === "granted"
      ) {
        new Notification(title, { body: message, tag: key });
      }
    };

    const refresh = async () => {
      try {
        const [workspaces, journalSummaries] = await Promise.all([
          listWorkspaces(),
          listJournalSummaries(),
        ]);
        const statusRows = await Promise.all(
          workspaces.map(async (workspace) => ({
            workspace,
            status: await getContinuousSyncStatus(workspace.id),
          })),
        );

        if (disposed) return;

        if (initialized.current) {
          for (const row of statusRows) {
            const previous = previousStatuses.current.get(row.workspace.id);
            const status = row.status;
            const eventKey = status.lastEventAt ?? status.lastCycleCompletedAt ?? status.lastMessage ?? "state";

            if (
              (status.state === "attention" || status.state === "error") &&
              previous?.state !== status.state
            ) {
              deliverAlert(
                `${row.workspace.name} needs attention`,
                latestMessage(status),
                "warning",
                `${row.workspace.id}:${status.state}:${eventKey}`,
              );
            }

            if (
              previous &&
              (previous.state === "running" || previous.state === "debouncing") &&
              status.state === "idle" &&
              status.lastSuccessAt &&
              status.lastSuccessAt !== previous.lastSuccessAt
            ) {
              deliverAlert(
                `${row.workspace.name} is up to date`,
                status.lastMessage ?? "The latest synchronization cycle completed successfully.",
                "success",
                `${row.workspace.id}:success:${status.lastSuccessAt}`,
              );
            }
          }
        }

        previousStatuses.current = new Map(
          statusRows.map((row) => [row.workspace.id, row.status]),
        );
        initialized.current = true;
        setRows(statusRows);
        setJournals(journalSummaries);
        setLoadError(null);
      } catch (error) {
        if (!disposed) {
          setLoadError(error instanceof Error ? error.message : String(error));
        }
      }
    };

    void refresh();
    const timer = window.setInterval(() => void refresh(), 3500);
    return () => {
      disposed = true;
      window.clearInterval(timer);
    };
  }, []);

  useEffect(() => {
    if (!toast) return;
    const timer = window.setTimeout(() => setToast(null), 5200);
    return () => window.clearTimeout(timer);
  }, [toast]);

  const summary = useMemo(() => {
    const active = rows.filter((row) =>
      row.status.state === "running" || row.status.state === "debouncing",
    ).length;
    const runtimeActive = rows.filter((row) => row.status.runtimeActive).length;
    const stateIssues = rows.filter((row) =>
      row.status.state === "attention" || row.status.state === "error",
    ).length;
    const queued = journals.reduce((total, journal) => total + journal.pendingOperations, 0);
    const conflicts = journals.reduce((total, journal) => total + journal.conflicts, 0);
    return { active, runtimeActive, issues: stateIssues + conflicts, queued, conflicts };
  }, [rows, journals]);

  const toggleAlerts = async () => {
    if (alertsEnabled) {
      localStorage.setItem(ALERTS_STORAGE_KEY, "0");
      setAlertsEnabled(false);
      return;
    }

    if ("Notification" in window && Notification.permission === "default") {
      const permission = await Notification.requestPermission();
      if (permission === "denied") {
        setToast({
          kind: "warning",
          title: "Desktop alerts are blocked",
          message: "AtrisBridge will continue to show in-app activity alerts.",
        });
        return;
      }
    }

    localStorage.setItem(ALERTS_STORAGE_KEY, "1");
    setAlertsEnabled(true);
    setToast({
      kind: "success",
      title: "Desktop alerts enabled",
      message: "AtrisBridge will surface completed cycles and items that need attention.",
    });
  };

  return (
    <>
      <section className={`desktop-activity ${open ? "open" : ""}`} aria-label="AtrisBridge activity">
        {open && (
          <div className="activity-panel">
            <div className="activity-panel-heading">
              <div className="activity-heading-copy">
                <span className="activity-icon"><Activity size={15} /></span>
                <div>
                  <strong>Sync activity</strong>
                  <small>{summary.runtimeActive} watcher{summary.runtimeActive === 1 ? "" : "s"} active</small>
                </div>
              </div>
              <button
                className={`activity-alert-toggle ${alertsEnabled ? "enabled" : ""}`}
                onClick={() => void toggleAlerts()}
                title={alertsEnabled ? "Disable desktop alerts" : "Enable desktop alerts"}
                type="button"
              >
                {alertsEnabled ? <Bell size={13} /> : <BellOff size={13} />}
                {alertsEnabled ? "Alerts on" : "Alerts off"}
              </button>
            </div>

            <div className="activity-metrics">
              <div><small>Active</small><strong>{summary.active}</strong></div>
              <div><small>Queued</small><strong>{summary.queued}</strong></div>
              <div className={summary.issues > 0 ? "attention" : ""}>
                <small>Attention</small><strong>{summary.issues}</strong>
              </div>
            </div>

            {loadError && <div className="activity-load-error">{loadError}</div>}

            <div className="activity-list">
              {rows.length === 0 && !loadError ? (
                <div className="activity-empty">Add a workspace to start monitoring synchronization activity.</div>
              ) : (
                rows.map(({ workspace, status }) => (
                  <div className={`activity-row state-${status.state}`} key={workspace.id}>
                    <div className="activity-row-icon">{stateIcon(status)}</div>
                    <div className="activity-row-copy">
                      <div className="activity-row-title">
                        <strong>{workspace.name}</strong>
                        <span>{stateLabel(status)}</span>
                      </div>
                      <small>{latestMessage(status)}</small>
                      {(status.state === "running" || status.state === "debouncing") && (
                        <div className="activity-progress" aria-label="Synchronization in progress">
                          <span />
                        </div>
                      )}
                    </div>
                  </div>
                ))
              )}
            </div>

            {summary.conflicts > 0 && (
              <div className="activity-conflict-note">
                <AlertTriangle size={13} />
                {summary.conflicts} unresolved conflict{summary.conflicts === 1 ? "" : "s"} require review.
              </div>
            )}
          </div>
        )}

        <button
          type="button"
          className={`activity-trigger ${summary.issues > 0 ? "attention" : summary.active > 0 ? "busy" : ""}`}
          onClick={() => setOpen((value) => !value)}
          aria-expanded={open}
        >
          <Activity size={14} />
          <span>
            {summary.issues > 0
              ? `${summary.issues} need attention`
              : summary.active > 0
                ? `${summary.active} sync active`
                : summary.queued > 0
                  ? `${summary.queued} queued`
                  : "Sync activity"}
          </span>
          {open ? <ChevronDown size={13} /> : <ChevronUp size={13} />}
        </button>
      </section>

      {toast && (
        <div className={`activity-toast ${toast.kind}`} role="status">
          {toast.kind === "warning" ? <AlertTriangle size={15} /> : <CheckCircle2 size={15} />}
          <div><strong>{toast.title}</strong><small>{toast.message}</small></div>
          <button type="button" onClick={() => setToast(null)} aria-label="Dismiss notification">×</button>
        </div>
      )}
    </>
  );
}
