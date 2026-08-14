import { useEffect, useMemo, useState } from "react";
import {
  Bot,
  CheckCircle2,
  ChevronDown,
  CircleOff,
  Cloud,
  Code2,
  KeyRound,
  Link2,
  RefreshCw,
  RotateCcw,
  ShieldAlert,
  ShieldCheck,
  TerminalSquare,
  Trash2,
  TriangleAlert,
} from "lucide-react";
import type {
  AiPermissionRecord,
  AiPermissionRule,
  LocalMcpClientKind,
  LocalMcpClientStatus,
  RemoteMcpClientRecord,
  RemoteMcpRelayStatus,
} from "./ai-gateway-types";
import {
  getRemoteMcpRelayStatus,
  listAiPermissions,
  listLocalMcpClients,
  listRemoteMcpClients,
  registerLocalMcpClient,
  resetAiPermission,
  retryRemoteMcpRelay,
  setAiPermission,
  unregisterLocalMcpClient,
} from "./lib/ai-gateway";
import type { Workspace } from "./types";
import "./ai-gateway.css";

interface AiGatewayPanelProps {
  workspaces: Workspace[];
  onError: (message: string) => void;
}

interface CapabilityDefinition {
  id: string;
  label: string;
  description: string;
  risk?: "destructive" | "open-world" | "sensitive";
}

interface CapabilityGroup {
  label: string;
  icon: "workspace" | "git" | "command" | "sync" | "sensitive";
  capabilities: CapabilityDefinition[];
}

interface PolicyClient {
  principal: string;
  label: string;
  transport: "local" | "remote";
}

const CONNECTOR_URL_FALLBACK = "https://atrishub.com/api/mcp/v1/mcp";

const CAPABILITY_GROUPS: CapabilityGroup[] = [
  {
    label: "Workspace",
    icon: "workspace",
    capabilities: [
      { id: "workspace.read", label: "Read files", description: "Inspect metadata, search text and read workspace files." },
      { id: "workspace.edit", label: "Edit files", description: "Prepare and apply reviewed multi-file changesets." },
      { id: "workspace.delete", label: "Delete files", description: "Allow changesets that remove workspace files.", risk: "destructive" },
    ],
  },
  {
    label: "Git",
    icon: "git",
    capabilities: [
      { id: "git.local", label: "Local Git", description: "Create worktrees, inspect diffs, stage and commit locally." },
      { id: "git.remote", label: "Remote Git", description: "Push commits to configured Git remotes.", risk: "open-world" },
    ],
  },
  {
    label: "Commands",
    icon: "command",
    capabilities: [
      { id: "command.execute", label: "Run project tasks", description: "Run AtrisBridge-approved build, test, lint and check profiles." },
    ],
  },
  {
    label: "Synchronization",
    icon: "sync",
    capabilities: [
      { id: "sync.read", label: "Inspect sync state", description: "Read remote mapping, plans and synchronization evidence." },
      { id: "sync.execute", label: "Execute safe sync", description: "Run approved synchronization operations." },
      { id: "sync.destructive", label: "Destructive sync", description: "Permit remote trash or local-delete synchronization actions.", risk: "destructive" },
    ],
  },
  {
    label: "Sensitive files",
    icon: "sensitive",
    capabilities: [
      { id: "sensitive.read", label: "Read sensitive workspace files", description: "Read files matched by the configurable sensitive-file policy.", risk: "sensitive" },
      { id: "sensitive.write", label: "Write sensitive workspace files", description: "Modify sensitive workspace files through the changeset engine.", risk: "sensitive" },
    ],
  },
];

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
  if (status.state === "online") return "Online";
  if (status.state === "connecting") return "Connecting";
  if (status.state === "reconnecting") return "Reconnecting";
  return "Signed out";
}

function relayTone(status: RemoteMcpRelayStatus | null): string {
  if (status?.state === "online") return "success";
  if (status?.state === "connecting" || status?.state === "reconnecting") return "warning";
  return "neutral";
}

function ruleDescription(rule: AiPermissionRule): string {
  if (rule === "allow") return "Allowed for this client and workspace";
  if (rule === "deny") return "Blocked for this client and workspace";
  return "Requires an explicit approval before a session can use it";
}

function groupIcon(icon: CapabilityGroup["icon"]) {
  if (icon === "git") return <Code2 size={15} />;
  if (icon === "command") return <TerminalSquare size={15} />;
  if (icon === "sync") return <Link2 size={15} />;
  if (icon === "sensitive") return <KeyRound size={15} />;
  return <Bot size={15} />;
}

function formatDate(value: string | null | undefined): string {
  if (!value) return "Not yet";
  const parsed = new Date(value);
  return Number.isNaN(parsed.getTime()) ? value : parsed.toLocaleString();
}

export default function AiGatewayPanel({ workspaces, onError }: AiGatewayPanelProps) {
  const [clients, setClients] = useState<LocalMcpClientStatus[]>([]);
  const [remoteClients, setRemoteClients] = useState<RemoteMcpClientRecord[]>([]);
  const [relayStatus, setRelayStatus] = useState<RemoteMcpRelayStatus | null>(null);
  const [selectedKind, setSelectedKind] = useState<LocalMcpClientKind>("codex");
  const [policyPrincipal, setPolicyPrincipal] = useState("");
  const [workspaceId, setWorkspaceId] = useState(workspaces[0]?.id ?? "");
  const [permissions, setPermissions] = useState<AiPermissionRecord[]>([]);
  const [clientBusy, setClientBusy] = useState<LocalMcpClientKind | "refresh" | "relay" | null>(null);
  const [permissionBusy, setPermissionBusy] = useState<string | null>(null);

  const policyClients = useMemo<PolicyClient[]>(() => [
    ...clients.map((client) => ({ principal: client.principal, label: client.label, transport: "local" as const })),
    ...remoteClients.map((client) => ({ principal: client.principal, label: client.displayName, transport: "remote" as const })),
  ], [clients, remoteClients]);
  const selectedPolicyClient = useMemo(
    () => policyClients.find((client) => client.principal === policyPrincipal) ?? null,
    [policyClients, policyPrincipal],
  );
  const selectedWorkspace = useMemo(
    () => workspaces.find((workspace) => workspace.id === workspaceId) ?? null,
    [workspaces, workspaceId],
  );
  const connectorUrl = relayStatus?.connectorUrl ?? CONNECTOR_URL_FALLBACK;

  useEffect(() => {
    if (!workspaceId && workspaces[0]) setWorkspaceId(workspaces[0].id);
    if (workspaceId && !workspaces.some((workspace) => workspace.id === workspaceId)) {
      setWorkspaceId(workspaces[0]?.id ?? "");
    }
  }, [workspaceId, workspaces]);

  useEffect(() => {
    void refreshClients();
    const timer = window.setInterval(() => void refreshRemoteSurface(false), 5_000);
    return () => window.clearInterval(timer);
  }, []);

  useEffect(() => {
    if (policyPrincipal && policyClients.some((client) => client.principal === policyPrincipal)) return;
    const preferred = clients.find((client) => client.kind === selectedKind) ?? clients[0];
    setPolicyPrincipal(preferred?.principal ?? remoteClients[0]?.principal ?? "");
  }, [clients, policyClients, policyPrincipal, remoteClients, selectedKind]);

  useEffect(() => {
    if (!workspaceId || !selectedPolicyClient) {
      setPermissions([]);
      return;
    }
    void refreshPermissions(workspaceId, selectedPolicyClient.principal);
  }, [workspaceId, selectedPolicyClient?.principal]);

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
      setClientBusy("refresh");
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
      setClientBusy(null);
    }
  }

  async function refreshPermissions(nextWorkspaceId: string, clientId: string) {
    try {
      setPermissions(await listAiPermissions(nextWorkspaceId, clientId));
    } catch (error) {
      onError(String(error));
    }
  }

  async function handleRelayRetry() {
    try {
      setClientBusy("relay");
      setRelayStatus(await retryRemoteMcpRelay());
      window.setTimeout(() => void refreshRemoteSurface(false), 900);
    } catch (error) {
      onError(String(error));
    } finally {
      setClientBusy(null);
    }
  }

  async function handleCopyEndpoint() {
    try {
      await navigator.clipboard.writeText(connectorUrl);
    } catch (error) {
      onError(`Could not copy MCP connector endpoint: ${String(error)}`);
    }
  }

  async function handleRegister(kind: LocalMcpClientKind) {
    try {
      setClientBusy(kind);
      const status = await registerLocalMcpClient(kind);
      setClients((current) => current.map((item) => item.kind === kind ? status : item));
      setSelectedKind(kind);
      setPolicyPrincipal(status.principal);
    } catch (error) {
      onError(String(error));
      await refreshClients();
    } finally {
      setClientBusy(null);
    }
  }

  async function handleRemove(kind: LocalMcpClientKind) {
    const status = clients.find((item) => item.kind === kind);
    if (!status || !window.confirm(`Remove the AtrisBridge MCP registration from ${status.label}? AtrisBridge workspace permissions and project files will not be deleted.`)) return;
    try {
      setClientBusy(kind);
      const next = await unregisterLocalMcpClient(kind);
      setClients((current) => current.map((item) => item.kind === kind ? next : item));
    } catch (error) {
      onError(String(error));
      await refreshClients();
    } finally {
      setClientBusy(null);
    }
  }

  function selectLocalPolicy(kind: LocalMcpClientKind, status: LocalMcpClientStatus | undefined) {
    setSelectedKind(kind);
    if (status) setPolicyPrincipal(status.principal);
  }

  async function handleRule(capability: string, rule: AiPermissionRule) {
    if (!workspaceId || !selectedPolicyClient) return;
    try {
      setPermissionBusy(capability);
      const next = await setAiPermission(workspaceId, selectedPolicyClient.principal, capability, rule);
      setPermissions((current) => current.map((item) => item.capability === capability ? next : item));
    } catch (error) {
      onError(String(error));
    } finally {
      setPermissionBusy(null);
    }
  }

  async function handleAllowAll() {
    if (!workspaceId || !selectedPolicyClient || !selectedWorkspace) return;
    const confirmed = window.confirm(
      `Allow all AtrisBridge capabilities for ${selectedPolicyClient.label} in ${selectedWorkspace.name}?\n\nThis includes file edits/deletes, sensitive-file read/write, project command execution, local Git, remote Git push, and destructive synchronization. The grant stays scoped to this exact client principal and workspace.`,
    );
    if (!confirmed) return;

    try {
      setPermissionBusy("allow-all");
      const next: AiPermissionRecord[] = [];
      for (const group of CAPABILITY_GROUPS) {
        for (const capability of group.capabilities) {
          next.push(await setAiPermission(workspaceId, selectedPolicyClient.principal, capability.id, "allow"));
        }
      }
      setPermissions(next);
    } catch (error) {
      onError(String(error));
      await refreshPermissions(workspaceId, selectedPolicyClient.principal);
    } finally {
      setPermissionBusy(null);
    }
  }

  async function handleResetAll() {
    if (!workspaceId || !selectedPolicyClient) return;
    try {
      setPermissionBusy("reset-all");
      const next: AiPermissionRecord[] = [];
      for (const group of CAPABILITY_GROUPS) {
        for (const capability of group.capabilities) {
          next.push(await resetAiPermission(workspaceId, selectedPolicyClient.principal, capability.id));
        }
      }
      setPermissions(next);
    } catch (error) {
      onError(String(error));
      await refreshPermissions(workspaceId, selectedPolicyClient.principal);
    } finally {
      setPermissionBusy(null);
    }
  }

  const permissionMap = new Map(permissions.map((permission) => [permission.capability, permission]));
  const localPolicyClients = policyClients.filter((client) => client.transport === "local");
  const remotePolicyClients = policyClients.filter((client) => client.transport === "remote");

  return (
    <section id="ai-clients" className="settings-section ai-gateway-section">
      <div className="settings-heading ai-gateway-heading">
        <div>
          <span className="section-kicker">AI workspace gateway</span>
          <h2>ChatGPT, remote MCP and local AI clients</h2>
          <p>AtrisBridge keeps one Rust workspace authority while local CLI clients and authenticated AtrisHub remote clients use independent per-workspace capability grants.</p>
        </div>
        <button className="button secondary" type="button" onClick={() => void refreshClients()} disabled={clientBusy !== null}>
          <RefreshCw size={14} className={clientBusy === "refresh" ? "spin" : ""} /> Refresh
        </button>
      </div>

      <div className="ai-remote-section">
        <div className="ai-remote-heading">
          <div>
            <span className="section-kicker">ChatGPT / remote MCP</span>
            <h3>AtrisHub secure relay</h3>
            <p>Use the public MCP endpoint in ChatGPT Developer Mode. AtrisHub handles OAuth and relays approved requests to this desktop; no inbound workstation port is exposed.</p>
          </div>
          <div>
            <span className={`ai-status-pill ${relayTone(relayStatus)}`}>
              {relayStatus?.state === "online" ? <CheckCircle2 size={11} /> : <Cloud size={11} />}
              {relayLabel(relayStatus)}
            </span>
            <button className="button secondary" type="button" onClick={() => void handleRelayRetry()} disabled={clientBusy !== null}>
              <RefreshCw size={13} className={clientBusy === "relay" ? "spin" : ""} /> Retry relay
            </button>
          </div>
        </div>

        <div className="ai-client-footer">
          <code>{connectorUrl}</code>
          <div>
            <span className="ai-status-pill success"><ShieldCheck size={11} /> Full capability policy</span>
            <button className="button secondary" type="button" onClick={() => void handleCopyEndpoint()}>Copy endpoint</button>
          </div>
        </div>

        <div className="ai-policy-summary">
          <div><small>Last connection attempt</small><strong>{formatDate(relayStatus?.lastAttemptAt)}</strong></div>
          <div><small>Last connected</small><strong>{formatDate(relayStatus?.lastConnectedAt)}</strong></div>
          <div><small>Reconnect attempts</small><strong>{relayStatus?.reconnectAttempts ?? 0}</strong></div>
        </div>

        {relayStatus?.lastError && (
          <div className="ai-ask-note">
            <TriangleAlert size={14} />
            <div><strong>Last relay error</strong><p>{relayStatus.lastError}</p></div>
          </div>
        )}

        {remoteClients.length === 0 ? (
          <div className="ai-remote-empty"><Cloud size={18} /><div><strong>No authenticated remote client observed yet</strong><p>ChatGPT will appear here after Scan Tools/OAuth sends its first validated MCP request to this desktop.</p></div></div>
        ) : (
          <div className="ai-remote-list">
            {remoteClients.map((client) => (
              <div className={`ai-remote-row ${policyPrincipal === client.principal ? "selected" : ""}`} key={client.principal}>
                <span className="ai-remote-icon"><Cloud size={16} /></span>
                <div className="ai-remote-identity"><div><strong>{client.displayName}</strong><span>Remote</span></div><code>{client.principal}</code><small>Last seen {formatDate(client.lastSeenAt)}</small></div>
                <button className="button secondary" type="button" onClick={() => setPolicyPrincipal(client.principal)}>Manage policy</button>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="ai-permission-header">
        <div>
          <span className="section-kicker">Local developer clients</span>
          <h3>Codex and Claude Code</h3>
          <p>These registrations use the packaged local MCP companion. They are separate from the ChatGPT/AtrisHub remote connector.</p>
        </div>
      </div>

      <div className="ai-client-grid">
        {CLIENT_ORDER.map((kind) => {
          const status = clients.find((client) => client.kind === kind);
          const loading = clientBusy === kind;
          return (
            <article key={kind} className={`ai-client-card ${selectedPolicyClient?.principal === status?.principal ? "selected" : ""}`}>
              <button className="ai-client-select" type="button" onClick={() => selectLocalPolicy(kind, status)} aria-label={`Manage ${status?.label ?? kind}`}>
                <span className="ai-client-icon">{kind === "codex" ? <Code2 size={19} /> : <Bot size={19} />}</span>
                <span className="ai-client-copy">
                  <span className="ai-client-title-line">
                    <strong>{status?.label ?? (kind === "codex" ? "Codex" : "Claude Code")}</strong>
                    {status && <span className={`ai-status-pill ${statusTone(status)}`}>{status.registrationHealthy ? <CheckCircle2 size={11} /> : status.registrationState === "conflict" ? <CircleOff size={11} /> : <TriangleAlert size={11} />}{statusLabel(status)}</span>}
                  </span>
                  <small>{status?.version ?? (status?.executableDetected ? "Version unavailable" : "Client detection pending")}</small>
                  <p>{status?.detail ?? "Inspecting local MCP configuration…"}</p>
                </span>
              </button>
              <div className="ai-client-footer">
                <code>{status?.principal ?? `mcp.${kind}`}</code>
                <div>
                  {status?.canRemove && <button className="icon-action danger" type="button" title="Remove AtrisBridge registration" aria-label={`Remove ${status.label} registration`} onClick={() => void handleRemove(kind)} disabled={clientBusy !== null}><Trash2 size={14} /></button>}
                  <button className={status?.registrationHealthy ? "button secondary" : "button primary"} type="button" onClick={() => void handleRegister(kind)} disabled={!status?.canRegister || clientBusy !== null}>
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
        <div><strong>Connection never grants workspace access by itself.</strong><p>OAuth scopes are only the remote ceiling. The selected client must still pass this exact workspace's persistent Deny / Ask / Allow capability policy.</p></div>
      </div>

      <div className="ai-permission-header">
        <div>
          <span className="section-kicker">Workspace policy</span>
          <h3>Capability grants</h3>
          <p>Choose exactly what {selectedPolicyClient?.label ?? "this client"} may do inside one AtrisBridge workspace.</p>
        </div>
        <div className="ai-policy-selectors">
          <label>
            <span>Client</span>
            <div className="select-wrap">
              <select value={policyPrincipal} onChange={(event) => setPolicyPrincipal(event.target.value)} disabled={policyClients.length === 0}>
                {policyClients.length === 0 && <option value="">No client</option>}
                {remotePolicyClients.length > 0 && <optgroup label="Remote">{remotePolicyClients.map((client) => <option value={client.principal} key={client.principal}>{client.label}</option>)}</optgroup>}
                {localPolicyClients.length > 0 && <optgroup label="Local">{localPolicyClients.map((client) => <option value={client.principal} key={client.principal}>{client.label}</option>)}</optgroup>}
              </select>
              <ChevronDown size={13} />
            </div>
          </label>
          <label>
            <span>Workspace</span>
            <div className="select-wrap"><select value={workspaceId} onChange={(event) => setWorkspaceId(event.target.value)} disabled={workspaces.length === 0}>{workspaces.length === 0 ? <option value="">No workspace</option> : workspaces.map((workspace) => <option key={workspace.id} value={workspace.id}>{workspace.name}</option>)}</select><ChevronDown size={13} /></div>
          </label>
        </div>
      </div>

      {!selectedWorkspace ? (
        <div className="ai-policy-empty"><Bot size={19} /><div><strong>Add a workspace before granting AI access</strong><p>Permissions are never global across project folders.</p></div></div>
      ) : !selectedPolicyClient ? (
        <div className="ai-policy-empty"><ShieldAlert size={19} /><div><strong>Select an AI client to manage policy</strong><p>Remote clients appear after their authenticated relay identity has been observed by this desktop session.</p></div></div>
      ) : (
        <div className="ai-permission-surface">
          <div className="ai-policy-summary">
            <div><small>Client principal</small><strong>{selectedPolicyClient.principal}</strong></div>
            <div><small>Workspace</small><strong>{selectedWorkspace.name}</strong></div>
            <div>
              <button className="button primary" type="button" onClick={() => void handleAllowAll()} disabled={permissionBusy !== null}><ShieldCheck size={13} /> Allow all</button>
              <button className="text-action" type="button" onClick={() => void handleResetAll()} disabled={permissionBusy !== null}><RotateCcw size={13} /> Reset to Ask</button>
            </div>
          </div>

          <div className="ai-capability-groups">
            {CAPABILITY_GROUPS.map((group) => (
              <div className="ai-capability-group" key={group.label}>
                <div className="ai-capability-group-title"><span>{groupIcon(group.icon)}</span><strong>{group.label}</strong></div>
                {group.capabilities.map((capability) => {
                  const permission = permissionMap.get(capability.id);
                  const rule: AiPermissionRule = permission?.rule ?? "ask";
                  const busy = permissionBusy === capability.id;
                  return (
                    <div className={`ai-capability-row ${capability.risk ? `risk-${capability.risk}` : ""}`} key={capability.id}>
                      <div className="ai-capability-copy">
                        <div><strong>{capability.label}</strong>{capability.risk === "destructive" && <span className="risk-badge destructive"><ShieldAlert size={10} /> Destructive</span>}{capability.risk === "open-world" && <span className="risk-badge open-world"><Link2 size={10} /> External write</span>}{capability.risk === "sensitive" && <span className="risk-badge sensitive"><KeyRound size={10} /> Sensitive</span>}</div>
                        <p>{capability.description}</p>
                        <small>{ruleDescription(rule)}{permission?.explicit ? " · saved override" : " · default policy"}</small>
                      </div>
                      <div className="permission-segment" aria-label={`${capability.label} permission`}>
                        {(["deny", "ask", "allow"] as AiPermissionRule[]).map((option) => (
                          <button key={option} type="button" className={rule === option ? `active ${option}` : ""} onClick={() => void handleRule(capability.id, option)} disabled={permissionBusy !== null}>{busy && rule === option ? <RefreshCw className="spin" size={11} /> : option === "deny" ? "Deny" : option === "ask" ? "Ask" : "Allow"}</button>
                        ))}
                      </div>
                    </div>
                  );
                })}
              </div>
            ))}
          </div>
        </div>
      )}

      <div className="ai-ask-note">
        <TriangleAlert size={14} />
        <div><strong>Ask remains fail-closed for non-interactive MCP sessions.</strong><p>Use Allow all when you intentionally want the selected ChatGPT/AI client to have the complete capability set for this workspace. The grant does not carry to another workspace or client principal.</p></div>
      </div>
    </section>
  );
}
