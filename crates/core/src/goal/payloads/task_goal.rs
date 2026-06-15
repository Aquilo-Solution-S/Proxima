use futures::future::BoxFuture;

use crate::{GoalPayload, StorageError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, sqlx::Type)]
#[sqlx(type_name = "proxima_core.task_priority")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
}

impl TaskPriority {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "Low",
            Self::Medium => "Medium",
            Self::High => "High",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskGoalV1 {
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_at: Option<time::OffsetDateTime>,
    pub priority: Option<TaskPriority>,
}

impl GoalPayload for TaskGoalV1 {
    const SCHEMA_ID: &'static str = "core/task-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.task_goal_v1")
    }

    fn sidecar_insert<'t>(
        &'t self,
        tx: &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
        goal_id: uuid::Uuid,
    ) -> BoxFuture<'t, Result<(), StorageError>> {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO proxima_core.task_goal_v1 (goal_id, due_at, priority)
                 VALUES ($1, $2, $3::proxima_core.task_priority)",
            )
            .bind(goal_id)
            .bind(self.due_at)
            .bind(self.priority.map(TaskPriority::as_str))
            .execute(&mut **tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}
