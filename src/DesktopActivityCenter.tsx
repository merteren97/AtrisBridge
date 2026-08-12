import { AlertTriangle, CheckCircle2 } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ACTIVITY_ALERTS_EVENT, activityAlertsEnabled } from "./activity-preferences";
import {
  getContinuousSyncStatus,
  listWorkspaces,
} from "./lib/bridge";
import type { ContinuousSyncStatus } from "./types";
import "./desktop-activity.css";

type Toast = {
  kind: "success" | "warning";
  title: string;
  message: string;
};

function latestMessage(status: ContinuousSyncStatus) {
  if (status.lastMessage) return status.lastMessage;
  if (status.state === "disabled") return "Continuous sync is paused for this workspace.";
  if (status.state === "debouncing") return "Waiting briefly for local changes to settle.";
  if (status.state === "running") return "A continuous synchronization cycle is active.";
  return "Continuous sync is ready for new local or remote changes.";
}

export default function DesktopActivityCenter() {
  const [toast, setToast] = useState<Toast | null>(null);
  const previousStatuses = useRef<Map<string, ContinuousSyncStatus>>(new Map());
  const notificationKeys = useRef<Set<string>>(new Set());
  const initialized = useRef(false);
  const alertsEnabledRef = useRef(activityAlertsEnabled());

  useEffect(() => {
    const syncPreference = () => {
      alertsEnabledRef.current = activityAlertsEnabled();
    };
    window.addEventListener(ACTIVITY_ALERTS_EVENT, syncPreference);
    return () => window.removeEventListener(ACTIVITY_ALERTS_EVENT, syncPreference);
  }, []);

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
        const workspaces = await listWorkspaces();
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
      } catch {
        // The Activity page surfaces live polling errors. Background monitoring stays silent.
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
    if (!toast) return undefined;
    const timer = window.setTimeout(() => setToast(null), 5200);
    return () => window.clearTimeout(timer);
  }, [toast]);

  if (!toast) return null;
  return (
    <div className={`activity-toast ${toast.kind}`} role="status" aria-live="polite">
      {toast.kind === "warning" ? <AlertTriangle size={16} /> : <CheckCircle2 size={16} />}
      <div><strong>{toast.title}</strong><small>{toast.message}</small></div>
      <button type="button" onClick={() => setToast(null)} aria-label="Dismiss notification">×</button>
    </div>
  );
}
