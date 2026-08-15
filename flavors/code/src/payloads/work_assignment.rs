use proxima_core::{EntityKind, MemoryId, PayloadReference, PerspectivePayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// "This worker should pick up that request."
///
/// Endpoints are a worker Perspective and a request Fact. A Fact asserts
/// no judgment; the target Perspective is append-only. Neither endpoint
/// owns the claim (docs/16 §The Thesis): this Perspective references both,
/// so the two `reference` index rows are re-derivable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeWorkAssignmentV1 {
    #[schemars(description = "Repo the assigned work item belongs to.")]
    pub repo_id: uuid::Uuid,
    #[schemars(
        description = "Memory id of the worker Perspective that should receive the work item."
    )]
    pub target_perspective_memory_id: uuid::Uuid,
    #[schemars(description = "Memory id of the assigned work/test request Fact.")]
    pub work_item_memory_id: uuid::Uuid,
    #[schemars(description = "Why this worker was assigned this item.")]
    pub reason: String,
}

impl PerspectivePayload for CodeWorkAssignmentV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("work-assignment-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_code.work_assignment_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CodeWorkAssignmentV1 schema serializes"),
        )
    }

    /// Both subjects. A Perspective sits at the top of the F/A/P order, so
    /// pointing at another Perspective (level) and at a Fact (downward)
    /// both satisfy the layering rule by construction.
    fn references(&self) -> Vec<PayloadReference> {
        vec![
            PayloadReference::memory(
                "target_perspective_memory_id",
                EntityKind::Perspective,
                MemoryId::new(self.target_perspective_memory_id),
            ),
            PayloadReference::memory(
                "work_item_memory_id",
                EntityKind::Fact,
                MemoryId::new(self.work_item_memory_id),
            ),
        ]
    }
}
