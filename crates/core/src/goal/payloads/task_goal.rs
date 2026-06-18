use crate::{GoalPayload, PayloadKeyBuilder};

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

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_option_time("due_at", self.due_at);
        key.field_option_str("priority", self.priority.map(TaskPriority::as_str));
        key.finish()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.task_goal_v1")
    }
}
