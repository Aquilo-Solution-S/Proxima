use crate::GoalPayload;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SimpleTextGoalV1 {}

impl GoalPayload for SimpleTextGoalV1 {
    const SCHEMA_ID: &'static str = "core/simple-text-v1";
    const SCHEMA_VERSION: u32 = 1;
}
