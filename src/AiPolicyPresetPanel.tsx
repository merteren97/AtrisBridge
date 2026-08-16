import { useEffect, useMemo, useState } from "react";
import { Bot, Check, ChevronDown, Code2, Eye, RefreshCw, ShieldCheck, Sparkles } from "lucide-react";
import type { AiPermissionRule, LocalMcpClientStatus, RemoteMcpClientRecord } from "./ai-gateway-types";
import { listLocalMcpClients, listRemoteMcpClients, setAiPermission } from "./lib/ai-gateway";
import type { Workspace } from "./types";
import "./ai-policy-presets.css";

interface AiPolicyPresetPanelProps {
  workspaces: Workspace[];
  onError: (message: string) => void;
}

interface PolicyClient {
  principal: string;
  label: string;
  transport: "local" | "remote";
}

type PolicyPreset = "read-only" | "developer" | "full";

const CAPABILITIES = [
  "workspace.read",
  "workspace.edit",
  "workspace.delete",
  "command.execute",
  "git.local",
  "git.remote",
  "sync.read",
  "sync.execute",
  "sync.destructive",
  "sensitive.read",
  "sensitive.write",
] as const;

const PRESETS: Array<{
  id: PolicyPreset;
  title: string;
  description: string;
  icon: typeof Eye;
}> = [
  {
    id: "read-only",
    title: "Read Only",
    description: "Read workspace files and inspect synchronization state. All mutation capabilities are denied.",
    icon: Eye,
  },
  {
    id: "developer",
    title: "Developer",
    description: "Read/edit files, run approved project tasks and use local Git. Risky/destructive capabilities remain Ask.",
    icon: Code2,
  },
  {
    id: "full",
    title: "Full Access",
    description: "Allow every AtrisBridge capability, including delete, push, sensitive files and destructive sync.",
    icon: ShieldCheck,
  },
];

function presetRule(preset: PolicyPreset, capability: string): AiPermissionRule {
  if (preset === "full") return "allow";
  if (preset === "read-only") {
    return capability === "workspace.read" || capability === "sync.read" ? "allow" : "deny";
  }
  if (["workspace.read", "workspace.edit", "command.execute", "git.local", "sync.read"].includes(capability)) {
    return "allow";
  }
  return "ask";
}

export default function AiPolicyPresetPanel({ workspaces, onError }: AiPolicyPresetPanelProps) {
  const [localClients, setLocalClients] = useState<LocalMcpClientStatus[]>([]);
  const [remoteClients, setRemoteClients] = useState<RemoteMcpClientRecord[]>([]);
  const [principal, setPrincipal] = useState("");
  const [workspaceIds, setWorkspaceIds] = useState<string[]>(workspaces[0] ? [workspaces[0].id] : []);
  const [busy, setBusy] = useState<PolicyPreset | "refresh" | null>(null);
  const [lastApplied, setLastApplied] = useState<PolicyPreset | null>(null);

  const clients = useMemo<PolicyClient[]>(() => [
    ...remoteClients.map((client) => ({ principal: client.principal, label: client.displayName, transport: "remote" as const })),
    ...localClients.map((client) => ({ principal: client.principal, label: client.label, transport: "local" as const })),
  ], [localClients, remoteClients]);

  const selectedClient = clients.find((client) => client.principal === principal) ?? null;
  const selectedWorkspaces = workspaces.filter((workspace) => workspaceIds.includes(workspace.id));

  useEffect(() => {
    void refreshClients();
  }, []);

  useEffect(() => {
    const validIds = workspaceIds.filter((id) => workspaces.some((workspace) => workspace.id === id));
    if (validIds.length !== workspaceIds.length) setWorkspaceIds(validIds);
    if (validIds.length === 0 && workspaces[0]) setWorkspaceIds([workspaces[0].id]);
  }, [workspaces]);

  useEffect(() => {
    if (principal && clients.some((client) => client.principal === principal)) return;
    setPrincipal(clients[0]?.principal ?? "");
  }, [clients, principal]);

  async function refreshClients() {
    try {
      setBusy("refresh");
      const [local, remote] = await Promise.all([listLocalMcpClients(), listRemoteMcpClients()]);
      setLocalClients(local);
      setRemoteClients(remote);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  function toggleWorkspace(id: string) {
    setWorkspaceIds((current) => current.includes(id)
      ? current.filter((value) => value !== id)
      : [...current, id]);
  }

  function selectAllWorkspaces() {
    setWorkspaceIds(workspaces.map((workspace) => workspace.id));
  }

  async function applyPreset(preset: PolicyPreset) {
    if (!selectedClient || selectedWorkspaces.length === 0) return;
    if (preset === "full") {
      const names = selectedWorkspaces.map((workspace) => workspace.name).join(", ");
      const confirmed = window.confirm(
        `Grant Full Access to ${selectedClient.label} for ${selectedWorkspaces.length} workspace(s)?\n\n${names}\n\nThis allows file deletion, sensitive-file read/write, command execution, Git push and destructive synchronization in every selected workspace.`,
      );
      if (!confirmed) return;
    }

    try {
      setBusy(preset);
      for (const workspace of selectedWorkspaces) {
        for (const capability of CAPABILITIES) {
          await setAiPermission(
            workspace.id,
            selectedClient.principal,
            capability,
            presetRule(preset, capability),
          );
        }
      }
      setLastApplied(preset);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section className="ai-bulk-policy" aria-labelledby="ai-bulk-policy-title">
      <header className="ai-bulk-policy-heading">
        <div>
          <span className="section-kicker">Permission UX v2</span>
          <h3 id="ai-bulk-policy-title">Policy presets &amp; workspace scope</h3>
          <p>Apply a predictable capability baseline to one or several workspaces without granting permissions globally.</p>
        </div>
        <button className="button secondary" type="button" onClick={() => void refreshClients()} disabled={busy !== null}>
          <RefreshCw size={13} className={busy === "refresh" ? "spin" : ""} /> Refresh clients
        </button>
      </header>

      <div className="ai-bulk-policy-config">
        <label className="ai-bulk-client-select">
          <span>Client</span>
          <div className="select-wrap">
            <select value={principal} onChange={(event) => setPrincipal(event.target.value)} disabled={clients.length === 0 || busy !== null}>
              {clients.length === 0 && <option value="">No client available</option>}
              {remoteClients.length > 0 && (
                <optgroup label="Remote">
                  {remoteClients.map((client) => <option key={client.principal} value={client.principal}>{client.displayName}</option>)}
                </optgroup>
              )}
              {localClients.length > 0 && (
                <optgroup label="Local">
                  {localClients.map((client) => <option key={client.principal} value={client.principal}>{client.label}</option>)}
                </optgroup>
              )}
            </select>
            <ChevronDown size={13} />
          </div>
        </label>

        <div className="ai-bulk-workspace-select">
          <div className="ai-bulk-workspace-title">
            <span>Workspaces</span>
            <button type="button" onClick={selectAllWorkspaces} disabled={workspaces.length === 0 || busy !== null}>Select all</button>
          </div>
          <div className="ai-bulk-workspace-grid">
            {workspaces.length === 0 ? (
              <span className="ai-bulk-empty">No workspace available.</span>
            ) : workspaces.map((workspace) => {
              const selected = workspaceIds.includes(workspace.id);
              return (
                <label className={selected ? "selected" : ""} key={workspace.id}>
                  <input type="checkbox" checked={selected} onChange={() => toggleWorkspace(workspace.id)} disabled={busy !== null} />
                  <span className="ai-bulk-check">{selected && <Check size={11} />}</span>
                  <span>{workspace.name}</span>
                </label>
              );
            })}
          </div>
        </div>
      </div>

      <div className="ai-preset-grid">
        {PRESETS.map((preset) => {
          const Icon = preset.icon;
          const loading = busy === preset.id;
          return (
            <button
              type="button"
              className={lastApplied === preset.id ? `ai-preset-card ${preset.id} applied` : `ai-preset-card ${preset.id}`}
              key={preset.id}
              onClick={() => void applyPreset(preset.id)}
              disabled={!selectedClient || selectedWorkspaces.length === 0 || busy !== null}
            >
              <span className="ai-preset-icon"><Icon size={18} /></span>
              <span className="ai-preset-copy"><strong>{preset.title}</strong><small>{preset.description}</small></span>
              <span className="ai-preset-action">{loading ? <RefreshCw className="spin" size={13} /> : lastApplied === preset.id ? <Check size={13} /> : <Sparkles size={13} />}</span>
            </button>
          );
        })}
      </div>

      <div className="ai-bulk-policy-note">
        <Bot size={14} />
        <p><strong>{selectedClient?.label ?? "Select a client"}</strong>{selectedWorkspaces.length > 0 ? ` · ${selectedWorkspaces.length} workspace(s) selected.` : " · Select at least one workspace."} Individual Deny / Ask / Allow controls above remain the source of truth and can override a preset afterward.</p>
      </div>
    </section>
  );
}
