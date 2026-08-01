use proxima_core::{EntityKind, MemoryId, PayloadReference, PerspectivePayload, proxima_schema_id};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// "This worker should pick up that request" — the node that replaced the
/// `proxima-code/targets-execution-request` relation.
///
/// The statement could not stay where the edge used to put it. Its
/// endpoints are a worker Perspective and a request Fact, and a Fact
/// asserts no judgment, so the request cannot be the source; the target
/// Perspective already exists and rows are append-only, so it cannot be
/// amended to say it either. Neither endpoint owns the claim, which by
/// the node-home test (docs/16 §The Thesis) means the model was missing a
/// node rather than an edge kind. This is that node: a Perspective whose
/// payload references both subjects, so the two `reference` index rows
/// are re-derivable from it and nothing writes an edge.
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
