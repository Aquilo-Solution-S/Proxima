use proxima_core::{
    EntityKind, FactPayload, MemoryId, PayloadKeyBuilder, PayloadReference, proxima_schema_id,
};
use serde::{Deserialize, Serialize};

/// Dispatch-boundary Fact: a planner requested implementation work for
/// a repo. Durable intent/rationale belongs to `CodeExecutionPlanV1`
/// or the originating core Goal; this Fact is the observed wake request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkRequestedV1 {
    pub repo_id: uuid::Uuid,
    pub title: String,
    pub instructions: String,
    pub request_key: String,
    /// Work items that must land before this one can dispatch. The
    /// schema-declared reference field that replaced `core/depends-on`:
    /// the dependency is a property of the depending row, so the index
    /// rows come from here (docs/16 §Flavor Migration).
    ///
    /// Not receipt key material — `repo_id` plus `request_key` is what
    /// makes a request the same request.
    #[serde(default)]
    pub depends_on_memory_ids: Vec<uuid::Uuid>,
}

impl FactPayload for WorkRequestedV1 {
    /// Matches this schema's `EmbeddingRecipe::Never`, which carries the
    /// reason. Freeze refuses the two disagreeing; they did, and the
    /// enqueue lane filed jobs the drain could only drop.
    const EMBEDDABLE: bool = false;

    const SCHEMA_ID: &'static str = proxima_schema_id!("work-requested-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("repo_id", self.repo_id);
        key.field_str("request_key", &self.request_key);
        key.finish()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.work_requested_v1")
    }

    fn render(&self) -> String {
        format!("{}: {}", self.request_key, self.title)
    }

    fn references(&self) -> Vec<PayloadReference> {
        self.depends_on_memory_ids
            .iter()
            .map(|memory_id| {
                PayloadReference::memory(
                    "depends_on_memory_ids",
                    EntityKind::Fact,
                    MemoryId::new(*memory_id),
                )
            })
            .collect()
    }
}

/// Backwards-compatible Rust type alias while MCP/tool names migrate.
pub type ExecutionRequestV1 = WorkRequestedV1;
