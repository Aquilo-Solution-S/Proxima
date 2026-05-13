use proxima_core::GoalPayload;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub enum TaskPriority {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct TaskGoalV1 {
    // RFC 3339 explicit so storage projections via Postgres
    // `row_to_json(sidecar)` deserialize cleanly. Without this, time's
    // default human-readable deserializer (`serde-human-readable`) rejects
    // the 'T' separator with "a character literal was not valid". See
    // `WakeTracePayload` for the same fix in the wake-trace path.
    #[serde(with = "time::serde::rfc3339::option")]
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
