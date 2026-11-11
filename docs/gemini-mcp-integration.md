# Gemini MCP integration: architecture and rollout plan

> Status (2026-11-12): Gemini CLI Desktop changes are proposed in this branch. Gemini Apps web interoperability is **not yet live-tested** and is subject to Google's account/region availability. This document is a design and validation plan, not a production certification.

## Objective

Connect Gemini CLI and, where Google enables Custom Connected Apps, Gemini Web to the existing AtrisBridge workspace authority. Preserve Codex, Claude Code and ChatGPT behavior. Never introduce an independent filesystem/shell API or silently grant workspace access.

## Existing implementation (verified in GitHub)

- **AtrisBridge Desktop:** `src-tauri/src/mcp_core.rs` and `mcp_dispatch.rs` define the existing tool catalog; `local_mcp_ipc.rs` authenticates a local companion over loopback with an OS-vault credential. `remote_mcp_relay.rs` connects Desktop to AtrisHub over WebSocket.
- **AtrisHub:** In the separate `merteren97/AtrisHub` repository, `services/mcp-gateway/mcp-http.mjs` implements the public HTTPS MCP endpoint and legacy protocol compatibility. `provider.mjs` implements OAuth/DCR/PKCE; `active-device.mjs` handles separate authorized grants and routing.
- **Existing endpoints:** Public Streamable HTTP: `https://atrishub.com/api/mcp/v1/mcp`. Desktop relay: `wss://atrishub.com/api/mcp/relay/v1/connect`.
- **Authorization:** Local clients use fixed transport principals (`mcp.codex`, `mcp.claude`, now `mcp.gemini`); remote OAuth grants use independent `mcp.remote.<32 hex>` principals. Existing capability gates and per-workspace rules remain authoritative.

## Phase A: Gemini CLI (code delivered by this PR)

1. Detect the installed `gemini` executable using the existing trusted PATH policy.
2. Register the packaged local stdio companion in **user scope** through the official CLI (without `--trust`):

   `gemini mcp add --scope user atrisbridge <managed-companion-path> -- --client gemini`

3. Read and parse `~/.gemini/settings.json` in a bounded fashion. Accept only an exact registered managed companion and the `["--client","gemini"]` argument pair.
4. Preserve foreign or non-owned `atrisbridge` entries, report conflict, and repair only obsolete versions within AtrisBridge's managed companion directory.
5. Use `gemini mcp remove --scope user atrisbridge` only after a validated owned registration.
6. Recognize `--client gemini` in the companion and map it to independent `mcp.gemini`; show Gemini in Connections and the existing Permissions UI.
7. Do not grant any workspace permissions as part of registration.

**CLI verification:** With AtrisBridge open and a workspace configured, connect Gemini from Desktop Settings. Check the user settings JSON. From a trusted project directory run `gemini mcp list`; within Gemini run `/mcp list`. Ensure the client can list workspaces but cannot read, edit or execute without its own workspace grant. Enable Read Only for one workspace and verify read succeeds while mutation is denied. Try repair, remove, foreign-entry conflict, and Codex/Claude regressions. Gemini CLI can report an untrusted folder as Disconnected even when configuration is correct.

## Phase B: Gemini Apps Web (eligibility + interoperability gate)

The browser must **not** connect to the Desktop's loopback endpoint. It should use the existing public HTTPS MCP endpoint and AtrisHub OAuth/relay.

1. On an eligible personal Google account, open gemini.google.com → Settings → Connected Apps → Custom apps → Add a custom app.
2. Enter `https://atrishub.com/api/mcp/v1/mcp`. Gemini performs OAuth discovery and Dynamic Client Registration, then the user signs in to AtrisHub and consents to a specific Desktop.
3. Confirm the Desktop relay is **Ready** and the actual OAuth grant appears in Authorized remote clients.
4. Set workspace permissions for that exact `mcp.remote.*` grant; do not use a hardcoded or display-name-derived Gemini principal.
5. Start a fresh Gemini chat and inspect the tool discovery/call flow with the Gemini Developer Mode inspector. Use `@AtrisBridge` only if that is the actual connected app name.
6. Routing to a different Desktop must be explicit. Existing sessions are pinned to their original device.

**Important verified compatibility:** Google Connected Apps supports MCP through protocol `2025-11-25` (and Streamable HTTP). AtrisHub currently supports `2026-07-28` plus legacy `2025-11-25` and `2025-06-18` handshakes, and strips tasks-only tools for legacy clients. Its OAuth server implements DCR, PKCE, refresh tokens and protected-resource discovery. This suggests reuse is possible but **does not prove Gemini's live HTTP headers, schema, OAuth grants and tool execution work**. Test rather than automatically modifying the server.

**Availability:** Google currently documents custom Gemini Apps MCP connections for US users aged 18+, personal Google accounts, English, with Keep Activity enabled. It is not currently documented as available for Turkish personal accounts or work/school accounts. Do not present the browser Connect action as universally available.

### Web acceptance tests

| Test | Pass criterion |
| --- | --- |
| OAuth metadata, DCR, PKCE | Exact issuer/resource audience, consent, redirect validation, refresh, revoke |
| Initialize | Negotiates Google's supported MCP version and returns tools capability |
| Tools | `tools/list` and a safe `tools/call` accepted; no unsupported task extension required |
| Permissions | Gemini and ChatGPT grants remain distinct; new grants get no automatic workspace access |
| Read-only | Read works only after grant; changeset apply, command, push and destructive sync remain denied |
| Multi-device | Ready relay required; route-switch works for new sessions without migrating pinned sessions |
| Offline/revoked | Explicit unavailable/unauthorized response; no fabricated success |
| Regression | Existing modern/legacy ChatGPT, Codex, Claude and sync flows unchanged |

If tests fail, make a **separate, narrow AtrisHub server PR** with request fixtures in `scripts/test-mcp-gateway-legacy-compat.mjs` and OAuth tests. Prefer the shared endpoint. Add a versioned endpoint only if clients require irreconcilable contracts; never bypass token audience, identity derivation or permission checks.

## Phase C: release hardening

- Prefer provider-neutral remote connection labels; show configured vs authorized vs relay-ready vs workspace-granted vs tool-verified states separately.
- Preserve safety annotations for read-only, destructive and sensitive tools. Keep schema version changes deliberate; Google takes schema snapshots during registration.
- Use bounded metadata-only connection diagnostics. Do not log credentials, workspace contents or raw token-bearing headers.
- Require local Rust tests, TypeScript build and real Windows/Linux CLI smoke tests. Gate web launch on an eligible account and a full OAuth + MCP test.
- Assess native Antigravity CLI support separately; do not assume it uses Gemini CLI's executable or settings directory.

## Recovery / rollback

For Gemini CLI, remove only the owned user-scoped `atrisbridge` registration. No workspace files or server grants are deleted. For Gemini Web, revoke that exact remote OAuth grant from AtrisBridge; local CLI principal and all other clients remain unaffected. Roll back this Desktop PR without changing AtrisHub's ChatGPT server path.

## Official references

- https://geminicli.com/docs/tools/mcp-server/
- https://support.google.com/gemini/answer/17209137
- https://developers.google.com/connected-apps/mcp/server-optimization
- https://developers.google.com/connected-apps/mcp/oauth
- https://developers.google.com/connected-apps/mcp/dev-mode/usage
