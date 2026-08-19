export type LocalMcpClientKind = "codex" | "claude";
export type LocalMcpRegistrationState =
  | "companion_unavailable"
  | "not_installed"
  | "not_registered"
  | "registered"
  | "update_available"
  | "conflict"
  | "error";

export interface LocalMcpClientStatus {
  kind: LocalMcpClientKind;
  label: string;
  principal: string;
  executableDetected: boolean;
  version: string | null;
  registrationState: LocalMcpRegistrationState;
  registrationHealthy: boolean;
  managedCompanionReady: boolean;
  managedCompanionVersion: string;
  canRegister: boolean;
  canRemove: boolean;
  detail: string;
}

export interface RemoteMcpGrantClient {
  principal: string;
  displayName: string;
  activeOnThisDevice: boolean;
  relayReadyOnThisDevice: boolean;
  authorizedAt: string | null;
  updatedAt: string | null;
}

export interface RemoteMcpClientRecord {
  principal: string;
  displayName: string;
  firstSeenAt: string;
  lastSeenAt: string;
  observed?: boolean;
  activeOnThisDevice?: boolean;
  relayReadyOnThisDevice?: boolean;
  authorizedAt?: string | null;
  authorizationUpdatedAt?: string | null;
}

export type RemoteMcpRelayState = "signed_out" | "connecting" | "online" | "reconnecting";

export interface RemoteMcpRelayStatus {
  started: boolean;
  state: RemoteMcpRelayState;
  observedClients: number;
  connectorUrl: string;
  lastError: string | null;
  lastAttemptAt: string | null;
  lastConnectedAt: string | null;
  lastFrameAt: string | null;
  lastServerPingAt: string | null;
  lastPongAt: string | null;
  lastDisconnectCode: string | null;
  reconnectAttempts: number;
}

export type AiPermissionRule = "deny" | "ask" | "allow";

export interface AiPermissionRecord {
  workspaceId: string;
  clientId: string;
  capability: string;
  rule: AiPermissionRule;
  explicit: boolean;
  updatedAt: string | null;
}
