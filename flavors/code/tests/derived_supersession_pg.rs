mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{TestDb, test_owner};
use proxima_code::{
    CodeExecutionPlanItemKind, CodeExecutionPlanItemV1, CodeExecutionPlanV1, build_engine,
};
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError};
use proxima_core::{
    AbstractionPayload, EntityKind, MemoryId, MemoryOperatorKind, SchemaId, SchemaVersion,
    SidecarPayload,
};
use uuid::Uuid;

#[derive(Debug)]
struct ConstantEmbedding;

#[async_trait]
impl EmbeddingClient for ConstantEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        let mut embedding = vec![0.0; EMBEDDING_DIM];
        embedding[0] = 1.0;
        Ok(embedding)
    }

    fn model_id(&self) -> &'static str {
        "test-code-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn code_execution_plan_can_use_core_superseding_derived_authoring() {
    let db = TestDb::fresh().await;
    let owner = test_owner();
    let engine = build_engine(db.pg.clone()).with_embed(Arc::new(ConstantEmbedding));

    let repo_id = Uuid::now_v7();
    let goal_activated_memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, 'test/goal-activation', 1, 'Abstraction',
                 'goal activation evidence', 'ExternalAgent', 'test', 'test', $4, 0)",
    )
    .bind(goal_activated_memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(Uuid::nil())
    .execute(db.pg.pool())
    .await
    .expect("insert goal activation evidence");

    let old_memory_id = MemoryId::new(Uuid::now_v7());
    let new_memory_id = MemoryId::new(Uuid::now_v7());
    let plan_key = "goal:repo:plan";

    let old_payload = plan_payload(repo_id, goal_activated_memory_id, plan_key, "old plan");
    let old_outcome = engine
        .author_derived(proxima_core::AuthorDerivedRequestInput {
            memory_id: old_memory_id,
            owner,
            kind: EntityKind::Abstraction,
            text: old_payload.summary.clone(),
            schema_id: SchemaId::new(CodeExecutionPlanV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::ExternalAgent,
            model_id: "test-planner",
            prompt_version: "proxima-code/test-plan-v1",
            author_personality_instance_id: None,
            sidecar_payload: SidecarPayload::abstraction(old_payload),
            supersedes: None,
            edges: &[],
        })
        .await
        .expect("old plan authored");
    assert_eq!(old_outcome.edge_count, 0);

    let new_payload = plan_payload(repo_id, goal_activated_memory_id, plan_key, "new plan");
    let new_outcome = engine
        .author_derived(proxima_core::AuthorDerivedRequestInput {
            memory_id: new_memory_id,
            owner,
            kind: EntityKind::Abstraction,
            text: new_payload.summary.clone(),
            schema_id: SchemaId::new(CodeExecutionPlanV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::ExternalAgent,
            model_id: "test-planner",
            prompt_version: "proxima-code/test-plan-v1",
            author_personality_instance_id: None,
            sidecar_payload: SidecarPayload::abstraction(new_payload),
            supersedes: Some(old_memory_id),
            edges: &[],
        })
        .await
        .expect("new plan authored");
    assert_eq!(new_outcome.edge_count, 1);

    let supersedes: Option<Uuid> =
        sqlx::query_scalar("SELECT supersedes FROM proxima_core.memories WHERE memory_id = $1")
            .bind(new_memory_id.into_inner())
            .fetch_one(db.pg.pool())
            .await
            .expect("read supersedes column");
    assert_eq!(supersedes, Some(old_memory_id.into_inner()));

    let edge_count: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM proxima_core.edges
          WHERE relation = $1
            AND source_memory_id = $2
            AND target_memory_id = $3
            AND relation_class = 'Supersession'",
    )
    .bind(proxima_core::CORE_SUPERSEDES_RELATION)
    .bind(new_memory_id.into_inner())
    .bind(old_memory_id.into_inner())
    .fetch_one(db.pg.pool())
    .await
    .expect("read supersedes edge");
    assert_eq!(edge_count, 1);

    let current_plan_ids: Vec<Uuid> =
        sqlx::query_scalar(
            "SELECT m.memory_id
           FROM proxima_core.memories m
           JOIN proxima_code.execution_plan_v1 p USING (memory_id)
           JOIN (SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories UNION ALL SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals) eo
             ON eo.entity_id = m.memory_id
WHERE eo.owner_kind = $1
            AND eo.owner_id = $2
            AND m.schema_id = $3
            AND p.plan_key = $4
            AND NOT EXISTS (
                 SELECT 1 FROM proxima_core.memories newer
                  WHERE newer.supersedes = m.memory_id
                    AND newer.tombstoned_at IS NULL
            )",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(CodeExecutionPlanV1::SCHEMA_ID)
        .bind(plan_key)
        .fetch_all(db.pg.pool())
        .await
        .expect("query current code plans");
    assert_eq!(current_plan_ids, vec![new_memory_id.into_inner()]);
}

fn plan_payload(
    repo_id: Uuid,
    goal_activated_memory_id: Uuid,
    plan_key: &str,
    summary: &str,
) -> CodeExecutionPlanV1 {
    CodeExecutionPlanV1 {
        repo_id,
        plan_key: plan_key.to_string(),
        goal_activated_memory_id,
        summary: summary.to_string(),
        items: vec![CodeExecutionPlanItemV1 {
            key: "work".to_string(),
            kind: CodeExecutionPlanItemKind::Work,
            title: "Implement work".to_string(),
            depends_on: Vec::new(),
            request_key: "request-work".to_string(),
        }],
        evidence_memory_ids: Vec::new(),
    }
}
