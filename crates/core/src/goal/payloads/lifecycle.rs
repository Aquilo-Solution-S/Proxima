use crate::FactPayload;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalActivatedV1 {
    pub goal_id: uuid::Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub transitioned_at: OffsetDateTime,
}

impl FactPayload for GoalActivatedV1 {
    const SCHEMA_ID: &'static str = "core/goal-activated-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("Goal activated: {}", self.goal_id)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.goal_activated_v1")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalPausedV1 {
    pub goal_id: uuid::Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub transitioned_at: OffsetDateTime,
}

impl FactPayload for GoalPausedV1 {
    const SCHEMA_ID: &'static str = "core/goal-paused-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("Goal paused: {}", self.goal_id)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.goal_paused_v1")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalAchievedV1 {
    pub goal_id: uuid::Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub transitioned_at: OffsetDateTime,
}

impl FactPayload for GoalAchievedV1 {
    const SCHEMA_ID: &'static str = "core/goal-achieved-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("Goal achieved: {}", self.goal_id)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.goal_achieved_v1")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalAbandonedV1 {
    pub goal_id: uuid::Uuid,
    #[serde(with = "time::serde::rfc3339")]
    pub transitioned_at: OffsetDateTime,
}

impl FactPayload for GoalAbandonedV1 {
    const SCHEMA_ID: &'static str = "core/goal-abandoned-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("Goal abandoned: {}", self.goal_id)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_core.goal_abandoned_v1")
    }
}
