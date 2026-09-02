use proxima_core::{
    AbstractionPayload, EntityKind, MemoryId, PayloadReference, ScopeKind, proxima_schema_id,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::repos::CODE_REPO_SCOPE;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, sqlx::Type)]
#[serde(rename_all = "snake_case")]
#[sqlx(
    type_name = "proxima_code.execution_plan_item_kind",
    rename_all = "snake_case"
)]
pub enum CodeExecutionPlanItemKind {
    Work,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeExecutionPlanItemV1 {
    pub key: String,
    pub kind: CodeExecutionPlanItemKind,
    pub title: String,
    pub depends_on: Vec<String>,
    pub request_key: String,
    /// The request Fact this item was emitted as. A schema-declared
    /// reference field: the plan is the node that owns "this plan item is
    /// that request", so the plan is written *after* its items and the
    /// index rows follow from this field.
    pub request_memory_id: uuid::Uuid,
}

/// Goal-native Code work plan. The durable desired future remains the
/// core Goal; this Abstraction records the code-flavor planning
/// synthesis derived from an Abstraction proof input in the context of
/// an active goal activation Fact plus optional evidence Facts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CodeExecutionPlanV1 {
    pub repo_id: uuid::Uuid,
    pub plan_key: String,
    pub goal_activated_memory_id: uuid::Uuid,
    pub summary: String,
    pub items: Vec<CodeExecutionPlanItemV1>,
    pub evidence_memory_ids: Vec<uuid::Uuid>,
}

impl AbstractionPayload for CodeExecutionPlanV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("execution-plan-v1");
    const SCHEMA_VERSION: u32 = 1;
    /// Repo-scoped. The substrate takes the `code-repo` fence and re-asks
    /// whether the repository is still registered on EVERY admission of
    /// this payload, whoever the writer is.
    const SCOPE_KIND: Option<ScopeKind> = Some(CODE_REPO_SCOPE);
    fn scope_id(&self) -> Option<uuid::Uuid> {
        Some(self.repo_id)
    }

    fn sidecar_table() -> &'static str {
        "proxima_code.execution_plan_v1"
    }

    fn json_schema() -> Option<serde_json::Value> {
        Some(
            serde_json::to_value(schemars::schema_for!(Self))
                .expect("CodeExecutionPlanV1 schema serializes"),
        )
    }

    /// Everything this plan points at: the activation Fact it was planned
    /// under, the evidence Facts it read, and the request Fact behind each
    /// item. Its Abstraction *input* is a separate claim and travels as
    /// `derived_from`, which lands `origin` rows instead.
    fn references(&self) -> Vec<PayloadReference> {
        let mut references = Vec::with_capacity(2 + self.evidence_memory_ids.len());
        references.push(PayloadReference::memory(
            "goal_activated_memory_id",
            EntityKind::Fact,
            MemoryId::new(self.goal_activated_memory_id),
        ));
        references.extend(self.evidence_memory_ids.iter().map(|memory_id| {
            PayloadReference::memory(
                "evidence_memory_ids",
                EntityKind::Fact,
                MemoryId::new(*memory_id),
            )
        }));
        references.extend(self.items.iter().map(|item| {
            PayloadReference::memory(
                "items.request_memory_id",
                EntityKind::Fact,
                MemoryId::new(item.request_memory_id),
            )
        }));
        references
    }
}
