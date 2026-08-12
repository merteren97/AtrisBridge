use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
};

use chrono::Utc;
use serde::Serialize;
use tauri::State;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceOperationKind {
    Observe,
    Configure,
    Plan,
    Execute,
    Recovery,
    Continuous,
    AiEdit,
    Git,
    Command,
}

impl WorkspaceOperationKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observe => "observe",
            Self::Configure => "configure",
            Self::Plan => "plan",
            Self::Execute => "execute",
            Self::Recovery => "recovery",
            Self::Continuous => "continuous",
            Self::AiEdit => "ai_edit",
            Self::Git => "git",
            Self::Command => "command",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceOperationStatus {
    pub workspace_id: String,
    pub operation_id: String,
    pub owner: String,
    pub kind: String,
    pub started_at: String,
}

#[derive(Debug, Clone)]
struct ActiveOperation {
    operation_id: String,
    owner: String,
    kind: WorkspaceOperationKind,
    started_at: String,
}

#[derive(Clone, Default)]
pub struct WorkspaceMutationCoordinator {
    inner: Arc<Mutex<HashMap<String, ActiveOperation>>>,
}

#[derive(Debug, Clone)]
pub enum WorkspaceLeaseError {
    Busy(WorkspaceOperationStatus),
    Unavailable,
}

impl fmt::Display for WorkspaceLeaseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(active) => write!(
                formatter,
                "Workspace is busy with '{}' owned by '{}'. Wait for operation {} to finish and retry.",
                active.kind, active.owner, active.operation_id
            ),
            Self::Unavailable => write!(formatter, "Workspace operation coordinator is unavailable."),
        }
    }
}

pub struct WorkspaceMutationLease {
    coordinator: WorkspaceMutationCoordinator,
    workspace_id: String,
    operation_id: String,
}

impl WorkspaceMutationCoordinator {
    pub fn acquire(
        &self,
        workspace_id: &str,
        owner: &str,
        kind: WorkspaceOperationKind,
    ) -> Result<WorkspaceMutationLease, WorkspaceLeaseError> {
        let mut active = self
            .inner
            .lock()
            .map_err(|_| WorkspaceLeaseError::Unavailable)?;

        if let Some(existing) = active.get(workspace_id) {
            return Err(WorkspaceLeaseError::Busy(status_for(
                workspace_id,
                existing,
            )));
        }

        let operation_id = Uuid::new_v4().to_string();
        active.insert(
            workspace_id.to_string(),
            ActiveOperation {
                operation_id: operation_id.clone(),
                owner: owner.to_string(),
                kind,
                started_at: Utc::now().to_rfc3339(),
            },
        );

        Ok(WorkspaceMutationLease {
            coordinator: self.clone(),
            workspace_id: workspace_id.to_string(),
            operation_id,
        })
    }

    pub fn status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<WorkspaceOperationStatus>, WorkspaceLeaseError> {
        let active = self
            .inner
            .lock()
            .map_err(|_| WorkspaceLeaseError::Unavailable)?;
        Ok(active
            .get(workspace_id)
            .map(|operation| status_for(workspace_id, operation)))
    }
}

impl Drop for WorkspaceMutationLease {
    fn drop(&mut self) {
        let Ok(mut active) = self.coordinator.inner.lock() else {
            return;
        };
        let should_remove = active
            .get(&self.workspace_id)
            .map(|operation| operation.operation_id == self.operation_id)
            .unwrap_or(false);
        if should_remove {
            active.remove(&self.workspace_id);
        }
    }
}

fn status_for(workspace_id: &str, operation: &ActiveOperation) -> WorkspaceOperationStatus {
    WorkspaceOperationStatus {
        workspace_id: workspace_id.to_string(),
        operation_id: operation.operation_id.clone(),
        owner: operation.owner.clone(),
        kind: operation.kind.as_str().to_string(),
        started_at: operation.started_at.clone(),
    }
}

#[tauri::command]
pub fn workspace_operation_status(
    id: String,
    coordinator: State<'_, WorkspaceMutationCoordinator>,
) -> Result<Option<WorkspaceOperationStatus>, String> {
    coordinator.status(&id).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_mutation_lease_can_own_a_workspace() {
        let coordinator = WorkspaceMutationCoordinator::default();
        let first = coordinator
            .acquire("ws-1", "desktop", WorkspaceOperationKind::Execute)
            .expect("first lease");

        let second =
            coordinator.acquire("ws-1", "continuous", WorkspaceOperationKind::Continuous);
        assert!(matches!(second, Err(WorkspaceLeaseError::Busy(_))));

        drop(first);
        coordinator
            .acquire("ws-1", "continuous", WorkspaceOperationKind::Continuous)
            .expect("lease after release");
    }

    #[test]
    fn different_workspaces_can_run_independently() {
        let coordinator = WorkspaceMutationCoordinator::default();
        let _first = coordinator
            .acquire("ws-1", "desktop", WorkspaceOperationKind::Execute)
            .expect("first workspace lease");
        let _second = coordinator
            .acquire("ws-2", "continuous", WorkspaceOperationKind::Continuous)
            .expect("second workspace lease");

        assert!(coordinator.status("ws-1").expect("status").is_some());
        assert!(coordinator.status("ws-2").expect("status").is_some());
    }

    #[test]
    fn dropping_a_stale_lease_cannot_release_a_newer_owner() {
        let coordinator = WorkspaceMutationCoordinator::default();
        let first = coordinator
            .acquire("ws-1", "desktop", WorkspaceOperationKind::Execute)
            .expect("first lease");
        let first_id = first.operation_id.clone();
        drop(first);

        let second = coordinator
            .acquire("ws-1", "ai", WorkspaceOperationKind::AiEdit)
            .expect("second lease");
        let second_id = second.operation_id.clone();
        assert_ne!(first_id, second_id);
        assert_eq!(
            coordinator
                .status("ws-1")
                .expect("status")
                .expect("active")
                .operation_id,
            second_id
        );
    }
}
