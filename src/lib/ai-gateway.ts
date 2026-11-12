import { invoke } from "@tauri-apps/api/core";
import { combineAuthorizedRemoteMcpClients } from "./remote-mcp-grants";
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

// AtrisHub is the authority for live OAuth grants. The Desktop's observed
// client cache is telemetry only: it must never resurrect a revoked grant.
export async function listRemoteMcpClients(): Promise<RemoteMcpClientRecord[]> {
  // Grant discovery failure must propagate. Treating it as an empty successful
  // lookup would falsely resurrect revoked process-local observed clients.
  const [grants, observed] = await Promise.all([
    invoke<RemoteMcpGrantClient[]>("list_remote_mcp_grant_clients"),
    invoke<RemoteMcpClientRecord[]>("list_remote_mcp_clients").catch(() => []),
  ]);
  return combineAuthorizedRemoteMcpClients(grants, observed);
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
