import { useEffect, useState } from "react";
import {
  Bot,
  CheckCircle2,
  CircleOff,
  Cloud,
  Code2,
  Link2,
  RefreshCw,
  ShieldCheck,
  Trash2,
  TriangleAlert,
} from "lucide-react";
import type {
  LocalMcpClientKind,
  LocalMcpClientStatus,
  RemoteMcpClientRecord,
  RemoteMcpRelayStatus,
} from "./ai-gateway-types";
import {
  getRemoteMcpRelayStatus,
  listLocalMcpClients,
  listRemoteMcpClients,
  registerLocalMcpClient,
  retryRemoteMcpRelay,
  revokeRemoteMcpClient,
  routeRemoteMcpClientHere,
  unregisterLocalMcpClient,
} from "./lib/ai-gateway";
import type { Workspace } from "./types";
import "./ai-gateway.css";

interface AiClientConnectionsPanelProps {
  workspaces?: Workspace[];
  onError: (message: string) => void;
}

const CONNECTOR_URL_FALLBACK = "https://atrishub.com/api/mcp/v1/mcp";
const CLIENT_ORDER: LocalMcpClientKind[] = ["codex", "claude"];

function statusLabel(status: LocalMcpClientStatus): string {
  switch (status.registrationState) {
    case "registered": return "Connected";
    case "update_available": return "Repair available";
    case "not_registered": return "Not connected";
    case "not_installed": return "Client not found";
    case "conflict": return "Name conflict";
    case "companion_unavailable": return "Companion unavailable";
    default: return "Needs attention";
  }
}

function statusTone(status: LocalMcpClientStatus): string {
  if (status.registrationHealthy) return "success";
  if (status.registrationState === "conflict" || status.registrationState === "error") return "danger";
  if (status.registrationState === "update_available") return "warning";
  return "neutral";
}

function relayLabel(status: RemoteMcpRelayStatus | null): string {
  if (!status?.started) return "Unavailable";
  if (status.state === "online") return "Ready";
  if (status.state === "connecting") return "Connecting";
  if (status.state === "reconnecting") return "Reconnecting";
  return "Signed out";
}

function relayTone(status: RemoteMcpRelayStatus | null): string {
  if (status?.state === "online") return "success";
  if (status?.state === "connecting" || status?.state === "reconnecting") return "warning";
  return "neutral";
}

function formatDate(value: string | null | undefined): string {
  if (!value) return "Not yet";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

function remoteClientDetail(client: RemoteMcpClientRecord): string {
  const authorized = client.authorizedAt ? `Authorized ${formatDate(client.authorizedAt)}` : "Authorized in AtrisHub";
  if (client.observed && client.lastSeenAt) return `${authorized} · last tool request ${formatDate(client.lastSeenAt)}`;
  return `${authorized} · no tool request observed in this Desktop process`;
}

function remoteRoutingLabel(client: RemoteMcpClientRecord): string {
  if (client.activeOnThisDevice === true && client.relayReadyOnThisDevice === true) return "Routed here · Ready";
  if (client.activeOnThisDevice === true) return "Routed here · Relay not ready";
  if (client.activeOnThisDevice === false) return "Routed elsewhere";
  return "Routing unknown";
}

function remoteRoutingTone(client: RemoteMcpClientRecord): string {
  if (client.activeOnThisDevice === true && client.relayReadyOnThisDevice === true) return "success";
  if (client.activeOnThisDevice === true || client.activeOnThisDevice === false) return "warning";
  return "neutral";
}

export default function AiClientConnectionsPanel({ onError }: AiClientConnectionsPanelProps) {
  const [clients, setClients] = useState<LocalMcpClientStatus[]>([]);
  const [remoteClients, setRemoteClients] = useState<RemoteMcpClientRecord[]>([]);
  const [relayStatus, setRelayStatus] = useState<RemoteMcpRelayStatus | null>(null);
  const [busy, setBusy] = useState<LocalMcpClientKind | "refresh" | "relay" | null>(null);
  const [remoteBusy, setRemoteBusy] = useState<string | null>(null);

  useEffect(() => {
    void refreshClients();
    const timer = window.setInterval(() => void refreshRemoteSurface(false), 10_000);
    return () => window.clearInterval(timer);
  }, []);

  async function refreshRemoteSurface(reportErrors = true) {
    try {
      const [nextRemoteClients, nextRelayStatus] = await Promise.all([
        listRemoteMcpClients(),
        getRemoteMcpRelayStatus(),
      ]);
      setRemoteClients(nextRemoteClients);
      setRelayStatus(nextRelayStatus);
    } catch (error) {
      if (reportErrors) onError(String(error));
    }
  }

  async function refreshClients() {
    try {
      setBusy("refresh");
      const [nextClients, nextRemoteClients, nextRelayStatus] = await Promise.all([
        listLocalMcpClients(),
        listRemoteMcpClients(),
        getRemoteMcpRelayStatus(),
      ]);
      setClients(nextClients);
      setRemoteClients(nextRemoteClients);
      setRelayStatus(nextRelayStatus);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleRelayRetry() {
    try {
      setBusy("relay");
      setRelayStatus(await retryRemoteMcpRelay());
      window.setTimeout(() => void refreshRemoteSurface(false), 900);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function handleCopyEndpoint() {
    try {
      await navigator.clipboard.writeText(relayStatus?.connectorUrl ?? CONNECTOR_URL_FALLBACK);
    } catch (error) {
      onError(`Could not copy MCP connector endpoint: ${String(error)}`);
    }
  }

  async function handleRegister(kind: LocalMcpClientKind) {
    try {
      setBusy(kind);
      const status = await registerLocalMcpClient(kind);
      setClients((current) => current.map((item) => item.kind === kind ? status : item));
    } catch (error) {
      onError(String(error));
      await refreshClients();
    } finally {
      setBusy(null);
    }
  }

  async function handleRemove(kind: LocalMcpClientKind) {
    const status = clients.find((item) => item.kind === kind);
    if (!status || !window.confirm(`Remove the AtrisBridge MCP registration from ${status.label}? AtrisBridge workspace permissions and project files will not be deleted.`)) return;
    try {
      setBusy(kind);
      const next = await unregisterLocalMcpClient(kind);
      setClients((current) => current.map((item) => item.kind === kind ? next : item));
    } catch (error) {
      onError(String(error));
      await refreshClients();
    } finally {
      setBusy(null);
    }
  }

  async function handleRemoteRouteHere(client: RemoteMcpClientRecord) {
    try {
      setRemoteBusy(`route:${client.principal}`);
      const activated = await routeRemoteMcpClientHere(client.principal);
      if (!activated) throw new Error("AtrisHub could not find this remote authorization. Refresh the client list and try again.");
      await refreshRemoteSurface(true);
    } catch (error) {
      onError(String(error));
      await refreshRemoteSurface(false);
    } finally {
      setRemoteBusy(null);
    }
  }

  async function handleRemoteRevoke(client: RemoteMcpClientRecord) {
    const confirmed = window.confirm(
      `Revoke the AtrisHub OAuth authorization for ${client.displayName}?\n\n${client.principal}\n\nThis disconnects that exact remote client authorization and invalidates its grant-bound tokens. Local AtrisBridge workspace permission rules remain saved but inactive unless this principal is authorized again.`,
    );
    if (!confirmed) return;
    try {
      setRemoteBusy(`revoke:${client.principal}`);
      await revokeRemoteMcpClient(client.principal);
      await refreshRemoteSurface(true);
    } catch (error) {
      onError(String(error));
      await refreshRemoteSurface(false);
    } finally {
      setRemoteBusy(null);
    }
  }

  const routedHereWithoutPresence = remoteClients.some(
    (client) => client.activeOnThisDevice === true && client.relayReadyOnThisDevice !== true,
  );

  return (
    <section id="ai-clients" className="settings-section ai-gateway-section">
      <div className="settings-heading ai-gateway-heading">
        <div>
          <span className="section-kicker">AI workspace gateway</span>
          <h2>ChatGPT and local AI clients</h2>
          <p>Connections establish identity and transport only. Workspace permissions are configured once in the unified policy section below.</p>
        </div>
        <button className="button secondary" type="button" onClick={() => void refreshClients()} disabled={busy !== null || remoteBusy !== null}>
          <RefreshCw size={14} className={busy === "refresh" ? "spin" : ""} /> Refresh
        </button>
      </div>

      <div className="ai-remote-section">
        <div className="ai-remote-heading">
          <div>
            <span className="section-kicker">ChatGPT / remote MCP</span>
            <h3>ChatGPT through AtrisHub</h3>
            <p>Each row is an OAuth authorization, not an open ChatGPT conversation. AtrisBridge keeps separate principals so permissions never bleed between independently registered clients.</p>
          </div>
          <div>
            <span className={`ai-status-pill ${relayTone(relayStatus)}`}>
              {relayStatus?.state === "online" ? <CheckCircle2 size={11} /> : <Cloud size={11} />}
              {relayLabel(relayStatus)}
            </span>
            <button className="button secondary" type="button" onClick={() => void handleRelayRetry()} disabled={busy !== null || remoteBusy !== null}>
              <RefreshCw size={13} className={busy === "relay" ? "spin" : ""} /> Retry relay
            </button>
          </div>
        </div>

        <div className="ai-client-footer">
          <code>{relayStatus?.connectorUrl ?? CONNECTOR_URL_FALLBACK}</code>
          <div>
            <span className="ai-status-pill success"><ShieldCheck size={11} /> Principal-scoped policy</span>
            <button className="button secondary" type="button" onClick={() => void handleCopyEndpoint()}>Copy endpoint</button>
          </div>
        </div>

        <div className="ai-policy-summary">
          <div><small>Last connection attempt</small><strong>{formatDate(relayStatus?.lastAttemptAt)}</strong></div>
          <div><small>Last ready</small><strong>{formatDate(relayStatus?.lastConnectedAt)}</strong></div>
          <div><small>Authorized remote clients</small><strong>{remoteClients.length}</strong></div>
        </div>

        {relayStatus?.lastError && (
          <div className="ai-ask-note">
            <TriangleAlert size={14} />
            <div><strong>Last relay error</strong><p>{relayStatus.lastError}</p></div>
          </div>
        )}

        {routedHereWithoutPresence && (
          <div className="ai-ask-note">
            <TriangleAlert size={14} />
            <div>
              <strong>Relay presence is not ready.</strong>
              <p>This authorization points to this computer, but AtrisHub has not confirmed a live relay presence for the current Desktop session. Retry the relay and wait for Ready before using ChatGPT.</p>
            </div>
          </div>
        )}

        <div className="ai-remote-list">
          {remoteClients.length === 0 ? (
            <div className="ai-remote-row">
              <span className="ai-remote-icon"><Cloud size={16} /></span>
              <div className="ai-remote-identity">
                <div><strong>ChatGPT</strong><span>Remote</span></div>
                <code>Secure principal created after OAuth</code>
                <small>Not authorized yet. The client stays visible, but permissions are not attached to a fake identity.</small>
              </div>
              <span className="ai-status-pill neutral">Not authorized</span>
            </div>
          ) : remoteClients.map((client) => {
            const routeBusy = remoteBusy === `route:${client.principal}`;
            const revokeBusy = remoteBusy === `revoke:${client.principal}`;
            return (
              <div className="ai-remote-row selected" key={client.principal}>
                <span className="ai-remote-icon"><Cloud size={16} /></span>
                <div className="ai-remote-identity">
                  <div><strong>{client.displayName}</strong><span>Remote</span></div>
                  <code>{client.principal}</code>
                  <small>{remoteClientDetail(client)}</small>
                </div>
                <div className="ai-remote-actions">
                  <span className={`ai-status-pill ${remoteRoutingTone(client)}`}>
                    {remoteRoutingLabel(client)}
                  </span>
                  {client.activeOnThisDevice === false && (
                    <button
                      className="button secondary"
                      type="button"
                      title="Route new ChatGPT sessions to this computer"
                      onClick={() => void handleRemoteRouteHere(client)}
                      disabled={remoteBusy !== null || busy !== null || relayStatus?.state !== "online"}
                    >
                      {routeBusy ? <RefreshCw className="spin" size={13} /> : <Link2 size={13} />}
                      Route here
                    </button>
                  )}
                  <button
                    className="icon-action danger"
                    type="button"
                    title="Revoke this OAuth authorization"
                    aria-label={`Revoke ${client.displayName} authorization`}
                    onClick={() => void handleRemoteRevoke(client)}
                    disabled={remoteBusy !== null || busy !== null}
                  >
                    {revokeBusy ? <RefreshCw className="spin" size={14} /> : <Trash2 size={14} />}
                  </button>
                </div>
              </div>
            );
          })}
        </div>

        {remoteClients.some((client) => client.activeOnThisDevice === false) && (
          <div className="ai-ask-note">
            <ShieldCheck size={14} />
            <div>
              <strong>Routing changes only new sessions.</strong>
              <p>Existing workspace sessions and command tasks remain pinned to the Desktop where they were created. Start a new ChatGPT workspace session after routing this authorization here.</p>
            </div>
          </div>
        )}
      </div>

      <div className="ai-permission-header">
        <div>
          <span className="section-kicker">Local developer clients</span>
          <h3>Codex and Claude Code</h3>
          <p>Local registrations use the packaged MCP companion. Their workspace permissions are managed by the same unified policy surface as ChatGPT.</p>
        </div>
      </div>

      <div className="ai-client-grid">
        {CLIENT_ORDER.map((kind) => {
          const status = clients.find((client) => client.kind === kind);
          const loading = busy === kind;
          return (
            <article key={kind} className="ai-client-card">
              <div className="ai-client-select">
                <span className="ai-client-icon">{kind === "codex" ? <Code2 size={19} /> : <Bot size={19} />}</span>
                <span className="ai-client-copy">
                  <span className="ai-client-title-line">
                    <strong>{status?.label ?? (kind === "codex" ? "Codex" : "Claude Code")}</strong>
                    {status && <span className={`ai-status-pill ${statusTone(status)}`}>{status.registrationHealthy ? <CheckCircle2 size={11} /> : status.registrationState === "conflict" ? <CircleOff size={11} /> : <TriangleAlert size={11} />}{statusLabel(status)}</span>}
                  </span>
                  <small>{status?.version ?? (status?.executableDetected ? "Version unavailable" : "Client detection pending")}</small>
                  <p>{status?.detail ?? "Inspecting local MCP configuration…"}</p>
                </span>
              </div>
              <div className="ai-client-footer">
                <code>{status?.principal ?? `mcp.${kind}`}</code>
                <div>
                  {status?.canRemove && <button className="icon-action danger" type="button" title="Remove AtrisBridge registration" aria-label={`Remove ${status.label} registration`} onClick={() => void handleRemove(kind)} disabled={busy !== null || remoteBusy !== null}><Trash2 size={14} /></button>}
                  <button className={status?.registrationHealthy ? "button secondary" : "button primary"} type="button" onClick={() => void handleRegister(kind)} disabled={!status?.canRegister || busy !== null || remoteBusy !== null}>
                    {loading ? <RefreshCw className="spin" size={13} /> : status?.registrationHealthy ? <ShieldCheck size={13} /> : <Link2 size={13} />}
                    {status?.registrationHealthy ? "Verify" : status?.registrationState === "update_available" ? "Repair" : "Connect"}
                  </button>
                </div>
              </div>
            </article>
          );
        })}
      </div>

      <div className="ai-policy-boundary">
        <ShieldCheck size={16} />
        <div><strong>Connection and permission are separate.</strong><p>Connecting a client never grants workspace access. Choose a preset or customize exact capabilities in the policy section below.</p></div>
      </div>
    </section>
  );
}
