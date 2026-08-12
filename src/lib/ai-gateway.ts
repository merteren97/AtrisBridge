import { invoke } from "@tauri-apps/api/core";
import type {
  AiPermissionRecord,
  AiPermissionRule,
  LocalMcpClientKind,
  LocalMcpClientStatus,
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