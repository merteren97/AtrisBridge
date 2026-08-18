import { invoke } from "@tauri-apps/api/core";
import type {
  AiPermissionRecord,
  AiPermissionRule,
  LocalMcpClientKind,
  LocalMcpClientStatus,
  RemoteMcpClientRecord,
  RemoteMcpGrantClient,
  RemoteMcpRelayStatus,
} from "../ai-gateway-types";

export async function listLocalMcpClients(): Promise<LocalMcpClientStatus[]> {
  return invoke<LocalMcpClientStatus[]>("list_local_mcp_clients");
}

export async function registerLocalMcpClient(
  kind: LocalMcpClientKind,
): Promise<LocalMcpClientStatus> {
  return invoke<LocalMcpClientStatus>("register_local_mcp_client", { kind });
}

export async function unregisterLocalMcpClient(
  kind: LocalMcpClientKind,
): Promise<LocalMcpClientStatus> {
  return invoke<LocalMcpClientStatus>("unregister_local_mcp_client", { kind });
}

export async function listRemoteMcpClients(): Promise<RemoteMcpClientRecord[]> {
  const observed = await invoke<RemoteMcpClientRecord[]>("list_remote_mcp_clients");
  let grants: RemoteMcpGrantClient[] = [];
  try {
    grants = await invoke<RemoteMcpGrantClient[]>("list_remote_mcp_grant_clients");
  } catch {
    grants = [];
  }

  const clients = new Map<string, RemoteMcpClientRecord>();
  for (const grant of grants) {
    clients.set(grant.principal, {
      principal: grant.principal,
      displayName: grant.displayName,
      firstSeenAt: "",
      lastSeenAt: "",
      observed: false,
      activeOnThisDevice: grant.activeOnThisDevice,
      authorizedAt: grant.authorizedAt,
      authorizationUpdatedAt: grant.updatedAt,
    });
  }
  for (const client of observed) {
    const discovered = clients.get(client.principal);
    clients.set(client.principal, {
      ...client,
      displayName: discovered?.displayName ?? client.displayName,
      activeOnThisDevice: discovered?.activeOnThisDevice,
      authorizedAt: discovered?.authorizedAt,
      authorizationUpdatedAt: discovered?.authorizationUpdatedAt,
      observed: true,
    });
  }

  return [...clients.values()].sort((left, right) => {
    if (left.activeOnThisDevice !== right.activeOnThisDevice) {
      return left.activeOnThisDevice === true ? -1 : right.activeOnThisDevice === true ? 1 : 0;
    }
    if (left.observed !== right.observed) return left.observed ? -1 : 1;
    const leftUpdated = left.authorizationUpdatedAt ?? "";
    const rightUpdated = right.authorizationUpdatedAt ?? "";
    if (leftUpdated !== rightUpdated) return rightUpdated.localeCompare(leftUpdated);
    return left.displayName.localeCompare(right.displayName);
  });
}

export async function routeRemoteMcpClientHere(principal: string): Promise<boolean> {
  return invoke<boolean>("route_remote_mcp_grant_client_here", { principal });
}

export async function revokeRemoteMcpClient(principal: string): Promise<boolean> {
  return invoke<boolean>("revoke_remote_mcp_grant_client", { principal });
}

export async function getRemoteMcpRelayStatus(): Promise<RemoteMcpRelayStatus> {
  return invoke<RemoteMcpRelayStatus>("remote_mcp_relay_status");
}

export async function retryRemoteMcpRelay(): Promise<RemoteMcpRelayStatus> {
  return invoke<RemoteMcpRelayStatus>("retry_remote_mcp_relay");
}

export async function listAiPermissions(
  workspaceId: string,
  clientId: string,
): Promise<AiPermissionRecord[]> {
  return invoke<AiPermissionRecord[]>("list_ai_permissions", { workspaceId, clientId });
}

export async function setAiPermission(
  workspaceId: string,
  clientId: string,
  capability: string,
  rule: AiPermissionRule,
): Promise<AiPermissionRecord> {
  return invoke<AiPermissionRecord>("set_ai_permission", {
    workspaceId,
    clientId,
    capability,
    rule,
  });
}

export async function resetAiPermission(
  workspaceId: string,
  clientId: string,
  capability: string,
): Promise<AiPermissionRecord> {
  return invoke<AiPermissionRecord>("reset_ai_permission", {
    workspaceId,
    clientId,
    capability,
  });
}
