use proxima_core::{
    FactPayload, SearchProjection, SearchProjectionColumnKind, SearchProjectionField,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalProposedV1 {
    pub goal_id: uuid::Uuid,
    pub schema_id: String,
    pub title: String,
}

impl FactPayload for GoalProposedV1 {
    const SCHEMA_ID: &'static str = "proxima-goal/goal-proposed-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("Goal proposed: {}", self.title)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_goal.goal_proposed_v1")
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "title",
                kind: SearchProjectionColumnKind::Text,
            }],
            tag_column: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalActivatedV1 {
    pub goal_id: uuid::Uuid,
    pub schema_id: String,
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub accepted_at: OffsetDateTime,
    pub evidence_count: u32,
}

impl FactPayload for GoalActivatedV1 {
    const SCHEMA_ID: &'static str = "proxima-goal/goal-activated-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("Goal activated: {}", self.title)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_goal.goal_activated_v1")
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "title",
                kind: SearchProjectionColumnKind::Text,
            }],
            tag_column: None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GoalAchievedV1 {
    pub goal_id: uuid::Uuid,
    pub schema_id: String,
    pub title: String,
    #[serde(with = "time::serde::rfc3339")]
    pub achieved_at: OffsetDateTime,
    pub evidence_count: u32,
}

impl FactPayload for GoalAchievedV1 {
    const SCHEMA_ID: &'static str = "proxima-goal/goal-achieved-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        format!("Goal achieved: {}", self.title)
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_goal.goal_achieved_v1")
    }

    fn search_projection() -> Option<SearchProjection> {
        Some(SearchProjection {
            fields: &[SearchProjectionField {
                column: "title",
                kind: SearchProjectionColumnKind::Text,
            }],
            tag_column: None,
        })
    }
}
