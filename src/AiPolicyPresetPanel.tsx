import { useEffect, useMemo, useRef, useState } from "react";
import {
  Bot,
  Check,
  ChevronDown,
  ChevronUp,
  Code2,
  Eye,
  KeyRound,
  Link2,
  RefreshCw,
  ShieldAlert,
  ShieldCheck,
  SlidersHorizontal,
  Sparkles,
  TerminalSquare,
} from "lucide-react";
import type {
  AiPermissionRecord,
  AiPermissionRule,
  LocalMcpClientStatus,
  RemoteMcpClientRecord,
} from "./ai-gateway-types";
import {
  listAiPermissions,
  listLocalMcpClients,
  listRemoteMcpClients,
  setAiPermission,
} from "./lib/ai-gateway";
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
type CapabilityRisk = "destructive" | "open-world" | "sensitive";

interface CapabilityDefinition {
  id: string;
  label: string;
  description: string;
  risk?: CapabilityRisk;
}

interface CapabilityGroup {
  label: string;
  icon: "workspace" | "git" | "command" | "sync" | "sensitive";
  capabilities: CapabilityDefinition[];
}

type PermissionSnapshots = Record<string, AiPermissionRecord[]>;

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
      { id: "git.local", label: "Local Git", description: "Inspect diffs, stage, commit and use local worktrees." },
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
      { id: "sync.read", label: "Inspect sync state", description: "Read remote mappings, plans and synchronization evidence." },
      { id: "sync.execute", label: "Execute safe sync", description: "Run approved synchronization operations." },
      { id: "sync.destructive", label: "Destructive sync", description: "Permit remote trash or local-delete synchronization actions.", risk: "destructive" },
    ],
  },
  {
    label: "Sensitive files",
    icon: "sensitive",
    capabilities: [
      { id: "sensitive.read", label: "Read sensitive files", description: "Read files matched by the sensitive-file policy.", risk: "sensitive" },
      { id: "sensitive.write", label: "Write sensitive files", description: "Modify files matched by the sensitive-file policy.", risk: "sensitive" },
    ],
  },
];

const CAPABILITIES = CAPABILITY_GROUPS.flatMap((group) => group.capabilities);
const HIGH_RISK_CAPABILITIES = new Set(
  CAPABILITIES.filter((capability) => capability.risk).map((capability) => capability.id),
);

const PRESETS: Array<{
  id: PolicyPreset;
  title: string;
  description: string;
  icon: typeof Eye;
}> = [
  {
    id: "read-only",
    title: "Read Only",
    description: "Read workspace files and inspect synchronization state. Mutation capabilities are denied.",
    icon: Eye,
  },
  {
    id: "developer",
    title: "Development Access",
    description: "Read/edit files, run approved project tasks and use local Git. Risky capabilities stay on Ask.",
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

function rulesForPreset(preset: PolicyPreset): Record<string, AiPermissionRule> {
  return Object.fromEntries(CAPABILITIES.map((capability) => [capability.id, presetRule(preset, capability.id)]));
}

function groupIcon(icon: CapabilityGroup["icon"]) {
  if (icon === "git") return <Code2 size={15} />;
  if (icon === "command") return <TerminalSquare size={15} />;
  if (icon === "sync") return <Link2 size={15} />;
  if (icon === "sensitive") return <KeyRound size={15} />;
  return <Bot size={15} />;
}

function riskLabel(risk: CapabilityRisk | undefined) {
  if (risk === "destructive") return "Destructive";
  if (risk === "open-world") return "External write";
  if (risk === "sensitive") return "Sensitive";
  return null;
}

function hasExplicitRules(records: AiPermissionRecord[] | undefined) {
  return records?.some((record) => record.explicit) === true;
}

function recordsMatchRules(
  records: AiPermissionRecord[] | undefined,
  rules: Record<string, AiPermissionRule>,
) {
  if (!records || !hasExplicitRules(records)) return false;
  const byCapability = new Map(records.map((record) => [record.capability, record]));
  return CAPABILITIES.every((capability) => byCapability.get(capability.id)?.rule === rules[capability.id]);
}

export default function AiPolicyPresetPanel({ workspaces, onError }: AiPolicyPresetPanelProps) {
  const [localClients, setLocalClients] = useState<LocalMcpClientStatus[]>([]);
  const [remoteClients, setRemoteClients] = useState<RemoteMcpClientRecord[]>([]);
  const [principal, setPrincipal] = useState("");
  const [workspaceIds, setWorkspaceIds] = useState<string[]>(workspaces[0] ? [workspaces[0].id] : []);
  const [busy, setBusy] = useState<PolicyPreset | "custom" | "refresh" | null>(null);
  const [permissionLoading, setPermissionLoading] = useState(false);
  const [permissionSnapshots, setPermissionSnapshots] = useState<PermissionSnapshots>({});
  const [selectedPreset, setSelectedPreset] = useState<PolicyPreset | null>(null);
  const [lastApplied, setLastApplied] = useState<PolicyPreset | "custom" | null>(null);
  const [customizing, setCustomizing] = useState(false);
  const [customRules, setCustomRules] = useState<Record<string, AiPermissionRule>>({});
  const hydratedPrincipalRef = useRef("");

  const clients = useMemo<PolicyClient[]>(() => [
    ...remoteClients.map((client) => ({ principal: client.principal, label: client.displayName, transport: "remote" as const })),
    ...localClients.map((client) => ({ principal: client.principal, label: client.label, transport: "local" as const })),
  ], [localClients, remoteClients]);

  const workspaceKey = useMemo(() => workspaces.map((workspace) => workspace.id).join("|"), [workspaces]);
  const selectedClient = clients.find((client) => client.principal === principal) ?? null;
  const selectedWorkspaces = workspaces.filter((workspace) => workspaceIds.includes(workspace.id));
  const selectedPresetInfo = PRESETS.find((preset) => preset.id === selectedPreset) ?? null;
  const detailRules: Record<string, AiPermissionRule> = selectedPreset ? rulesForPreset(selectedPreset) : {};
  const activeRules: Record<string, AiPermissionRule> = customizing ? customRules : detailRules;
  const ruleCounts = CAPABILITIES.reduce(
    (counts, capability) => {
      const rule = activeRules[capability.id];
      if (rule) counts[rule] += 1;
      return counts;
    },
    { allow: 0, ask: 0, deny: 0 } as Record<AiPermissionRule, number>,
  );
  const savedPreset = PRESETS.find((preset) => (
    selectedWorkspaces.length > 0
    && selectedWorkspaces.every((workspace) => recordsMatchRules(
      permissionSnapshots[workspace.id],
      rulesForPreset(preset.id),
    ))
  )) ?? null;
  const selectedConfiguredCount = selectedWorkspaces.filter((workspace) => (
    hasExplicitRules(permissionSnapshots[workspace.id])
  )).length;

  useEffect(() => {
    void refreshClients();
  }, []);

  useEffect(() => {
    const validIds = workspaceIds.filter((id) => workspaces.some((workspace) => workspace.id === id));
    if (validIds.length !== workspaceIds.length) setWorkspaceIds(validIds);
    if (validIds.length === 0 && workspaces[0] && hydratedPrincipalRef.current === principal) {
      setWorkspaceIds([workspaces[0].id]);
    }
  }, [workspaceKey]);

  useEffect(() => {
    if (principal && clients.some((client) => client.principal === principal)) return;
    hydratedPrincipalRef.current = "";
    setPrincipal(clients[0]?.principal ?? "");
  }, [clients, principal]);

  useEffect(() => {
    if (!principal || workspaces.length === 0) {
      setPermissionSnapshots({});
      return;
    }

    let cancelled = false;
    setPermissionLoading(true);
    void loadPermissionSnapshots(principal)
      .then((snapshots) => {
        if (cancelled) return;
        setPermissionSnapshots(snapshots);
        if (hydratedPrincipalRef.current !== principal) {
          const configuredWorkspaceIds = workspaces
            .filter((workspace) => hasExplicitRules(snapshots[workspace.id]))
            .map((workspace) => workspace.id);
          setWorkspaceIds(configuredWorkspaceIds.length > 0
            ? configuredWorkspaceIds
            : workspaces[0] ? [workspaces[0].id] : []);
          hydratedPrincipalRef.current = principal;
          setSelectedPreset(null);
          setLastApplied(null);
          setCustomizing(false);
        }
      })
      .catch((error) => {
        if (!cancelled) onError(String(error));
      })
      .finally(() => {
        if (!cancelled) setPermissionLoading(false);
      });

    return () => {
      cancelled = true;
    };
  }, [principal, workspaceKey]);

  async function loadPermissionSnapshots(clientPrincipal: string): Promise<PermissionSnapshots> {
    const entries = await Promise.all(workspaces.map(async (workspace) => (
      [workspace.id, await listAiPermissions(workspace.id, clientPrincipal)] as const
    )));
    return Object.fromEntries(entries);
  }

  async function refreshClients() {
    try {
      setBusy("refresh");
      const [local, remote] = await Promise.all([listLocalMcpClients(), listRemoteMcpClients()]);
      setLocalClients(local);
      setRemoteClients(remote);
      if (principal && workspaces.length > 0) {
        const snapshots = await loadPermissionSnapshots(principal);
        setPermissionSnapshots(snapshots);
      }
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  function selectPrincipal(nextPrincipal: string) {
    hydratedPrincipalRef.current = "";
    setPrincipal(nextPrincipal);
    setPermissionSnapshots({});
    setSelectedPreset(null);
    setLastApplied(null);
    setCustomizing(false);
  }

  function toggleWorkspace(id: string) {
    setWorkspaceIds((current) => current.includes(id)
      ? current.filter((value) => value !== id)
      : [...current, id]);
    setLastApplied(null);
  }

  function toggleAllWorkspaces() {
    if (workspaceIds.length === workspaces.length) {
      setWorkspaceIds([]);
      setLastApplied(null);
      return;
    }
    setWorkspaceIds(workspaces.map((workspace) => workspace.id));
    setLastApplied(null);
  }

  function openPreset(preset: PolicyPreset) {
    if (selectedPreset === preset && !customizing) {
      setSelectedPreset(null);
      return;
    }
    setSelectedPreset(preset);
    setCustomizing(false);
    setCustomRules(rulesForPreset(preset));
  }

  function startCustomize() {
    if (!selectedPreset) return;
    setCustomRules(rulesForPreset(selectedPreset));
    setCustomizing(true);
  }

  async function applyRules(
    rules: Record<string, AiPermissionRule>,
    label: string,
    busyState: PolicyPreset | "custom",
  ) {
    if (!selectedClient || selectedWorkspaces.length === 0) return;
    const highRiskAllowed = CAPABILITIES.some(
      (capability) => HIGH_RISK_CAPABILITIES.has(capability.id) && rules[capability.id] === "allow",
    );
    if (highRiskAllowed) {
      const names = selectedWorkspaces.map((workspace) => workspace.name).join(", ");
      const confirmed = window.confirm(
        `Apply ${label} to ${selectedClient.label} for ${selectedWorkspaces.length} workspace(s)?\n\n${names}\n\nThis selection allows one or more high-risk capabilities such as deletion, remote Git writes, sensitive files or destructive synchronization.`,
      );
      if (!confirmed) return;
    }

    try {
      setBusy(busyState);
      for (const workspace of selectedWorkspaces) {
        for (const capability of CAPABILITIES) {
          await setAiPermission(
            workspace.id,
            selectedClient.principal,
            capability.id,
            rules[capability.id] ?? "ask",
          );
        }
      }

      const snapshots = await loadPermissionSnapshots(selectedClient.principal);
      for (const workspace of selectedWorkspaces) {
        if (!recordsMatchRules(snapshots[workspace.id], rules)) {
          throw new Error(`AtrisBridge could not verify the saved AI permission policy for '${workspace.name}'.`);
        }
      }
      setPermissionSnapshots(snapshots);
      setLastApplied(busyState);
    } catch (error) {
      onError(String(error));
    } finally {
      setBusy(null);
    }
  }

  async function applyPreset(preset: PolicyPreset) {
    await applyRules(rulesForPreset(preset), PRESETS.find((item) => item.id === preset)?.title ?? preset, preset);
  }

  async function applyCustom() {
    await applyRules(customRules, "Custom policy", "custom");
  }

  return (
    <section className="ai-gateway-section ai-bulk-policy" aria-labelledby="ai-bulk-policy-title">
      <header className="ai-bulk-policy-heading">
        <div>
          <span className="section-kicker">AI client permissions</span>
          <h3 id="ai-bulk-policy-title">Presets, workspace scope &amp; custom capabilities</h3>
          <p>Choose a client and one or more workspaces, review a preset, then apply it as-is or customize individual capabilities. Saved permissions are loaded back from AtrisBridge so this scope reflects the workspaces already configured for the selected client.</p>
        </div>
        <button className="button secondary" type="button" onClick={() => void refreshClients()} disabled={busy !== null || permissionLoading}>
          <RefreshCw size={13} className={busy === "refresh" || permissionLoading ? "spin" : ""} /> Refresh clients
        </button>
      </header>

      <div className="ai-bulk-policy-config">
        <label className="ai-bulk-client-select">
          <span>Client</span>
          <div className="select-wrap">
            <select value={principal} onChange={(event) => selectPrincipal(event.target.value)} disabled={clients.length === 0 || busy !== null || permissionLoading}>
              {clients.length === 0 && <option value="">No authenticated client yet</option>}
              <optgroup label="Remote">
                {remoteClients.length === 0
                  ? <option value="" disabled>ChatGPT — connect with OAuth to configure</option>
                  : remoteClients.map((client) => <option key={client.principal} value={client.principal}>{client.displayName}</option>)}
              </optgroup>
              {localClients.length > 0 && (
                <optgroup label="Local">
                  {localClients.map((client) => <option key={client.principal} value={client.principal}>{client.label}</option>)}
                </optgroup>
              )}
            </select>
            <ChevronDown size={13} />
          </div>
          <small className="ai-client-discovery-note">
            {permissionLoading
              ? "Loading saved workspace permissions..."
              : remoteClients.length > 0
                ? "Remote OAuth identity discovered from AtrisHub; saved AtrisBridge workspace rules are loaded locally."
                : "ChatGPT is supported now, but a secure remote principal is created only after OAuth authorization."}
          </small>
        </label>

        <div className="ai-bulk-workspace-select">
          <div className="ai-bulk-workspace-title">
            <span>Workspaces</span>
            <button type="button" onClick={toggleAllWorkspaces} disabled={workspaces.length === 0 || busy !== null || permissionLoading}>
              {workspaceIds.length === workspaces.length && workspaces.length > 0 ? "Clear all" : "Select all"}
            </button>
          </div>
          <div className="ai-bulk-workspace-grid">
            {workspaces.length === 0 ? (
              <span className="ai-bulk-empty">No workspace available.</span>
            ) : workspaces.map((workspace) => {
              const selected = workspaceIds.includes(workspace.id);
              return (
                <label className={selected ? "selected" : ""} key={workspace.id}>
                  <input type="checkbox" checked={selected} onChange={() => toggleWorkspace(workspace.id)} disabled={busy !== null || permissionLoading} />
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
          const selected = selectedPreset === preset.id;
          const applied = lastApplied === preset.id || savedPreset?.id === preset.id;
          return (
            <button
              type="button"
              className={`ai-preset-card ${preset.id}${selected ? " selected" : ""}${applied ? " applied" : ""}`}
              key={preset.id}
              onClick={() => openPreset(preset.id)}
              disabled={!selectedClient || selectedWorkspaces.length === 0 || busy !== null || permissionLoading}
              aria-expanded={selected}
            >
              <span className="ai-preset-icon"><Icon size={18} /></span>
              <span className="ai-preset-copy"><strong>{preset.title}</strong><small>{preset.description}</small></span>
              <span className="ai-preset-action">
                {busy === preset.id ? <RefreshCw className="spin" size={13} /> : applied ? <Check size={13} /> : selected ? <ChevronUp size={13} /> : <ChevronDown size={13} />}
              </span>
            </button>
          );
        })}
      </div>

      {selectedPresetInfo && (
        <div className="ai-policy-detail-panel">
          <div className="ai-policy-detail-heading">
            <div>
              <span className="section-kicker">{customizing ? "Custom policy" : `${selectedPresetInfo.title} details`}</span>
              <h4>{customizing ? `Customize from ${selectedPresetInfo.title}` : `Review ${selectedPresetInfo.title}`}</h4>
              <p>{customizing ? "Change any capability before applying. These rules will be written to every selected workspace for this exact client principal." : "Nothing changes until you press Apply. Saved rules are read back after applying and the operation is treated as failed if verification does not match."}</p>
            </div>
            <div className="ai-policy-rule-summary" aria-label="Policy rule summary">
              <span className="allow">{ruleCounts.allow} Allow</span>
              <span className="ask">{ruleCounts.ask} Ask</span>
              <span className="deny">{ruleCounts.deny} Deny</span>
            </div>
          </div>

          <div className="ai-policy-detail-groups">
            {CAPABILITY_GROUPS.map((group) => (
              <section className="ai-policy-detail-group" key={group.label}>
                <header><span>{groupIcon(group.icon)}</span><strong>{group.label}</strong></header>
                {group.capabilities.map((capability) => {
                  const rule = activeRules[capability.id] ?? "ask";
                  const risk = riskLabel(capability.risk);
                  return (
                    <div className={`ai-policy-detail-row ${capability.risk ? `risk-${capability.risk}` : ""}`} key={capability.id}>
                      <div className="ai-policy-detail-copy">
                        <div><strong>{capability.label}</strong>{risk && <span className={`ai-risk-mini ${capability.risk}`}><ShieldAlert size={10} /> {risk}</span>}</div>
                        <p>{capability.description}</p>
                      </div>
                      {customizing ? (
                        <div className="ai-custom-rule-segment" aria-label={`${capability.label} permission`}>
                          {(["deny", "ask", "allow"] as AiPermissionRule[]).map((option) => (
                            <button
                              key={option}
                              type="button"
                              className={rule === option ? `active ${option}` : ""}
                              onClick={() => setCustomRules((current) => ({ ...current, [capability.id]: option }))}
                              disabled={busy !== null}
                            >
                              {option === "deny" ? "Deny" : option === "ask" ? "Ask" : "Allow"}
                            </button>
                          ))}
                        </div>
                      ) : (
                        <span className={`ai-rule-pill ${rule}`}>{rule === "allow" ? "Allow" : rule === "deny" ? "Deny" : "Ask"}</span>
                      )}
                    </div>
                  );
                })}
              </section>
            ))}
          </div>

          <div className="ai-policy-detail-actions">
            <div>
              <strong>{selectedClient?.label}</strong>
              <span>
                {selectedWorkspaces.length} workspace(s) selected
                {savedPreset ? ` · Saved: ${savedPreset.title}` : selectedConfiguredCount > 0 ? ` · ${selectedConfiguredCount} with saved custom/mixed rules` : " · No saved rules in this selection"}
              </span>
            </div>
            <div>
              {customizing ? (
                <>
                  <button className="button secondary" type="button" onClick={() => setCustomizing(false)} disabled={busy !== null}>Back to preset</button>
                  <button className="button primary" type="button" onClick={() => void applyCustom()} disabled={busy !== null}>
                    {busy === "custom" ? <RefreshCw className="spin" size={13} /> : <SlidersHorizontal size={13} />}
                    Apply custom
                  </button>
                </>
              ) : (
                <>
                  <button className="button secondary" type="button" onClick={startCustomize} disabled={busy !== null}><SlidersHorizontal size={13} /> Customize</button>
                  <button className="button primary" type="button" onClick={() => void applyPreset(selectedPresetInfo.id)} disabled={busy !== null}>
                    {busy === selectedPresetInfo.id ? <RefreshCw className="spin" size={13} /> : <Sparkles size={13} />}
                    Apply {selectedPresetInfo.title}
                  </button>
                </>
              )}
            </div>
          </div>
        </div>
      )}

      <div className="ai-bulk-policy-note">
        <Bot size={14} />
        <p>
          <strong>{selectedClient?.label ?? "Select an authenticated client"}</strong>
          {permissionLoading
            ? " · Loading saved permissions."
            : selectedWorkspaces.length > 0
              ? ` · ${selectedWorkspaces.length} workspace(s) selected${savedPreset ? ` · ${savedPreset.title} is saved for this selection.` : selectedConfiguredCount > 0 ? " · Saved custom or mixed rules detected." : "."}`
              : " · Select at least one workspace."}
          {" "}These rules are scoped to AtrisBridge workspace authority; they do not enable or disable ChatGPT Developer Mode.
        </p>
      </div>
    </section>
  );
}
