use std::collections::HashSet;

use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use tauri::AppHandle;
use uuid::Uuid;

use crate::{database::open_database, storage::find_workspace};

const MAX_CLIENT_ID_LEN: usize = 128;
const MIN_SESSION_TTL_MINUTES: u64 = 5;
const MAX_SESSION_TTL_MINUTES: u64 = 24 * 60;
const DEFAULT_AUDIT_LIMIT: u32 = 100;
const MAX_AUDIT_LIMIT: u32 = 500;

pub const AI_CAPABILITIES: [&str; 11] = [
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
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiPermissionRule {
    Deny,
    Ask,
    Allow,
}

impl AiPermissionRule {
    fn as_str(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Ask => "ask",
            Self::Allow => "allow",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "deny" => Ok(Self::Deny),
            "ask" => Ok(Self::Ask),
            "allow" => Ok(Self::Allow),
            _ => Err("Stored AI permission rule is invalid.".into()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiSessionMode {
    Direct,
    IsolatedWorktree,
}

impl AiSessionMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::IsolatedWorktree => "isolated_worktree",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiPermissionRecord {
    pub workspace_id: String,
    pub client_id: String,
    pub capability: String,
    pub rule: AiPermissionRule,
    pub explicit: bool,
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiSession {
    pub id: String,
    pub client_id: String,
    pub workspace_id: String,
    pub mode: String,
    pub status: String,
    pub created_at: String,
    pub last_activity_at: String,
    pub expires_at: String,
    pub closed_at: Option<String>,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiGatewayOverview {
    pub capabilities: Vec<String>,
    pub active_sessions: u64,
    pub permission_model: &'static str,
    pub audit_content_policy: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiAuditEntry {
    pub id: String,
    pub session_id: Option<String>,
    pub client_id: String,
    pub workspace_id: String,
    pub capability: Option<String>,
    pub tool_name: String,
    pub outcome: String,
    pub duration_ms: Option<u64>,
    pub operation_id: Option<String>,
    pub detail_code: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct AiAuditEvent<'a> {
    pub session_id: Option<&'a str>,
    pub client_id: &'a str,
    pub workspace_id: &'a str,
    pub capability: Option<&'a str>,
    pub tool_name: &'a str,
    pub outcome: &'a str,
    pub duration_ms: Option<u64>,
    pub operation_id: Option<&'a str>,
    pub detail_code: Option<&'a str>,
}

pub fn initialize(app: &AppHandle) -> Result<(), String> {
    let connection = open_ai_database(app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE ai_sessions
             SET status = 'interrupted', closed_at = ?1, last_activity_at = ?1
             WHERE status = 'active'",
            params![now],
        )
        .map_err(|error| format!("Could not close interrupted AI sessions: {error}"))?;
    Ok(())
}

#[tauri::command]
pub fn ai_gateway_overview(app: AppHandle) -> Result<AiGatewayOverview, String> {
    let connection = open_ai_database(&app)?;
    expire_sessions(&connection)?;
    let active_sessions = count_active_sessions(&connection)?;
    Ok(AiGatewayOverview {
        capabilities: AI_CAPABILITIES
            .iter()
            .map(|value| (*value).to_string())
            .collect(),
        active_sessions,
        permission_model: "default_ask_explicit_workspace_client_rules",
        audit_content_policy: "metadata_only_no_prompts_credentials_or_file_content",
    })
}

#[tauri::command]
pub fn list_ai_permissions(
    app: AppHandle,
    workspace_id: String,
    client_id: String,
) -> Result<Vec<AiPermissionRecord>, String> {
    find_workspace(&app, &workspace_id)?;
    validate_client_id(&client_id)?;
    let connection = open_ai_database(&app)?;
    AI_CAPABILITIES
        .iter()
        .map(|capability| permission_record(&connection, &workspace_id, &client_id, capability))
        .collect()
}

#[tauri::command]
pub fn set_ai_permission(
    app: AppHandle,
    workspace_id: String,
    client_id: String,
    capability: String,
    rule: AiPermissionRule,
) -> Result<AiPermissionRecord, String> {
    find_workspace(&app, &workspace_id)?;
    validate_client_id(&client_id)?;
    validate_capability(&capability)?;
    let connection = open_ai_database(&app)?;
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "INSERT INTO ai_permission_rules (
                workspace_id, client_id, capability, rule, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(workspace_id, client_id, capability) DO UPDATE SET
                rule = excluded.rule,
                updated_at = excluded.updated_at",
            params![workspace_id, client_id, capability, rule.as_str(), now],
        )
        .map_err(|error| format!("Could not save AI permission rule: {error}"))?;

    record_audit_with_connection(
        &connection,
        AiAuditEvent {
            session_id: None,
            client_id: &client_id,
            workspace_id: &workspace_id,
            capability: Some(&capability),
            tool_name: "permission.set",
            outcome: "success",
            duration_ms: None,
            operation_id: None,
            detail_code: Some(rule.as_str()),
        },
    )?;

    permission_record(&connection, &workspace_id, &client_id, &capability)
}

#[tauri::command]
pub fn reset_ai_permission(
    app: AppHandle,
    workspace_id: String,
    client_id: String,
    capability: String,
) -> Result<AiPermissionRecord, String> {
    find_workspace(&app, &workspace_id)?;
    validate_client_id(&client_id)?;
    validate_capability(&capability)?;
    let connection = open_ai_database(&app)?;
    connection
        .execute(
            "DELETE FROM ai_permission_rules
             WHERE workspace_id = ?1 AND client_id = ?2 AND capability = ?3",
            params![workspace_id, client_id, capability],
        )
        .map_err(|error| format!("Could not reset AI permission rule: {error}"))?;

    record_audit_with_connection(
        &connection,
        AiAuditEvent {
            session_id: None,
            client_id: &client_id,
            workspace_id: &workspace_id,
            capability: Some(&capability),
            tool_name: "permission.reset",
            outcome: "success",
            duration_ms: None,
            operation_id: None,
            detail_code: Some("default_ask"),
        },
    )?;

    permission_record(&connection, &workspace_id, &client_id, &capability)
}

#[tauri::command]
pub fn open_ai_session(
    app: AppHandle,
    workspace_id: String,
    client_id: String,
    mode: AiSessionMode,
    requested_capabilities: Vec<String>,
    approved_session_capabilities: Vec<String>,
    ttl_minutes: u64,
) -> Result<AiSession, String> {
    find_workspace(&app, &workspace_id)?;
    validate_client_id(&client_id)?;
    validate_session_ttl(ttl_minutes)?;
    let requested = normalize_capabilities(requested_capabilities)?;
    if requested.is_empty() {
        return Err("An AI session must request at least one capability.".into());
    }
    if mode == AiSessionMode::IsolatedWorktree
        && !requested.iter().any(|capability| capability == "git.local")
    {
        return Err("Isolated-worktree AI sessions must request the git.local capability.".into());
    }
    let approved = normalize_capabilities(approved_session_capabilities)?;
    if approved.iter().any(|value| !requested.contains(value)) {
        return Err("Session approval contains a capability that was not requested.".into());
    }
    let approved = approved.into_iter().collect::<HashSet<_>>();

    let connection = open_ai_database(&app)?;
    expire_sessions(&connection)?;

    let mut grants = Vec::<(String, &'static str)>::new();
    for capability in &requested {
        let rule = effective_rule(&connection, &workspace_id, &client_id, capability)?;
        match rule {
            AiPermissionRule::Deny => {
                return Err(format!(
                    "AI capability '{capability}' is denied for this client and workspace."
                ));
            }
            AiPermissionRule::Ask if !approved.contains(capability) => {
                return Err(format!(
                    "AI capability '{capability}' requires explicit session approval."
                ));
            }
            AiPermissionRule::Ask => grants.push((capability.clone(), "session_approval")),
            AiPermissionRule::Allow => grants.push((capability.clone(), "persistent_rule")),
        }
    }

    let id = Uuid::new_v4().to_string();
    let now = Utc::now();
    let created_at = now.to_rfc3339();
    let expires_at = (now + ChronoDuration::minutes(ttl_minutes as i64)).to_rfc3339();
    let transaction = connection
        .unchecked_transaction()
        .map_err(|error| format!("Could not start AI session transaction: {error}"))?;
    transaction
        .execute(
            "INSERT INTO ai_sessions (
                id, client_id, workspace_id, mode, status,
                created_at, last_activity_at, expires_at
             ) VALUES (?1, ?2, ?3, ?4, 'active', ?5, ?5, ?6)",
            params![
                id,
                client_id,
                workspace_id,
                mode.as_str(),
                created_at,
                expires_at
            ],
        )
        .map_err(|error| format!("Could not create AI session: {error}"))?;
    for (capability, source) in &grants {
        transaction
            .execute(
                "INSERT INTO ai_session_capabilities (session_id, capability, source)
                 VALUES (?1, ?2, ?3)",
                params![id, capability, source],
            )
            .map_err(|error| format!("Could not grant AI session capability: {error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("Could not commit AI session: {error}"))?;

    let connection = open_ai_database(&app)?;
    record_audit_with_connection(
        &connection,
        AiAuditEvent {
            session_id: Some(&id),
            client_id: &client_id,
            workspace_id: &workspace_id,
            capability: None,
            tool_name: "session.open",
            outcome: "success",
            duration_ms: None,
            operation_id: None,
            detail_code: Some(mode.as_str()),
        },
    )?;
    load_session(&connection, &id)?.ok_or_else(|| "AI session was not found after creation.".into())
}

#[tauri::command]
pub fn close_ai_session(app: AppHandle, session_id: String) -> Result<AiSession, String> {
    let connection = open_ai_database(&app)?;
    expire_sessions(&connection)?;
    let existing = load_session(&connection, &session_id)?
        .ok_or_else(|| "AI session was not found.".to_string())?;
    if existing.status == "active" {
        let now = Utc::now().to_rfc3339();
        connection
            .execute(
                "UPDATE ai_sessions
                 SET status = 'closed', closed_at = ?1, last_activity_at = ?1
                 WHERE id = ?2 AND status = 'active'",
                params![now, session_id],
            )
            .map_err(|error| format!("Could not close AI session: {error}"))?;
    }
    let session = load_session(&connection, &session_id)?
        .ok_or_else(|| "AI session disappeared while closing.".to_string())?;
    record_audit_with_connection(
        &connection,
        AiAuditEvent {
            session_id: Some(&session.id),
            client_id: &session.client_id,
            workspace_id: &session.workspace_id,
            capability: None,
            tool_name: "session.close",
            outcome: "success",
            duration_ms: None,
            operation_id: None,
            detail_code: Some(&session.status),
        },
    )?;
    Ok(session)
}

#[tauri::command]
pub fn list_ai_sessions(
    app: AppHandle,
    workspace_id: Option<String>,
) -> Result<Vec<AiSession>, String> {
    if let Some(workspace_id) = workspace_id.as_deref() {
        find_workspace(&app, workspace_id)?;
    }
    let connection = open_ai_database(&app)?;
    expire_sessions(&connection)?;
    load_sessions(&connection, workspace_id.as_deref())
}

#[tauri::command]
pub fn list_ai_audit(
    app: AppHandle,
    workspace_id: String,
    limit: Option<u32>,
) -> Result<Vec<AiAuditEntry>, String> {
    find_workspace(&app, &workspace_id)?;
    let limit = limit
        .unwrap_or(DEFAULT_AUDIT_LIMIT)
        .clamp(1, MAX_AUDIT_LIMIT);
    let connection = open_ai_database(&app)?;
    let mut statement = connection
        .prepare(
            "SELECT id, session_id, client_id, workspace_id, capability, tool_name,
                    outcome, duration_ms, operation_id, detail_code, created_at
             FROM ai_tool_audit
             WHERE workspace_id = ?1
             ORDER BY created_at DESC, rowid DESC
             LIMIT ?2",
        )
        .map_err(|error| format!("Could not prepare AI audit query: {error}"))?;
    let rows = statement
        .query_map(params![workspace_id, i64::from(limit)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, String>(10)?,
            ))
        })
        .map_err(|error| format!("Could not query AI audit entries: {error}"))?;
    rows.map(|row| {
        let row = row.map_err(|error| format!("Could not read AI audit entry: {error}"))?;
        Ok(AiAuditEntry {
            id: row.0,
            session_id: row.1,
            client_id: row.2,
            workspace_id: row.3,
            capability: row.4,
            tool_name: row.5,
            outcome: row.6,
            duration_ms: row.7.map(|value| u64::try_from(value).unwrap_or(0)),
            operation_id: row.8,
            detail_code: row.9,
            created_at: row.10,
        })
    })
    .collect()
}

pub fn authorize_session(
    app: &AppHandle,
    session_id: &str,
    capability: &str,
) -> Result<AiSession, String> {
    validate_capability(capability)?;
    let connection = open_ai_database(app)?;
    expire_sessions(&connection)?;
    let mut session = load_session(&connection, session_id)?
        .ok_or_else(|| "AI session was not found.".to_string())?;
    if session.status != "active" {
        return Err(format!(
            "AI session is not active (status: {}).",
            session.status
        ));
    }
    if !session.capabilities.iter().any(|value| value == capability) {
        if !grant_persistently_allowed_capability(&connection, &session, capability)? {
            return Err(format!(
                "AI session is not authorized for capability '{capability}'."
            ));
        }
        session.capabilities = load_session_capabilities(&connection, session_id)?;
        record_audit_with_connection(
            &connection,
            AiAuditEvent {
                session_id: Some(session_id),
                client_id: &session.client_id,
                workspace_id: &session.workspace_id,
                capability: Some(capability),
                tool_name: "session.capability_refresh",
                outcome: "success",
                duration_ms: None,
                operation_id: None,
                detail_code: Some("persistent_rule"),
            },
        )?;
    }
    connection
        .execute(
            "UPDATE ai_sessions SET last_activity_at = ?1 WHERE id = ?2 AND status = 'active'",
            params![Utc::now().to_rfc3339(), session_id],
        )
        .map_err(|error| format!("Could not refresh AI session activity: {error}"))?;
    Ok(session)
}

pub fn record_audit(app: &AppHandle, event: AiAuditEvent<'_>) -> Result<(), String> {
    validate_client_id(event.client_id)?;
    if let Some(capability) = event.capability {
        validate_capability(capability)?;
    }
    let connection = open_ai_database(app)?;
    record_audit_with_connection(&connection, event)
}

fn open_ai_database(app: &AppHandle) -> Result<Connection, String> {
    let connection = open_database(app)?;
    ensure_schema(&connection)?;
    Ok(connection)
}

fn ensure_schema(connection: &Connection) -> Result<(), String> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS ai_permission_rules (
                workspace_id TEXT NOT NULL,
                client_id TEXT NOT NULL,
                capability TEXT NOT NULL,
                rule TEXT NOT NULL CHECK(rule IN ('deny', 'ask', 'allow')),
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                PRIMARY KEY(workspace_id, client_id, capability),
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_ai_permission_rules_client_workspace
                ON ai_permission_rules(client_id, workspace_id);

            CREATE TABLE IF NOT EXISTS ai_sessions (
                id TEXT PRIMARY KEY,
                client_id TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                mode TEXT NOT NULL CHECK(mode IN ('direct', 'isolated_worktree')),
                status TEXT NOT NULL CHECK(status IN (
                    'active', 'closed', 'expired', 'revoked', 'interrupted'
                )),
                created_at TEXT NOT NULL,
                last_activity_at TEXT NOT NULL,
                expires_at TEXT NOT NULL,
                closed_at TEXT,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_ai_sessions_workspace_status
                ON ai_sessions(workspace_id, status, expires_at);
            CREATE INDEX IF NOT EXISTS idx_ai_sessions_client_status
                ON ai_sessions(client_id, status, expires_at);

            CREATE TABLE IF NOT EXISTS ai_session_capabilities (
                session_id TEXT NOT NULL,
                capability TEXT NOT NULL,
                source TEXT NOT NULL CHECK(source IN ('persistent_rule', 'session_approval')),
                PRIMARY KEY(session_id, capability),
                FOREIGN KEY(session_id) REFERENCES ai_sessions(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS ai_tool_audit (
                id TEXT PRIMARY KEY,
                session_id TEXT,
                client_id TEXT NOT NULL,
                workspace_id TEXT NOT NULL,
                capability TEXT,
                tool_name TEXT NOT NULL,
                outcome TEXT NOT NULL CHECK(outcome IN ('success', 'denied', 'failed', 'cancelled')),
                duration_ms INTEGER CHECK(duration_ms IS NULL OR duration_ms >= 0),
                operation_id TEXT,
                detail_code TEXT,
                created_at TEXT NOT NULL,
                FOREIGN KEY(session_id) REFERENCES ai_sessions(id) ON DELETE SET NULL,
                FOREIGN KEY(workspace_id) REFERENCES workspaces(id) ON DELETE CASCADE
            );

            CREATE INDEX IF NOT EXISTS idx_ai_tool_audit_workspace_created
                ON ai_tool_audit(workspace_id, created_at DESC);
            CREATE INDEX IF NOT EXISTS idx_ai_tool_audit_session_created
                ON ai_tool_audit(session_id, created_at DESC);",
        )
        .map_err(|error| format!("Could not initialize AI gateway metadata: {error}"))
}

fn permission_record(
    connection: &Connection,
    workspace_id: &str,
    client_id: &str,
    capability: &str,
) -> Result<AiPermissionRecord, String> {
    let stored = connection
        .query_row(
            "SELECT rule, updated_at FROM ai_permission_rules
             WHERE workspace_id = ?1 AND client_id = ?2 AND capability = ?3",
            params![workspace_id, client_id, capability],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| format!("Could not read AI permission rule: {error}"))?;
    let (rule, explicit, updated_at) = match stored {
        Some((rule, updated_at)) => (AiPermissionRule::parse(&rule)?, true, Some(updated_at)),
        None => (AiPermissionRule::Ask, false, None),
    };
    Ok(AiPermissionRecord {
        workspace_id: workspace_id.to_string(),
        client_id: client_id.to_string(),
        capability: capability.to_string(),
        rule,
        explicit,
        updated_at,
    })
}

fn effective_rule(
    connection: &Connection,
    workspace_id: &str,
    client_id: &str,
    capability: &str,
) -> Result<AiPermissionRule, String> {
    Ok(permission_record(connection, workspace_id, client_id, capability)?.rule)
}

fn grant_persistently_allowed_capability(
    connection: &Connection,
    session: &AiSession,
    capability: &str,
) -> Result<bool, String> {
    let inserted = connection
        .execute(
            "INSERT INTO ai_session_capabilities (session_id, capability, source)
             SELECT ?1, ?2, 'persistent_rule'
             WHERE EXISTS (
                 SELECT 1 FROM ai_permission_rules
                 WHERE workspace_id = ?3
                   AND client_id = ?4
                   AND capability = ?2
                   AND rule = 'allow'
             )
             ON CONFLICT(session_id, capability) DO NOTHING",
            params![
                session.id,
                capability,
                session.workspace_id,
                session.client_id
            ],
        )
        .map_err(|error| format!("Could not refresh AI session capability: {error}"))?;
    Ok(inserted > 0)
}

fn load_session(connection: &Connection, session_id: &str) -> Result<Option<AiSession>, String> {
    let row = connection
        .query_row(
            "SELECT id, client_id, workspace_id, mode, status, created_at,
                    last_activity_at, expires_at, closed_at
             FROM ai_sessions WHERE id = ?1",
            params![session_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()
        .map_err(|error| format!("Could not read AI session: {error}"))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let capabilities = load_session_capabilities(connection, &row.0)?;
    Ok(Some(AiSession {
        id: row.0,
        client_id: row.1,
        workspace_id: row.2,
        mode: row.3,
        status: row.4,
        created_at: row.5,
        last_activity_at: row.6,
        expires_at: row.7,
        closed_at: row.8,
        capabilities,
    }))
}

fn load_sessions(
    connection: &Connection,
    workspace_id: Option<&str>,
) -> Result<Vec<AiSession>, String> {
    let sql = if workspace_id.is_some() {
        "SELECT id FROM ai_sessions WHERE workspace_id = ?1 ORDER BY created_at DESC"
    } else {
        "SELECT id FROM ai_sessions WHERE ?1 IS NULL ORDER BY created_at DESC"
    };
    let mut statement = connection
        .prepare(sql)
        .map_err(|error| format!("Could not prepare AI session query: {error}"))?;
    let rows = statement
        .query_map(params![workspace_id], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Could not query AI sessions: {error}"))?;
    let ids = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read AI session IDs: {error}"))?;
    ids.into_iter()
        .map(|id| {
            load_session(connection, &id)?
                .ok_or_else(|| "AI session disappeared while listing sessions.".to_string())
        })
        .collect()
}

fn load_session_capabilities(
    connection: &Connection,
    session_id: &str,
) -> Result<Vec<String>, String> {
    let mut statement = connection
        .prepare(
            "SELECT capability FROM ai_session_capabilities
             WHERE session_id = ?1 ORDER BY capability ASC",
        )
        .map_err(|error| format!("Could not prepare AI session capability query: {error}"))?;
    let rows = statement
        .query_map(params![session_id], |row| row.get::<_, String>(0))
        .map_err(|error| format!("Could not query AI session capabilities: {error}"))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|error| format!("Could not read AI session capabilities: {error}"))
}

fn expire_sessions(connection: &Connection) -> Result<(), String> {
    let now = Utc::now().to_rfc3339();
    connection
        .execute(
            "UPDATE ai_sessions
             SET status = 'expired', closed_at = ?1, last_activity_at = ?1
             WHERE status = 'active' AND expires_at <= ?1",
            params![now],
        )
        .map_err(|error| format!("Could not expire stale AI sessions: {error}"))?;
    Ok(())
}

fn count_active_sessions(connection: &Connection) -> Result<u64, String> {
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM ai_sessions WHERE status = 'active'",
            [],
            |row| row.get(0),
        )
        .map_err(|error| format!("Could not count active AI sessions: {error}"))?;
    u64::try_from(count).map_err(|_| "Stored AI session count is invalid.".into())
}

fn record_audit_with_connection(
    connection: &Connection,
    event: AiAuditEvent<'_>,
) -> Result<(), String> {
    if !matches!(event.outcome, "success" | "denied" | "failed" | "cancelled") {
        return Err("AI audit outcome is invalid.".into());
    }
    let duration_ms = event
        .duration_ms
        .map(|value| i64::try_from(value).map_err(|_| "AI audit duration exceeds SQLite range."))
        .transpose()?;
    connection
        .execute(
            "INSERT INTO ai_tool_audit (
                id, session_id, client_id, workspace_id, capability, tool_name,
                outcome, duration_ms, operation_id, detail_code, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                Uuid::new_v4().to_string(),
                event.session_id,
                event.client_id,
                event.workspace_id,
                event.capability,
                event.tool_name,
                event.outcome,
                duration_ms,
                event.operation_id,
                event.detail_code,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|error| format!("Could not record AI audit event: {error}"))?;
    Ok(())
}

fn validate_client_id(client_id: &str) -> Result<(), String> {
    let trimmed = client_id.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_CLIENT_ID_LEN {
        return Err(format!(
            "AI client ID must be between 1 and {MAX_CLIENT_ID_LEN} characters."
        ));
    }
    if trimmed != client_id {
        return Err("AI client ID must not contain leading or trailing whitespace.".into());
    }
    Ok(())
}

fn validate_capability(capability: &str) -> Result<(), String> {
    if AI_CAPABILITIES.contains(&capability) {
        Ok(())
    } else {
        Err(format!("Unsupported AI capability '{capability}'."))
    }
}

fn normalize_capabilities(values: Vec<String>) -> Result<Vec<String>, String> {
    let mut normalized = Vec::new();
    let mut seen = HashSet::new();
    for value in values {
        validate_capability(&value)?;
        if seen.insert(value.clone()) {
            normalized.push(value);
        }
    }
    normalized.sort();
    Ok(normalized)
}

fn validate_session_ttl(ttl_minutes: u64) -> Result<(), String> {
    if !(MIN_SESSION_TTL_MINUTES..=MAX_SESSION_TTL_MINUTES).contains(&ttl_minutes) {
        return Err(format!(
            "AI session lifetime must be between {MIN_SESSION_TTL_MINUTES} and {MAX_SESSION_TTL_MINUTES} minutes."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_database() -> Connection {
        let connection = Connection::open_in_memory().expect("database");
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 CREATE TABLE workspaces (id TEXT PRIMARY KEY);
                 INSERT INTO workspaces (id) VALUES ('ws-1');",
            )
            .expect("workspace schema");
        ensure_schema(&connection).expect("AI schema");
        connection
    }

    fn insert_active_session(connection: &Connection, session_id: &str) -> AiSession {
        let now = Utc::now();
        let created_at = now.to_rfc3339();
        let expires_at = (now + ChronoDuration::minutes(60)).to_rfc3339();
        connection
            .execute(
                "INSERT INTO ai_sessions (
                    id, client_id, workspace_id, mode, status,
                    created_at, last_activity_at, expires_at
                 ) VALUES (?1, 'chatgpt', 'ws-1', 'direct', 'active', ?2, ?2, ?3)",
                params![session_id, created_at, expires_at],
            )
            .expect("session");
        load_session(connection, session_id)
            .expect("load session")
            .expect("session exists")
    }

    #[test]
    fn unknown_permissions_default_to_ask() {
        let connection = test_database();
        let record = permission_record(&connection, "ws-1", "chatgpt", "workspace.read")
            .expect("permission");
        assert_eq!(record.rule, AiPermissionRule::Ask);
        assert!(!record.explicit);
    }

    #[test]
    fn persistent_allow_can_refresh_an_existing_session_capability() {
        let connection = test_database();
        let session = insert_active_session(&connection, "session-allow");
        connection
            .execute(
                "INSERT INTO ai_permission_rules (
                    workspace_id, client_id, capability, rule, created_at, updated_at
                 ) VALUES ('ws-1', 'chatgpt', 'command.execute', 'allow', 'now', 'now')",
                [],
            )
            .expect("permission");

        assert!(
            grant_persistently_allowed_capability(&connection, &session, "command.execute")
                .expect("refresh")
        );
        assert!(load_session_capabilities(&connection, &session.id)
            .expect("capabilities")
            .iter()
            .any(|capability| capability == "command.execute"));
    }

    #[test]
    fn default_ask_does_not_refresh_an_existing_session_capability() {
        let connection = test_database();
        let session = insert_active_session(&connection, "session-ask");

        assert!(
            !grant_persistently_allowed_capability(&connection, &session, "command.execute")
                .expect("refresh")
        );
        assert!(!load_session_capabilities(&connection, &session.id)
            .expect("capabilities")
            .iter()
            .any(|capability| capability == "command.execute"));
    }

    #[test]
    fn capability_allowlist_rejects_generic_shell_authority() {
        assert!(validate_capability("workspace.read").is_ok());
        assert!(validate_capability("shell.execute").is_err());
        assert!(validate_capability("filesystem.any_path").is_err());
    }

    #[test]
    fn duplicate_capabilities_are_normalized_deterministically() {
        let values = normalize_capabilities(vec![
            "sync.read".into(),
            "workspace.read".into(),
            "sync.read".into(),
        ])
        .expect("capabilities");
        assert_eq!(values, vec!["sync.read", "workspace.read"]);
    }

    #[test]
    fn schema_rejects_invalid_permission_rule() {
        let connection = test_database();
        let result = connection.execute(
            "INSERT INTO ai_permission_rules (
                workspace_id, client_id, capability, rule, created_at, updated_at
             ) VALUES ('ws-1', 'client', 'workspace.read', 'wildcard', 'now', 'now')",
            [],
        );
        assert!(result.is_err());
    }

    #[test]
    fn session_ttl_is_bounded() {
        assert!(validate_session_ttl(4).is_err());
        assert!(validate_session_ttl(5).is_ok());
        assert!(validate_session_ttl(1440).is_ok());
        assert!(validate_session_ttl(1441).is_err());
    }

    #[test]
    fn audit_schema_does_not_store_prompt_or_file_content_columns() {
        let connection = test_database();
        let mut statement = connection
            .prepare("SELECT name FROM pragma_table_info('ai_tool_audit')")
            .expect("table info");
        let columns = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("columns")
            .collect::<rusqlite::Result<Vec<_>>>()
            .expect("column names");
        assert!(!columns.iter().any(|value| value == "prompt"));
        assert!(!columns.iter().any(|value| value == "file_content"));
        assert!(!columns.iter().any(|value| value == "credential"));
    }
}
