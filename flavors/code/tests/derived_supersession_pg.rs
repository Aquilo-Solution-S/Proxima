mod common;

use std::sync::Arc;

use async_trait::async_trait;
use common::{TestDb, test_owner};
use proxima_code::testkit::build_engine;
use proxima_code::{CodeExecutionPlanItemKind, CodeExecutionPlanItemV1, CodeExecutionPlanV1};
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError};
use proxima_core::{
    AuthPath, AuthzContext, DerivationIdentity, DerivedMemory, MemoryId, MemoryTarget, OperatorId,
    SeriesHandle,
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
    common::register_fixture_repo(db.pg.pool_for_tests(), &owner, repo_id).await;
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

    // The Facts the plan's payload points at: the activation it was planned
    // under, and the request behind its one item. Both must exist before the
    // plan is written — an index row cannot point at a node that is not there.
    let goal_activated_memory_id = seed_fact(&db, owner, "goal activation").await;
    let request_memory_id = seed_fact(&db, owner, "work requested").await;

    let old_memory_id = MemoryId::new(Uuid::now_v7());
    let plan_key = "goal:repo:plan";

    // A→A must originate from an Abstraction. The two Facts the payload
    // names arrive as references.
    let derived_from = [MemoryId::new(plan_source_memory_id)];
    let old_payload = plan_payload(
        repo_id,
        goal_activated_memory_id,
        request_memory_id,
        plan_key,
        "old plan",
    );
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let old_outcome = engine
        .derive_memory(
            &authz,
            DerivedMemory::abstraction(
                MemoryTarget::Series(SeriesHandle::new(old_memory_id.into_inner())),
                owner,
                old_payload.summary.clone(),
                old_payload,
                derived_from,
                DerivationIdentity::new(
                    OperatorId::new(Uuid::now_v7()),
                    proxima_core::InputContractId::new(Uuid::now_v7()),
                ),
            )
            .expect("typed old plan request"),
        )
        .await
        .expect("old plan authored");
    // One origin (the Abstraction input) and two references (the activation
    // Fact and the item's request Fact).
    let _ = old_outcome;

    let new_payload = plan_payload(
        repo_id,
        goal_activated_memory_id,
        request_memory_id,
        plan_key,
        "new plan",
    );
    let new_outcome = engine
        .derive_memory(
            &authz,
            DerivedMemory::abstraction(
                MemoryTarget::Revision(old_outcome.memory_id),
                owner,
                new_payload.summary.clone(),
                new_payload,
                derived_from,
                DerivationIdentity::new(
                    OperatorId::new(Uuid::now_v7()),
                    proxima_core::InputContractId::new(Uuid::now_v7()),
                ),
            )
            .expect("typed revised plan request"),
        )
        .await
        .expect("new plan authored");
    let _ = new_outcome;
    let current_plan_ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT p.t
           FROM proxima_code.execution_plan_v1 p
           JOIN proxima_core.memory_head h ON h.t = p.t
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
          WHERE m.owner_id = $1
            AND p.plan_key = $2
          ORDER BY m.t DESC",
    )
    .bind(owner.stored_owner_id())
    .bind(plan_key)
    .fetch_all(db.pg.pool_for_tests())
    .await
    .expect("query current code plans");
    assert!(
        current_plan_ids.contains(&new_outcome.memory_id.into_inner()),
        "new plan t must be the live head"
    );
    assert!(
        !current_plan_ids.contains(&old_outcome.memory_id.into_inner()),
        "superseded plan t must not remain the live head"
    );
}

/// A Fact row the plan's payload may point at. Receiptless, which is legal
/// for a seeded fixture and is all the index needs: the endpoint has to
/// exist and be of the declared kind.
async fn seed_fact(db: &TestDb, owner: proxima_core::Owner, _text: &str) -> Uuid {
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
