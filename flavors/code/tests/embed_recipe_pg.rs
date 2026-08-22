//! `EmbeddingRecipe::Never` is a claim about work that must not happen.

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

/// The enqueue lane binds one list of schema ids for the linked flavors, and
/// this is what it holds. Stated as its complement because the embedding set
/// is the short one and the one a reader can check: eight schemas across both
/// flavors carry text worth a vector, and every other declaration is a
/// `Never` the lane must skip whatever kind it was registered under.
#[test]
fn only_the_text_schemas_are_embeddable() {
    let registry = common::code_registry_with_test_citations();
    let mut embeds: Vec<String> = registry
        .contracts()
        .iter()
        .flat_map(|contract| contract.schemas.iter())
        .map(|schema| schema.schema_id().as_str().to_owned())
        .filter(|schema_id| registry.schema_is_embeddable(schema_id))
        .collect();
    embeds.sort();
    embeds.dedup();
    assert_eq!(
        embeds,
        [
            "core/agent-derivation-v1",
            "core/agent-note-v1",
            "core/interpretation-v1",
            "core/utterance-v1",
            "proxima-code/code-chunk-v1",
            "proxima-code/commit-summary-v1",
            "proxima-code/commit-v1",
            "proxima-code/file-revision-v1",
        ]
    );
}

/// A derived write inside an open transaction defers its vector to a job
/// rather than holding the pool slot across an HTTP call. `execution-plan-v1`
/// declares `Never`, so there is no vector to defer and no job to file: the
/// drain would find no embed unit for the schema and drop the row.
#[tokio::test]
async fn a_never_schema_enqueues_no_embedding_job() {
    let db = TestDb::fresh().await;
    let owner = test_owner();
    let engine = build_engine(db.pg.clone()).with_embed(Arc::new(ConstantEmbedding));
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let plan_source_memory_id = Uuid::now_v7();
    common::seed_memory(
        db.pg.pool_for_tests(),
        &owner,
        "test/plan-source",
        "abstraction",
        Some(plan_source_memory_id),
        None,
        &[],
    )
    .await
    .expect("insert plan source abstraction");
    let goal_activated_memory_id = seed_fact(&db, owner).await;
    let request_memory_id = seed_fact(&db, owner).await;

    let payload = plan_payload(goal_activated_memory_id, request_memory_id);
    let memory_id = MemoryId::new(Uuid::now_v7());
    let derived_from = [EdgeEndpoint::memory(
        EntityKind::Abstraction,
        MemoryId::new(plan_source_memory_id),
    )];

    let mut uow = engine.unit_of_work(&authz).await.expect("unit of work");
    // The lock is only a way to open the transaction before the derived
    // write, which is what selects the deferring arm.
    uow.advisory_xact_lock(0x0072_6563).await.expect("lock");
    let outcome = uow
        .author_derived(proxima_core::AuthorDerivedRequestInput {
            memory_id,
            owner,
            kind: EntityKind::Abstraction,
            text: payload.summary.clone(),
            schema_id: SchemaId::new(CodeExecutionPlanV1::SCHEMA_ID.into()),
            schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
            operator_kind: MemoryOperatorKind::AtoA,
            operator_id: OperatorId::new(Uuid::now_v7()),
            input_contract_id: InputContractId::new(Uuid::now_v7()),
            model_id: "test-planner",
            sidecar_payload: SidecarPayload::abstraction(payload),
            derived_from: &derived_from,
            extra_refs: &[],
            supersedes: None,
            lexical_language: None,
        })
        .await
        .expect("plan authored");
    uow.commit().await.expect("commit");

    assert_eq!(
        embedding_jobs(&db, outcome.memory_id).await,
        0,
        "a Never schema files no embedding job"
    );
    assert_eq!(
        embeddings(&db, outcome.memory_id).await,
        0,
        "a Never schema stores no vector"
    );
}

/// The same write outside a transaction takes the other arm: the vector is
/// resolved inline against the provider. `Never` has to cut that call too,
/// or the schema pays for an embedding it declared it would never hold.
#[tokio::test]
async fn a_never_schema_stores_no_inline_vector() {
    let db = TestDb::fresh().await;
    let owner = test_owner();
    let engine = build_engine(db.pg.clone()).with_embed(Arc::new(ConstantEmbedding));
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let plan_source_memory_id = Uuid::now_v7();
    common::seed_memory(
        db.pg.pool_for_tests(),
        &owner,
        "test/plan-source",
        "abstraction",
        Some(plan_source_memory_id),
        None,
        &[],
    )
    .await
    .expect("insert plan source abstraction");
    let goal_activated_memory_id = seed_fact(&db, owner).await;
    let request_memory_id = seed_fact(&db, owner).await;

    let payload = plan_payload(goal_activated_memory_id, request_memory_id);
    let memory_id = MemoryId::new(Uuid::now_v7());
    let derived_from = [EdgeEndpoint::memory(
        EntityKind::Abstraction,
        MemoryId::new(plan_source_memory_id),
    )];
    let outcome = engine
        .author_derived_authorized(
            &authz,
            proxima_core::AuthorDerivedRequestInput {
                memory_id,
                owner,
                kind: EntityKind::Abstraction,
                text: payload.summary.clone(),
                schema_id: SchemaId::new(CodeExecutionPlanV1::SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(CodeExecutionPlanV1::SCHEMA_VERSION),
                operator_kind: MemoryOperatorKind::AtoA,
                operator_id: OperatorId::new(Uuid::now_v7()),
                input_contract_id: InputContractId::new(Uuid::now_v7()),
                model_id: "test-planner",
                sidecar_payload: SidecarPayload::abstraction(payload),
                derived_from: &derived_from,
                extra_refs: &[],
                supersedes: None,
                lexical_language: None,
            },
        )
        .await
        .expect("plan authored");

    assert_eq!(
        embeddings(&db, outcome.memory_id).await,
        0,
        "a Never schema stores no vector"
    );
}

async fn embedding_jobs(db: &TestDb, memory_id: MemoryId) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*)::bigint FROM proxima_core.embedding_jobs WHERE entity_id = $1",
    )
    .bind(memory_id.into_inner())
    .fetch_one(db.pg.pool_for_tests())
    .await
    .expect("count embedding jobs")
}

async fn embeddings(db: &TestDb, memory_id: MemoryId) -> i64 {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.embeddings WHERE entity_id = $1")
        .bind(memory_id.into_inner())
        .fetch_one(db.pg.pool_for_tests())
        .await
        .expect("count embeddings")
}

async fn seed_fact(db: &TestDb, owner: proxima_core::Owner) -> Uuid {
    let memory_id = Uuid::now_v7();
    common::seed_memory(
        db.pg.pool_for_tests(),
        &owner,
        "test/plan-subject",
        "fact",
        Some(memory_id),
        None,
        &[],
    )
    .await
    .expect("insert plan subject fact");
    memory_id
}

fn plan_payload(goal_activated_memory_id: Uuid, request_memory_id: Uuid) -> CodeExecutionPlanV1 {
    CodeExecutionPlanV1 {
        repo_id: Uuid::now_v7(),
        plan_key: "goal:repo:plan".to_string(),
        goal_activated_memory_id,
        summary: "a plan that never embeds".to_string(),
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
