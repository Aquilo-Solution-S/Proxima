use crate::{GoalPayload, schema_only_key};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SimpleTextGoalV1 {}

impl GoalPayload for SimpleTextGoalV1 {
    const SCHEMA_ID: &'static str = "core/simple-text-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        schema_only_key(Self::SCHEMA_ID, Self::SCHEMA_VERSION)
    }
}
