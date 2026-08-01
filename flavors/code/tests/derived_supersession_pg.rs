mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{TestDb, test_owner};
use proxima_code::testkit::build_engine;
use proxima_code::{CodeExecutionPlanItemKind, CodeExecutionPlanItemV1, CodeExecutionPlanV1};
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError};
use proxima_core::{
    AbstractionPayload, AuthPath, AuthzContext, EdgeEndpoint, EntityKind, InputContractId,
    MemoryId, MemoryOperatorKind, OperatorId, SchemaId, SchemaVersion, SidecarPayload,
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
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    // The plan's Abstraction *input* — what it was made from.
    let plan_source_memory_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id,
             prompt_version)
         VALUES ($1, $2, $3, 'test/plan-source', 1, 'Abstraction',
                 'planning synthesis', 'AtoA',
                 '00000000-0000-0000-0000-000000000451'::uuid,
                 '00000000-0000-0000-0000-000000000452'::uuid, NULL,
                 'test', 'test')",
    )
    .bind(plan_source_memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(db.pg.pool_for_tests())
    .await
    .expect("insert plan source abstraction");

    // The Facts the plan's payload points at: the activation it was planned
    // under, and the request behind its one item. Both must exist before the
    // plan is written — an index row cannot point at a node that is not there.
    let goal_activated_memory_id = seed_fact(&db, owner, "goal activation").await;
    let request_memory_id = seed_fact(&db, owner, "work requested").await;

    let old_memory_id = MemoryId::new(Uuid::now_v7());
    let new_memory_id = MemoryId::new(Uuid::now_v7());
    let plan_key = "goal:repo:plan";

    // What the plan was made from is a list of endpoints on the request; the
    // two Facts its payload names arrive as references, derived from the
    // payload rather than passed beside it.
    let derived_from = [EdgeEndpoint::memory(
        EntityKind::Abstraction,
        MemoryId::new(plan_source_memory_id),
    )];
    let old_payload = plan_payload(
        repo_id,
        goal_activated_memory_id,
        request_memory_id,
        plan_key,
        "old plan",
    );
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let old_outcome = engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
                memory_id: old_memory_id,
                owner,
                kind: EntityKind::Abstraction,
                text: old_payload.summary.clone(),
                schema_id: SchemaId::new(CodeExecutionPlanV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
                operator_kind: MemoryOperatorKind::AtoA,
                operator_id: OperatorId::new(Uuid::now_v7()),
                input_contract_id: InputContractId::new(Uuid::now_v7()),
                source_batch_id: None,
                model_id: "test-planner",
                prompt_version: "proxima-code/test-plan-v1",
                sidecar_payload: SidecarPayload::abstraction(old_payload),
                authoring_perspective_id: None,
                derived_from: &derived_from,
                supersedes: None,
                lexical_language: None,
            },
        )
        .await
        .expect("old plan authored");
    // One origin (the Abstraction input) and two references (the activation
    // Fact and the item's request Fact).
    assert_eq!(old_outcome.edge_count, 3);

    let new_payload = plan_payload(
        repo_id,
        goal_activated_memory_id,
        request_memory_id,
        plan_key,
        "new plan",
    );
    let new_outcome = engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
                memory_id: new_memory_id,
                owner,
                kind: EntityKind::Abstraction,
                text: new_payload.summary.clone(),
                schema_id: SchemaId::new(CodeExecutionPlanV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
                operator_kind: MemoryOperatorKind::AtoA,
                operator_id: OperatorId::new(Uuid::now_v7()),
                input_contract_id: InputContractId::new(Uuid::now_v7()),
                source_batch_id: None,
                model_id: "test-planner",
                prompt_version: "proxima-code/test-plan-v1",
                sidecar_payload: SidecarPayload::abstraction(new_payload),
                authoring_perspective_id: None,
                derived_from: &derived_from,
                supersedes: Some(old_memory_id),
                lexical_language: None,
            },
        )
        .await
        .expect("new plan authored");
    assert_eq!(new_outcome.edge_count, 3);

    let supersedes: Option<Uuid> =
        sqlx::query_scalar("SELECT supersedes FROM proxima_core.memories WHERE memory_id = $1")
            .bind(new_memory_id.into_inner())
            .fetch_one(db.pg.pool_for_tests())
            .await
            .expect("read supersedes column");
    assert_eq!(supersedes, Some(old_memory_id.into_inner()));

    // Supersession is not a connection between two things — it is the same
    // thing persisting through revision — so it is a pointer on both rows and
    // no edge exists to find.
    let superseded_by: Option<Uuid> =
        sqlx::query_scalar("SELECT superseded_by FROM proxima_core.memories WHERE memory_id = $1")
            .bind(old_memory_id.into_inner())
            .fetch_one(db.pg.pool_for_tests())
            .await
            .expect("read superseded_by column");
    assert_eq!(superseded_by, Some(new_memory_id.into_inner()));

    let supersession_edges: i64 = sqlx::query_scalar(
        "SELECT count(*)
           FROM proxima_core.edges
          WHERE source_id = $1
            AND target_id = $2",
    )
    .bind(new_memory_id.into_inner())
    .bind(old_memory_id.into_inner())
    .fetch_one(db.pg.pool_for_tests())
    .await
    .expect("read supersession edges");
    assert_eq!(supersession_edges, 0, "supersession writes no edge");

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
        .fetch_all(db.pg.pool_for_tests())
        .await
        .expect("query current code plans");
    assert_eq!(current_plan_ids, vec![new_memory_id.into_inner()]);
}

/// A Fact row the plan's payload may point at. Receiptless, which is legal
/// for a seeded fixture and is all the index needs: the endpoint has to
/// exist and be of the declared kind.
async fn seed_fact(db: &TestDb, owner: proxima_core::Owner, text: &str) -> Uuid {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    // A Fact row carries `kind = NULL`; the F/A/P discriminator only names
    // the derived layers.
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text)
         VALUES ($1, $2, $3, 'test/plan-subject', 1, NULL, $4)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .execute(db.pg.pool_for_tests())
    .await
    .expect("insert plan subject fact");
    memory_id
}

fn plan_payload(
    repo_id: Uuid,
    goal_activated_memory_id: Uuid,
    request_memory_id: Uuid,
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
            request_memory_id,
        }],
        evidence_memory_ids: Vec::new(),
    }
}
