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
    #[schemars(
        description = "`G...` Goal handle in provider-facing typed emit wrappers; stored as the source Goal UUID."
    )]
    pub goal_id: uuid::Uuid,
    #[schemars(
        description = "`F...` goal-activated Fact memory handle in provider-facing typed emit wrappers; stored as the activation memory UUID."
    )]
    pub goal_activated_memory_id: uuid::Uuid,
    #[schemars(description = "Original Goal text the planner interpreted.")]
    pub original_goal_text: String,
    #[schemars(description = "Planner's interpreted outcome for the Goal.")]
    pub interpreted_outcome: String,
    #[schemars(description = "Target user or audience for the desired artifact.")]
    pub target_user: String,
    #[schemars(description = "Concrete use case the artifact should support.")]
    pub use_case: String,
    #[schemars(
        description = "Expected artifact shape, such as frontend app, plan, or repo-native test."
    )]
    pub artifact_shape: String,
    #[schemars(
        description = "Ambition level for the artifact: Prototype, Competent, Production, or Exceptional."
    )]
    pub ambition_level: VisionAmbitionLevel,
    #[schemars(description = "Quality bar the implementation should satisfy.")]
    pub quality_bar: String,
    #[schemars(
        description = "Constraints the implementation must respect. Use `[]` when none are known."
    )]
    pub constraints: Vec<String>,
    #[schemars(description = "Planner assumptions. Use `[]` when none are needed.")]
    pub assumptions: Vec<String>,
    #[schemars(
        description = "Open questions. Use `[]` unless the worker must resolve explicit unknowns."
    )]
    pub open_questions: Vec<String>,
    #[schemars(
        description = "Acceptance rubric items for evaluating the artifact. Use `[]` when no rubric is needed."
    )]
    pub acceptance_rubric: Vec<String>,
    #[schemars(description = "Concrete demo or proof expected after implementation.")]
    pub demo_proof: String,
    #[schemars(description = "Directive for the downstream planner or worker.")]
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
