use proxima_core::GoalPayload;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum TaskPriority {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TaskGoalV1 {
    pub due_at: Option<time::OffsetDateTime>,
    pub priority: Option<TaskPriority>,
}

impl GoalPayload for TaskGoalV1 {
    const SCHEMA_ID: &'static str = "proxima-goal/task-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_goal.task_goal_v1"
    }
}
