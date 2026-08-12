import { useEffect, useMemo, useState } from "react";
import {
  Bot,
  CheckCircle2,
  ChevronDown,
  CircleOff,
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
} from "./ai-gateway-types";
import {
  listAiPermissions,
  listLocalMcpClients,
  registerLocalMcpClient,
  resetAiPermission,
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

export default function AiGatewayPanel({ workspaces, onError }: AiGatewayPanelProps) {
  const [clients, setClients] = useState<LocalMcpClientStatus[]>([]);
  const [selectedKind, setSelectedKind] = useState<LocalMcpClientKind>("codex");
  const [workspaceId, setWorkspaceId] = useState(workspaces[0]?.id ?? "");
  const [permissions, setPermissions] = useState<AiPermissionRecord[]>([]);
  const [clientBusy, setClientBusy] = useState<LocalMcpClientKind | "refresh" | null>(null);
  const [permissionBusy, setPermissionBusy] = useState<string | "reset-all" | null>(null);

  const selectedClient = useMemo(
    () => clients.find((client) => client.kind === selectedKind) ?? null,
    [clients, selectedKind],
  );
  const selectedWorkspace = useMemo(
    () => workspaces.find((workspace) => workspace.id === workspaceId) ?? null,
    [workspaces, workspaceId],
  );

  useEffect(() => {
    if (!workspaceId && workspaces[0]) setWorkspaceId(workspaces[0].id);
    if (workspaceId && !workspaces.some((workspace) => workspace.id === workspaceId)) {
      setWorkspaceId(workspaces[0]?.id ?? "");
    }
  }, [workspaceId, workspaces]);

  useEffect(() => {
    void refreshClients();
  }, []);

  useEffect(() => {
    if (!workspaceId || !selectedClient) {
      setPermissions([]);
      return;
    }
    void refreshPermissions(workspaceId, selectedClient.principal);
  }, [workspaceId, selectedClient?.principal]);

  async function refreshClients() {
    try {
      setClientBusy("refresh");
      setClients(await listLocalMcpClients());
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

  async function handleRegister(kind: LocalMcpClientKind) {
    try {
      setClientBusy(kind);
      const status = await registerLocalMcpClient(kind);
      setClients((current) => current.map((item) => item.kind === kind ? status : item));
      setSelectedKind(kind);
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

  async function handleRule(capability: string, rule: AiPermissionRule) {
    if (!workspaceId || !selectedClient) return;
    try {
      setPermissionBusy(capability);
      const next = await setAiPermission(workspaceId, selectedClient.principal, capability, rule);
      setPermissions((current) => current.map((item) => item.capability === capability ? next : item));
    } catch (error) {
      onError(String(error));
    } finally {
      setPermissionBusy(null);
    }
  }

  async function handleResetAll() {
    if (!workspaceId || !selectedClient) return;
    try {
      setPermissionBusy("reset-all");
      const next: AiPermissionRecord[] = [];
      for (const group of CAPABILITY_GROUPS) {
        for (const capability of group.capabilities) {
          next.push(await resetAiPermission(workspaceId, selectedClient.principal, capability.id));
        }
      }
      setPermissions(next);
    } catch (error) {
      onError(String(error));
      await refreshPermissions(workspaceId, selectedClient.principal);
    } finally {
      setPermissionBusy(null);
    }
  }

  const permissionMap = new Map(permissions.map((permission) => [permission.capability, permission]));

  return (
    <section id="ai-clients" className="settings-section ai-gateway-section">
      <div className="settings-heading ai-gateway-heading">
        <div>
          <span className="section-kicker">AI workspace gateway</span>
          <h2>Local AI clients</h2>
          <p>Connect supported MCP hosts to the packaged AtrisBridge companion. Workspace files remain behind the desktop Rust authority and its permission model.</p>
        </div>
        <button className="button secondary" type="button" onClick={() => void refreshClients()} disabled={clientBusy !== null}>
          <RefreshCw size={14} className={clientBusy === "refresh" ? "spin" : ""} /> Refresh
        </button>
      </div>

      <div className="ai-client-grid">
        {CLIENT_ORDER.map((kind) => {
          const status = clients.find((client) => client.kind === kind);
          const loading = clientBusy === kind;
          return (
            <article key={kind} className={`ai-client-card ${selectedKind === kind ? "selected" : ""}`}>
              <button className="ai-client-select" type="button" onClick={() => setSelectedKind(kind)} aria-label={`Manage ${status?.label ?? kind}`}>
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
        <div><strong>Registration does not grant workspace access.</strong><p>Each client gets its own principal and each workspace keeps an independent capability policy. The companion cannot select a different client identity.</p></div>
      </div>

      <div className="ai-permission-header">
        <div>
          <span className="section-kicker">Workspace policy</span>
          <h3>Capability grants</h3>
          <p>Choose what {selectedClient?.label ?? "this client"} may do inside one AtrisBridge workspace.</p>
        </div>
        <div className="ai-policy-selectors">
          <label>
            <span>Client</span>
            <div className="select-wrap"><select value={selectedKind} onChange={(event) => setSelectedKind(event.target.value as LocalMcpClientKind)}>{CLIENT_ORDER.map((kind) => <option value={kind} key={kind}>{clients.find((client) => client.kind === kind)?.label ?? kind}</option>)}</select><ChevronDown size={13} /></div>
          </label>
          <label>
            <span>Workspace</span>
            <div className="select-wrap"><select value={workspaceId} onChange={(event) => setWorkspaceId(event.target.value)} disabled={workspaces.length === 0}>{workspaces.length === 0 ? <option value="">No workspace</option> : workspaces.map((workspace) => <option key={workspace.id} value={workspace.id}>{workspace.name}</option>)}</select><ChevronDown size={13} /></div>
          </label>
        </div>
      </div>

      {!selectedWorkspace ? (
        <div className="ai-policy-empty"><Bot size={19} /><div><strong>Add a workspace before granting AI access</strong><p>Permissions are never global across project folders.</p></div></div>
      ) : (
        <div className="ai-permission-surface">
          <div className="ai-policy-summary">
            <div><small>Client principal</small><strong>{selectedClient?.principal ?? `mcp.${selectedKind}`}</strong></div>
            <div><small>Workspace</small><strong>{selectedWorkspace.name}</strong></div>
            <button className="text-action" type="button" onClick={() => void handleResetAll()} disabled={!selectedClient || permissionBusy !== null}><RotateCcw size={13} /> Reset all to Ask</button>
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
                          <button key={option} type="button" className={rule === option ? `active ${option}` : ""} onClick={() => void handleRule(capability.id, option)} disabled={busy || permissionBusy === "reset-all"}>{busy && rule === option ? <RefreshCw className="spin" size={11} /> : option === "deny" ? "Deny" : option === "ask" ? "Ask" : "Allow"}</button>
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
        <div><strong>Ask remains fail-closed for non-interactive MCP sessions.</strong><p>AtrisBridge will not silently promote an Ask rule to Allow. Set a persistent workspace/client rule when you intentionally want a local MCP client to use that capability without a separate approval flow.</p></div>
      </div>
    </section>
  );
}
