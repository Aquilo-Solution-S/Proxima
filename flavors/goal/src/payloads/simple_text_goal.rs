use proxima_core::GoalPayload;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct SimpleTextGoalV1 {}

impl GoalPayload for SimpleTextGoalV1 {
    const SCHEMA_ID: &'static str = "proxima-goal/simple-text-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_goal.simple_text_goal_v1"
    }
}
