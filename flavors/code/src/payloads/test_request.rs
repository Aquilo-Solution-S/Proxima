use proxima_core::{
    EntityKind, FactPayload, MemoryId, PayloadKeyBuilder, PayloadReference, ScopeKind,
    proxima_schema_id,
};
use serde::{Deserialize, Serialize};

use crate::repos::CODE_REPO_SCOPE;

use super::AcceptanceCriterionV1;

/// Dispatch-boundary Fact: a planner requested verification work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TestRequestedV1 {
    pub repo_id: uuid::Uuid,
    pub title: String,
    pub instructions: String,
    pub test_key: String,
    pub criteria: Vec<AcceptanceCriterionV1>,
    /// Work items that must land before this test can dispatch — the
    /// same reference field `WorkRequestedV1` carries, for the same
    /// reason: a dependency belongs to the depending row.
    #[serde(default)]
    pub depends_on_memory_ids: Vec<uuid::Uuid>,
}

impl FactPayload for TestRequestedV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-requested-v1");
    const SCHEMA_VERSION: u32 = 1;
    /// Repo-scoped. The substrate takes the `code-repo` fence and re-asks
    /// whether the repository is still registered on EVERY admission of
    /// this payload, whoever the writer is.
    const SCOPE_KIND: Option<ScopeKind> = Some(CODE_REPO_SCOPE);
    fn scope_id(&self) -> Option<uuid::Uuid> {
        Some(self.repo_id)
    }

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("repo_id", self.repo_id);
        key.field_str("test_key", &self.test_key);
        key.finish()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("proxima_code.test_requested_v1")
    }

    fn render(&self) -> String {
        format!("{}: {}", self.test_key, self.title)
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
pub type TestRequestV1 = TestRequestedV1;
