#[cfg(test)]
use std::collections::HashSet;

use serde::Serialize;
use serde_json::{json, Value};

use crate::ai_gateway::AI_CAPABILITIES;

pub const MCP_PROTOCOL_VERSION: &str = "2026-07-28";
pub const MCP_TASK_EXTENSION: &str = "io.modelcontextprotocol/tasks";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpCoreManifest {
    pub protocol_version: &'static str,
    pub state_model: &'static str,
    pub principal_model: &'static str,
    pub extensions: Vec<&'static str>,
    pub instructions: &'static str,
    pub tools: Vec<McpToolContract>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolContract {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub required_capabilities: Vec<&'static str>,
    pub requires_session: bool,
    pub requires_isolated_worktree: bool,
    pub input_schema: Value,
    pub annotations: McpToolAnnotations,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution: Option<McpToolExecution>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolAnnotations {
    pub read_only_hint: bool,
    pub destructive_hint: bool,
    pub idempotent_hint: bool,
    pub open_world_hint: bool,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct McpToolExecution {
    pub task_support: &'static str,
}

#[tauri::command]
pub fn ai_mcp_core_manifest() -> McpCoreManifest {
    manifest()
}

pub fn manifest() -> McpCoreManifest {
    McpCoreManifest {
        protocol_version: MCP_PROTOCOL_VERSION,
        state_model: "stateless_mcp_explicit_atris_ai_session_handle",
        principal_model: "authenticated_transport_principal_plus_explicit_workspace_session",
        extensions: vec![MCP_TASK_EXTENSION],
        instructions: "AtrisBridge is the sole local workspace authority. The transport authenticates the AI client principal; model-visible arguments never choose clientId. Use workspace_list and session_open to bootstrap an explicit AtrisBridge workspace session, then include that session handle on every session-bound tool. Never infer or construct absolute local paths, never request raw shell/rclone/Git passthrough, and never bypass workspace permissions. Session mode determines whether workspace operations run directly or inside an isolated worktree; fixed command profiles require command.execute permission and follow that session mode. Review changes before remote Git or destructive workspace operations.",
        tools: tool_catalog(),
    }
}

pub fn tool_catalog() -> Vec<McpToolContract> {
    vec![
        tool(
            "workspace_list",
            "List authorized workspaces",
            "List only workspace identities visible to the authenticated transport principal. Local absolute paths are never exposed.",
            &[],
            false,
            schema(json!({}), &[]),
            read_only(false),
            None,
        ),
        tool(
            "session_open",
            "Open workspace session",
            "Open an explicit AtrisBridge AI workspace session for the authenticated transport principal. Client identity is supplied by the transport and is never a model argument.",
            &[],
            false,
            schema(
                json!({
                    "workspaceId": {"type": "string", "minLength": 1, "maxLength": 128},
                    "mode": {"type": "string", "enum": ["direct", "isolated_worktree"]},
                    "requestedCapabilities": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": AI_CAPABILITIES.len(),
                        "uniqueItems": true,
                        "items": capability_schema()
                    },
                    "ttlMinutes": {"type": "integer", "minimum": 5, "maximum": 1440}
                }),
                &["workspaceId", "mode", "requestedCapabilities"],
            ),
            mutating(false, false, false),
            None,
        ),
        tool(
            "session_status",
            "Inspect workspace session",
            "Inspect the current explicit AtrisBridge AI session and its granted capability set.",
            &[],
            false,
            session_only_schema(),
            read_only(false),
            None,
        ),
        tool(
            "session_close",
            "Close workspace session",
            "Close the current AtrisBridge AI workspace session without inferring or changing client identity.",
            &[],
            false,
            session_only_schema(),
            mutating(false, false, false),
            None,
        ),
        tool(
            "workspace_stat",
            "Inspect workspace file",
            "Inspect a workspace-relative regular file without exposing an absolute local path.",
            &["workspace.read"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "relativePath": relative_path_schema()
                }),
                &["sessionId", "relativePath"],
            ),
            read_only(false),
            None,
        ),
        tool(
            "workspace_read_text",
            "Read text file",
            "Read a bounded UTF-8 line window from a workspace-relative file. Sensitive files still require sensitive.read.",
            &["workspace.read"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "relativePath": relative_path_schema(),
                    "startLine": {"type": ["integer", "null"], "minimum": 1},
                    "endLine": {"type": ["integer", "null"], "minimum": 1}
                }),
                &["sessionId", "relativePath"],
            ),
            read_only(false),
            None,
        ),
        tool(
            "workspace_search",
            "Search workspace",
            "Search bounded workspace text using AtrisBridge path, ignore, sensitive-file, and symlink policies. includeSensitive additionally requires sensitive.read at execution time.",
            &["workspace.read"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "query": {"type": "string", "minLength": 1, "maxLength": 512},
                    "limit": {"type": ["integer", "null"], "minimum": 1, "maximum": 200},
                    "includeSensitive": {"type": "boolean", "default": false}
                }),
                &["sessionId", "query"],
            ),
            read_only(false),
            None,
        ),
        tool(
            "changeset_prepare",
            "Prepare workspace changeset",
            "Prepare a recoverable create/replace/delete/move changeset. Replace, delete, and move require current BLAKE3 evidence.",
            &["workspace.edit"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "changes": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 100,
                        "items": change_request_schema()
                    }
                }),
                &["sessionId", "changes"],
            ),
            mutating(false, false, false),
            None,
        ),
        tool(
            "changeset_apply",
            "Apply workspace changeset",
            "Apply a previously prepared AtrisBridge changeset after stale-evidence and recovery preflight.",
            &["workspace.edit"],
            false,
            id_schema("changesetId"),
            mutating(true, false, false),
            None,
        ),
        tool(
            "changeset_undo",
            "Undo workspace changeset",
            "Rollback an applied AtrisBridge changeset only when current filesystem evidence makes rollback safe.",
            &["workspace.edit"],
            false,
            id_schema("changesetId"),
            mutating(true, false, false),
            None,
        ),
        tool(
            "changeset_get",
            "Inspect changeset",
            "Read metadata for a changeset owned by the current AtrisBridge AI session/workspace.",
            &["workspace.read"],
            false,
            id_schema("changesetId"),
            read_only(false),
            None,
        ),
        tool(
            "changeset_list",
            "List session changesets",
            "List bounded changeset metadata visible to the current AtrisBridge AI session.",
            &["workspace.read"],
            false,
            session_limit_schema(),
            read_only(false),
            None,
        ),
        tool(
            "worktree_provision",
            "Provision isolated worktree",
            "Provision and verify an AtrisBridge-owned Git worktree for an isolated AI session.",
            &["git.local"],
            true,
            session_only_schema(),
            mutating(false, false, false),
            None,
        ),
        tool(
            "worktree_list",
            "List isolated worktrees",
            "List AtrisBridge-managed worktree metadata for the current AI session/workspace.",
            &["git.local"],
            true,
            session_only_schema(),
            read_only(false),
            None,
        ),
        tool(
            "worktree_discard",
            "Discard isolated worktree",
            "Remove an AtrisBridge-managed isolated worktree. Dirty worktrees require an explicit force decision at the authority layer.",
            &["git.local"],
            true,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "force": {"type": "boolean", "default": false}
                }),
                &["sessionId"],
            ),
            mutating(true, false, false),
            None,
        ),
        tool(
            "git_status",
            "Git status",
            "Read bounded Git status for the session worktree.",
            &["git.local"],
            false,
            session_only_schema(),
            read_only(false),
            None,
        ),
        tool(
            "git_diff",
            "Git diff",
            "Read bounded Git diff with external diff, textconv, and rename detection disabled.",
            &["git.local", "workspace.read"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "staged": {"type": "boolean", "default": false},
                    "relativePath": {"type": ["string", "null"], "maxLength": 1024}
                }),
                &["sessionId"],
            ),
            read_only(false),
            None,
        ),
        tool(
            "git_log",
            "Git log",
            "Read a bounded commit log for the session worktree.",
            &["git.local"],
            false,
            session_limit_schema(),
            read_only(false),
            None,
        ),
        tool(
            "git_branches",
            "List Git branches",
            "List bounded local branch metadata for the session repository.",
            &["git.local"],
            false,
            session_only_schema(),
            read_only(false),
            None,
        ),
        tool(
            "git_stage",
            "Stage files",
            "Stage explicit regular-file paths and bind ownership to Git index evidence for the current AI session.",
            &["git.local"],
            false,
            session_paths_schema(),
            mutating(false, false, false),
            None,
        ),
        tool(
            "git_unstage",
            "Unstage files",
            "Unstage explicit paths only when the session-owned Git index evidence still matches.",
            &["git.local"],
            false,
            session_paths_schema(),
            mutating(false, false, false),
            None,
        ),
        tool(
            "git_commit",
            "Commit staged AI changes",
            "Commit only paths whose staged index evidence is owned by the current AI session.",
            &["git.local"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "message": {"type": "string", "minLength": 1, "maxLength": 4096}
                }),
                &["sessionId", "message"],
            ),
            mutating(false, false, false),
            None,
        ),
        tool(
            "git_create_branch",
            "Create Git branch",
            "Create a validated local branch in the session repository.",
            &["git.local"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "branchName": {"type": "string", "minLength": 1, "maxLength": 255}
                }),
                &["sessionId", "branchName"],
            ),
            mutating(false, false, false),
            None,
        ),
        tool(
            "git_revert",
            "Revert commit",
            "Create a non-interactive revert for an explicit commit. AtrisBridge aborts the revert automatically on failure.",
            &["git.local", "workspace.edit"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "commit": {"type": "string", "minLength": 7, "maxLength": 128}
                }),
                &["sessionId", "commit"],
            ),
            mutating(true, false, false),
            None,
        ),
        tool(
            "git_push",
            "Push Git branch",
            "Perform a non-force push through an approved HTTPS/SSH remote. Local/custom transports are rejected.",
            &["git.local", "git.remote"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "remote": {"type": "string", "minLength": 1, "maxLength": 128},
                    "branch": {"type": ["string", "null"], "maxLength": 255}
                }),
                &["sessionId", "remote"],
            ),
            mutating(true, false, true),
            None,
        ),
        tool(
            "command_profiles",
            "List command profiles",
            "List only AtrisBridge-detected fixed build/test/lint/check profiles available for the current workspace session.",
            &["command.execute"],
            false,
            session_only_schema(),
            read_only(false),
            None,
        ),
        tool(
            "command_run",
            "Run command profile",
            "Run one fixed AtrisBridge command profile in the current direct or isolated session as a cancellable durable task with bounded encrypted output.",
            &["command.execute"],
            false,
            schema(
                json!({
                    "sessionId": session_id_schema(),
                    "profileId": {"type": "string", "minLength": 1, "maxLength": 64}
                }),
                &["sessionId", "profileId"],
            ),
            mutating(true, false, true),
            Some(McpToolExecution {
                task_support: "required",
            }),
        ),
    ]
}

fn tool(
    name: &'static str,
    title: &'static str,
    description: &'static str,
    required_capabilities: &[&'static str],
    requires_isolated_worktree: bool,
    input_schema: Value,
    annotations: McpToolAnnotations,
    execution: Option<McpToolExecution>,
) -> McpToolContract {
    McpToolContract {
        name,
        title,
        description,
        required_capabilities: required_capabilities.to_vec(),
        requires_session: !matches!(name, "workspace_list" | "session_open"),
        requires_isolated_worktree,
        input_schema,
        annotations,
        execution,
    }
}

fn read_only(open_world: bool) -> McpToolAnnotations {
    McpToolAnnotations {
        read_only_hint: true,
        destructive_hint: false,
        idempotent_hint: true,
        open_world_hint: open_world,
    }
}

fn mutating(destructive: bool, idempotent: bool, open_world: bool) -> McpToolAnnotations {
    McpToolAnnotations {
        read_only_hint: false,
        destructive_hint: destructive,
        idempotent_hint: idempotent,
        open_world_hint: open_world,
    }
}

fn schema(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": properties,
        "required": required
    })
}

fn capability_schema() -> Value {
    let values = AI_CAPABILITIES
        .iter()
        .map(|value| Value::String((*value).to_string()))
        .collect::<Vec<_>>();
    json!({"type": "string", "enum": values})
}

fn session_id_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 128,
        "description": "Explicit AtrisBridge AI session handle issued by the desktop authority."
    })
}

fn relative_path_schema() -> Value {
    json!({
        "type": "string",
        "minLength": 1,
        "maxLength": 1024,
        "description": "Portable workspace-relative path. Absolute paths and traversal are rejected."
    })
}

fn session_only_schema() -> Value {
    schema(json!({"sessionId": session_id_schema()}), &["sessionId"])
}

fn session_limit_schema() -> Value {
    schema(
        json!({
            "sessionId": session_id_schema(),
            "limit": {"type": "integer", "minimum": 1, "maximum": 200}
        }),
        &["sessionId"],
    )
}

fn session_paths_schema() -> Value {
    schema(
        json!({
            "sessionId": session_id_schema(),
            "paths": {
                "type": "array",
                "minItems": 1,
                "maxItems": 100,
                "uniqueItems": true,
                "items": relative_path_schema()
            }
        }),
        &["sessionId", "paths"],
    )
}

fn id_schema(id_name: &str) -> Value {
    let mut properties = serde_json::Map::new();
    properties.insert("sessionId".to_string(), session_id_schema());
    properties.insert(
        id_name.to_string(),
        json!({"type": "string", "minLength": 1, "maxLength": 128}),
    );
    schema(Value::Object(properties), &["sessionId", id_name])
}

fn change_request_schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "operation": {"type": "string", "enum": ["create", "replace", "delete", "move"]},
            "relativePath": relative_path_schema(),
            "destinationPath": {"type": ["string", "null"], "maxLength": 1024},
            "expectedBeforeHash": {"type": ["string", "null"], "maxLength": 128},
            "content": {"type": ["string", "null"]}
        },
        "required": ["operation", "relativePath"]
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_are_unique_and_compatibly_shaped() {
        let tools = tool_catalog();
        let mut names = HashSet::new();
        for tool in tools {
            assert!(names.insert(tool.name));
            assert!(tool.name.len() <= 64);
            assert!(tool
                .name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'));
        }
    }

    #[test]
    fn bootstrap_tools_use_transport_principal_without_session_handle() {
        for name in ["workspace_list", "session_open"] {
            let tool = tool_catalog()
                .into_iter()
                .find(|tool| tool.name == name)
                .expect("bootstrap tool");
            assert!(!tool.requires_session, "{name}");
            let required = tool.input_schema["required"]
                .as_array()
                .expect("required array");
            assert!(!required.iter().any(|value| value == "sessionId"), "{name}");
            assert!(
                tool.input_schema["properties"].get("clientId").is_none(),
                "{name}"
            );
            assert_eq!(tool.input_schema["additionalProperties"], false);
        }
    }

    #[test]
    fn every_session_bound_tool_requires_explicit_session_handle() {
        for tool in tool_catalog()
            .into_iter()
            .filter(|tool| tool.requires_session)
        {
            let required = tool.input_schema["required"]
                .as_array()
                .expect("required array");
            assert!(
                required.iter().any(|value| value == "sessionId"),
                "{}",
                tool.name
            );
            assert_eq!(
                tool.input_schema["additionalProperties"], false,
                "{}",
                tool.name
            );
        }
    }

    #[test]
    fn session_open_capabilities_match_authority_catalog() {
        let tool = tool_catalog()
            .into_iter()
            .find(|tool| tool.name == "session_open")
            .expect("session_open");
        let values = tool.input_schema["properties"]["requestedCapabilities"]["items"]["enum"]
            .as_array()
            .expect("capability enum");
        let actual = values
            .iter()
            .filter_map(Value::as_str)
            .collect::<HashSet<_>>();
        let expected = AI_CAPABILITIES.iter().copied().collect::<HashSet<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn contracts_match_high_risk_authority_requirements() {
        let tools = tool_catalog();
        let diff = tools
            .iter()
            .find(|tool| tool.name == "git_diff")
            .expect("git_diff");
        assert!(diff.required_capabilities.contains(&"workspace.read"));
        let revert = tools
            .iter()
            .find(|tool| tool.name == "git_revert")
            .expect("git_revert");
        assert!(revert.required_capabilities.contains(&"workspace.edit"));
        let profiles = tools
            .iter()
            .find(|tool| tool.name == "command_profiles")
            .expect("command_profiles");
        assert!(!profiles.annotations.open_world_hint);
        let stage = tools
            .iter()
            .find(|tool| tool.name == "git_stage")
            .expect("git_stage");
        assert!(stage.input_schema["properties"].get("paths").is_some());
        assert!(stage.input_schema["properties"]
            .get("relativePath")
            .is_none());
        let search = tools
            .iter()
            .find(|tool| tool.name == "workspace_search")
            .expect("workspace_search");
        assert!(search.input_schema["properties"]
            .get("includeSensitive")
            .is_some());
        assert!(search.input_schema["properties"]
            .get("pathPrefix")
            .is_none());
    }

    #[test]
    fn command_contract_matches_runtime_authority() {
        for name in ["command_profiles", "command_run"] {
            let tool = tool_catalog()
                .into_iter()
                .find(|tool| tool.name == name)
                .expect("command tool");
            assert_eq!(tool.required_capabilities, vec!["command.execute"]);
            assert!(!tool.requires_isolated_worktree);
        }
    }

    #[test]
    fn id_schema_uses_requested_property_name() {
        let value = id_schema("changesetId");
        assert!(value["properties"].get("changesetId").is_some());
        assert!(value["properties"].get("id_name").is_none());
        assert!(value["required"]
            .as_array()
            .expect("required")
            .iter()
            .any(|entry| entry == "changesetId"));
    }

    #[test]
    fn command_run_uses_mcp_task_extension_contract() {
        let tool = tool_catalog()
            .into_iter()
            .find(|tool| tool.name == "command_run")
            .expect("command_run");
        assert_eq!(
            tool.execution.expect("task execution").task_support,
            "required"
        );
        assert!(tool.annotations.open_world_hint);
        assert!(!tool.annotations.read_only_hint);
    }

    #[test]
    fn read_only_tools_are_explicitly_retry_safe() {
        for tool in tool_catalog()
            .into_iter()
            .filter(|tool| tool.annotations.read_only_hint)
        {
            assert!(tool.annotations.idempotent_hint, "{}", tool.name);
            assert!(!tool.annotations.destructive_hint, "{}", tool.name);
        }
    }

    #[test]
    fn remote_git_and_command_are_open_world() {
        for name in ["git_push", "command_run"] {
            let tool = tool_catalog()
                .into_iter()
                .find(|tool| tool.name == name)
                .expect("tool");
            assert!(tool.annotations.open_world_hint);
        }
    }
}
