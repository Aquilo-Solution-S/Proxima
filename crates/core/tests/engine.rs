//! M1 done-when proof — boot the Engine, Schema returns empty,
//! Query over the configured Owner returns empty, Query for a
//! foreign Owner returns Forbidden.

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::ids::{OrgId, UserId};
use proxima_core::operators::{
    ConsolidateBatchF2AOutcome, ConsolidateBatchF2ARequest, EmbeddingClient, F2AContext,
    F2AInvocationKey, F2AOperator, FactRow, LlmClient, NewAbstraction, OperatorError,
    OperatorRegistry,
};
use proxima_core::owner::{Owner, Principal};
use proxima_core::storage::{Storage, StorageError};
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::{MemoryStore, QueryRequest};
use proxima_core::verbs::schema::{SchemaRegistry, SchemaRequest};
use proxima_core::verbs::subscribe::ChangeEventStream;
use proxima_core::{
    AbstractionPayload, FactPayload, FlavorRegistry, GoalId, MemoryId, SchemaId, SchemaVersion,
    SourceBatchId,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use time::OffsetDateTime;
use uuid::Uuid;

fn fresh_owner() -> (Principal, Owner) {
    let user = UserId::new(Uuid::now_v7());
    let principal = Principal::User(user);
    let owner = Owner {
        principal: principal.clone(),
        org_id: OrgId::new(Uuid::now_v7()),
    };
    (principal, owner)
}

fn boot_engine(principal: Principal, owner: Owner) -> Engine {
    let resolver = NoAuth::new(principal, owner);
    Engine::new(
        SchemaRegistry::new(),
        MemoryStore::new(),
        Box::new(resolver),
    )
}

#[test]
fn schema_verb_returns_empty_registry() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner);
    let resp = engine.schema(&SchemaRequest);
    assert!(resp.schemas.is_empty(), "M1 registry must be empty");
}

#[tokio::test]
async fn query_verb_returns_empty_for_configured_owner() {
    let (principal, owner) = fresh_owner();
    let engine = boot_engine(principal, owner.clone());

    let resp = engine
        .query(&Credentials::None, &QueryRequest::for_owner(owner))
        .await
        .expect("NoAuth single-Owner query must succeed");

    assert!(resp.memories.is_empty(), "M1 store must be empty");
    assert!(
        resp.seq_high_water.is_none(),
        "no events have been written; seq_high_water must be None"
    );
}

#[tokio::test]
async fn query_verb_allows_same_principal_with_different_org() {
    let (principal, configured) = fresh_owner();
    let engine = boot_engine(principal, configured.clone());
    let same_principal_different_org = Owner {
        principal: configured.principal,
        org_id: OrgId::new(Uuid::now_v7()),
    };

    let resp = engine
        .query(
            &Credentials::None,
            &QueryRequest::for_owner(same_principal_different_org),
        )
        .await
        .expect("access is scoped by principal, not org_id");

    assert!(resp.memories.is_empty(), "M1 store must be empty");
}

#[tokio::test]
async fn query_verb_rejects_foreign_owner_with_forbidden() {
    let (principal, configured) = fresh_owner();
    let engine = boot_engine(principal, configured);

    // A different Owner — same shape, fresh ids, NOT in
    // NoAuth's accessible set.
    let (_, foreign) = fresh_owner();

    let err = engine
        .query(&Credentials::None, &QueryRequest::for_owner(foreign))
        .await
        .expect_err("foreign Owner must be rejected");
    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestFact {
    text: String,
}

impl FactPayload for TestFact {
    const SCHEMA_ID: &'static str = "test/fact";
    const SCHEMA_VERSION: u32 = 1;

    fn render(&self) -> String {
        self.text.clone()
    }

    fn sidecar_table() -> &'static str {
        "test.fact_v1"
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestAbs {
    summary: String,
}

impl AbstractionPayload for TestAbs {
    const SCHEMA_ID: &'static str = "test/out";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "test.out_v1"
    }
}

#[derive(Debug)]
struct FakeStorage {
    facts: Vec<FactRow>,
    consolidate_calls: AtomicUsize,
}

impl FakeStorage {
    fn new(facts: Vec<FactRow>) -> Self {
        Self {
            facts,
            consolidate_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl Storage for FakeStorage {
    async fn ingest_event_atomic(
        &self,
        _draft: &EventDraft,
    ) -> Result<EventIngestOutcome, StorageError> {
        unimplemented!("not used by these tests")
    }

    async fn write_goal_atomic(
        &self,
        _draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        unimplemented!("not used by these tests")
    }

    async fn supersede_goal_atomic(
        &self,
        _prior: GoalId,
        _draft: &GoalDraft,
    ) -> Result<GoalWriteOutcome, StorageError> {
        unimplemented!("not used by these tests")
    }

    async fn subscribe_changes(
        &self,
        _owner: &Owner,
        _since: Option<Uuid>,
    ) -> Result<ChangeEventStream, StorageError> {
        Ok(Box::pin(futures_util::stream::empty()))
    }

    async fn query_memories(
        &self,
        _req: &proxima_core::verbs::query::QueryRequest,
        _schemas: &[proxima_core::verbs::schema::SchemaInfo],
    ) -> Result<proxima_core::verbs::query::QueryResponse, StorageError> {
        Ok(proxima_core::verbs::query::QueryResponse {
            memories: Vec::new(),
            goals: Vec::new(),
            edges: Vec::new(),
            seq_high_water: None,
        })
    }

    async fn close_batch(
        &self,
        _owner: &Owner,
        source_batch_id: SourceBatchId,
    ) -> Result<CloseBatchOutcome, StorageError> {
        Ok(CloseBatchOutcome {
            source_batch_id,
            closed_at: OffsetDateTime::now_utc(),
            already_closed: false,
        })
    }

    async fn load_batch_facts(
        &self,
        _owner: &Owner,
        _batch_id: SourceBatchId,
        _sidecars: &[proxima_core::operators::SidecarSpec],
    ) -> Result<Vec<FactRow>, StorageError> {
        Ok(self.facts.clone())
    }

    async fn consolidate_batch_f2a(
        &self,
        _req: &ConsolidateBatchF2ARequest<'_>,
    ) -> Result<ConsolidateBatchF2AOutcome, StorageError> {
        self.consolidate_calls.fetch_add(1, Ordering::SeqCst);
        Ok(ConsolidateBatchF2AOutcome {
            abstraction_ids: Vec::new(),
            already_consolidated: false,
        })
    }

    async fn list_unconsolidated_batches(
        &self,
        _owner: &Owner,
        _key: &F2AInvocationKey<'_>,
    ) -> Result<Vec<SourceBatchId>, StorageError> {
        Ok(Vec::new())
    }
}

#[derive(Debug)]
struct StaticOutputOp {
    outputs: Vec<NewAbstraction>,
}

#[async_trait::async_trait]
impl F2AOperator for StaticOutputOp {
    fn operator_id(&self) -> &'static str {
        "test/f2a"
    }

    fn output_schema_id(&self) -> &'static str {
        TestAbs::SCHEMA_ID
    }

    fn output_schema_version(&self) -> u32 {
        TestAbs::SCHEMA_VERSION
    }

    fn prompt_version(&self) -> &'static str {
        "v1"
    }

    fn consumes(&self, schema_id: &SchemaId) -> bool {
        schema_id.as_str() == TestFact::SCHEMA_ID
    }

    async fn run(&self, _ctx: F2AContext<'_>) -> Result<Vec<NewAbstraction>, OperatorError> {
        Ok(self.outputs.clone())
    }
}

#[derive(Debug)]
struct FakeLlm;

#[async_trait::async_trait]
impl LlmClient for FakeLlm {
    async fn complete_json(
        &self,
        _system_prompt: &str,
        _user_prompt: &str,
    ) -> Result<serde_json::Value, OperatorError> {
        Ok(serde_json::json!({}))
    }

    fn model_id(&self) -> &str {
        "test-llm"
    }
}

#[derive(Debug)]
struct FakeEmbed;

#[async_trait::async_trait]
impl EmbeddingClient for FakeEmbed {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, OperatorError> {
        Ok(vec![0.0])
    }

    fn model_id(&self) -> &str {
        "test-embed"
    }

    fn dim(&self) -> usize {
        1
    }
}

fn f2a_engine_with_output(
    principal: Principal,
    owner: Owner,
    output: NewAbstraction,
) -> (Engine, Arc<FakeStorage>, SourceBatchId) {
    let fact_id = output.provenance[0];
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema::<TestFact>();
    registry.add_abstraction_schema::<TestAbs>();

    let storage = Arc::new(FakeStorage::new(vec![FactRow {
        memory_id: fact_id,
        schema_id: TestFact::schema_id(),
        schema_version: SchemaVersion::new(TestFact::SCHEMA_VERSION),
        payload_json: serde_json::json!({ "text": "source fact" }),
    }]));

    let mut ops = OperatorRegistry::new();
    ops.register_f2a(StaticOutputOp {
        outputs: vec![output],
    });

    let batch_id = SourceBatchId::new(Uuid::now_v7());
    let engine = Engine::new(
        registry.freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(principal, owner)),
    )
    .with_storage(storage.clone())
    .with_operators(ops)
    .with_llm(Arc::new(FakeLlm))
    .with_embed(Arc::new(FakeEmbed));

    (engine, storage, batch_id)
}

fn valid_output_with(
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    typed_payload: serde_json::Value,
) -> NewAbstraction {
    NewAbstraction {
        schema_id,
        schema_version,
        text: "summary".into(),
        typed_payload,
        provenance: vec![MemoryId::new(Uuid::now_v7())],
        embedding: vec![0.0],
        embedding_model_id: "test-embed".into(),
    }
}

#[tokio::test]
async fn f2a_rejects_output_schema_mismatch_before_storage_write() {
    let (principal, owner) = fresh_owner();
    let output = valid_output_with(
        SchemaId::new("test/wrong".into()),
        SchemaVersion::new(TestAbs::SCHEMA_VERSION),
        serde_json::json!({ "summary": "ok" }),
    );
    let (engine, storage, batch_id) = f2a_engine_with_output(principal, owner.clone(), output);

    let err = engine
        .close_batch(&Credentials::None, owner, batch_id)
        .await
        .expect_err("schema mismatch must be rejected");

    assert_eq!(err.code, ErrorCode::Internal);
    assert!(err.message.contains("returned schema test/wrong v1"));
    assert_eq!(storage.consolidate_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn f2a_rejects_invalid_output_payload_before_storage_write() {
    let (principal, owner) = fresh_owner();
    let output = valid_output_with(
        TestAbs::schema_id(),
        SchemaVersion::new(TestAbs::SCHEMA_VERSION),
        serde_json::json!({ "summary": 42 }),
    );
    let (engine, storage, batch_id) = f2a_engine_with_output(principal, owner.clone(), output);

    let err = engine
        .close_batch(&Credentials::None, owner, batch_id)
        .await
        .expect_err("invalid typed payload must be rejected");

    assert_eq!(err.code, ErrorCode::Internal);
    assert!(err.message.contains("returned invalid test/out v1 payload"));
    assert_eq!(storage.consolidate_calls.load(Ordering::SeqCst), 0);
}
