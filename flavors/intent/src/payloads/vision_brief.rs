use proxima_core::AbstractionPayload;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum VisionAmbitionLevel {
    Prototype,
    Competent,
    Production,
    Exceptional,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct VisionBriefV1 {
    pub goal_id: uuid::Uuid,
    pub goal_activated_memory_id: uuid::Uuid,
    pub original_goal_text: String,
    pub interpreted_outcome: String,
    pub target_user: String,
    pub use_case: String,
    pub artifact_shape: String,
    pub ambition_level: VisionAmbitionLevel,
    pub quality_bar: String,
    pub constraints: Vec<String>,
    pub assumptions: Vec<String>,
    pub open_questions: Vec<String>,
    pub acceptance_rubric: Vec<String>,
    pub demo_proof: String,
    pub planner_directive: String,
}

impl AbstractionPayload for VisionBriefV1 {
    const SCHEMA_ID: &'static str = "proxima-intent/vision-brief-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_intent.vision_brief_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("VisionBriefV1 schema serializes"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_round_trips_through_json() {
        let payload = VisionBriefV1 {
            goal_id: uuid::Uuid::now_v7(),
            goal_activated_memory_id: uuid::Uuid::now_v7(),
            original_goal_text: "I want a Kanban board".into(),
            interpreted_outcome: "A usable operational board".into(),
            target_user: "Team lead".into(),
            use_case: "Review blocked delivery work".into(),
            artifact_shape: "Static frontend".into(),
            ambition_level: VisionAmbitionLevel::Competent,
            quality_bar: "Scannable and usable without instructions".into(),
            constraints: vec!["No package install".into()],
            assumptions: vec!["User wants product-grade UX".into()],
            open_questions: vec!["Preferred visual density".into()],
            acceptance_rubric: vec!["Browser-visible workflow proof".into()],
            demo_proof: "Executable browser or repo-native tests".into(),
            planner_directive: "Plan from the product outcome, not file names".into(),
        };
        let value = serde_json::to_value(&payload).expect("serialize");
        let back: VisionBriefV1 = serde_json::from_value(value).expect("deserialize");
        assert_eq!(payload, back);
    }

    #[test]
    fn schema_id_is_stable() {
        assert_eq!(VisionBriefV1::SCHEMA_ID, "proxima-intent/vision-brief-v1");
        assert_eq!(VisionBriefV1::SCHEMA_VERSION, 1);
        assert_eq!(
            VisionBriefV1::sidecar_table(),
            "proxima_intent.vision_brief_v1"
        );
    }
}
