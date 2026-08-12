export const ACTIVITY_ALERTS_STORAGE_KEY = "atrisbridge.desktopAlerts";
export const ACTIVITY_ALERTS_EVENT = "atrisbridge:activity-alerts-changed";

export function activityAlertsEnabled(): boolean {
  return localStorage.getItem(ACTIVITY_ALERTS_STORAGE_KEY) === "1";
}

export function setActivityAlertsEnabled(enabled: boolean) {
  localStorage.setItem(ACTIVITY_ALERTS_STORAGE_KEY, enabled ? "1" : "0");
  window.dispatchEvent(new CustomEvent(ACTIVITY_ALERTS_EVENT, { detail: { enabled } }));
}
