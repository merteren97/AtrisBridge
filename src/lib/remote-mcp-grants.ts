import type { RemoteMcpClientRecord, RemoteMcpGrantClient } from "../ai-gateway-types";

// Only the AtrisHub grant list establishes whether a remote client is
// currently authorized. Observed clients are untrusted process-local telemetry.
export function combineAuthorizedRemoteMcpClients(
  grants: RemoteMcpGrantClient[],
  observed: RemoteMcpClientRecord[],
): RemoteMcpClientRecord[] {
  const observations = new Map(observed.map((client) => [client.principal, client]));

  return grants.map((grant): RemoteMcpClientRecord => {
    const seen = observations.get(grant.principal);
    return {
      principal: grant.principal,
      displayName: grant.displayName,
      firstSeenAt: seen?.firstSeenAt ?? "",
      lastSeenAt: seen?.lastSeenAt ?? "",
      observed: Boolean(seen),
      activeOnThisDevice: grant.activeOnThisDevice,
      relayReadyOnThisDevice: grant.relayReadyOnThisDevice,
      authorizedAt: grant.authorizedAt,
      authorizationUpdatedAt: grant.updatedAt,
    };
  }).sort((left, right) => {
    if (left.relayReadyOnThisDevice !== right.relayReadyOnThisDevice) {
      return left.relayReadyOnThisDevice === true
        ? -1
        : right.relayReadyOnThisDevice === true
          ? 1
          : 0;
    }
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
