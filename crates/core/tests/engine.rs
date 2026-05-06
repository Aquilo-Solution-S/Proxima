//! M1 done-when proof — boot the Engine, Schema returns empty,
//! Query over the configured Owner returns empty, Query for a
//! foreign Owner returns Forbidden.

use proxima_core::auth::{Credentials, NoAuth};
use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::ids::{OrgId, UserId};
use proxima_core::operators::{
    A2PContext, A2PContextSpec, A2PInvocationKey, A2PLineageKey, A2POperator, AbstractionRow,
    ConsolidateA2POutcome, ConsolidateA2PRequest, ConsolidateBatchF2AOutcome,
    ConsolidateBatchF2ARequest, EmbeddingClient, F2AContext, F2AInvocationKey, F2AOperator,
    FactRow, LlmClient, NewAbstraction, NewPerspective, OperatorError, OperatorRegistry,
    PersonalitySnapshot,
};
use proxima_core::owner::{Owner, Principal};
use proxima_core::personality::{PersonalityContext, PersonalityFlavor};
use proxima_core::storage::{Storage, StorageError};
use proxima_core::verbs::close_batch::CloseBatchOutcome;
use proxima_core::verbs::event_history::{EventHistoryRequest, EventHistoryResponse};
use proxima_core::verbs::event_ingest::{EventDraft, EventIngestOutcome};
use proxima_core::verbs::goal_write::{GoalDraft, GoalWriteOutcome};
use proxima_core::verbs::query::{MemoryStore, QueryRequest};
use proxima_core::verbs::schema::{SchemaRegistry, SchemaRequest};
use proxima_core::verbs::subscribe::ChangeEventStream;
use proxima_core::{
    AbstractionPayload, FactPayload, FlavorRegistry, GoalId, MemoryId, ModelTier, PersonalityId,
    PersonalityStateHash, PerspectivePayload, SchemaId, SchemaVersion, SourceBatchId,
};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TestPerspective {
    summary: String,
}

impl PerspectivePayload for TestPerspective {
    const SCHEMA_ID: &'static str = "test/perspective";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "test.perspective_v1"
    }
}

#[derive(Debug)]
struct FakeStorage {
    facts: Vec<FactRow>,
    abstractions: Vec<AbstractionRow>,
    consolidate_calls: AtomicUsize,
    consolidate_a2p_calls: AtomicUsize,
    has_a2p_invocation: bool,
    prior_a2p_head: Option<MemoryId>,
    last_a2p_model_id: Mutex<Option<String>>,
}

impl FakeStorage {
    fn new(facts: Vec<FactRow>) -> Self {
        Self {
            facts,
            abstractions: Vec::new(),
            consolidate_calls: AtomicUsize::new(0),
            consolidate_a2p_calls: AtomicUsize::new(0),
            has_a2p_invocation: false,
            prior_a2p_head: None,
            last_a2p_model_id: Mutex::new(None),
        }
    }

    fn with_abstractions(abstractions: Vec<AbstractionRow>) -> Self {
        Self {
            facts: Vec::new(),
            abstractions,
            consolidate_calls: AtomicUsize::new(0),
            consolidate_a2p_calls: AtomicUsize::new(0),
            has_a2p_invocation: false,
            prior_a2p_head: None,
            last_a2p_model_id: Mutex::new(None),
        }
    }

    fn with_existing_a2p_invocation(mut self) -> Self {
        self.has_a2p_invocation = true;
        self
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

    async fn event_history(
        &self,
        _req: &EventHistoryRequest,
    ) -> Result<EventHistoryResponse, StorageError> {
        Ok(EventHistoryResponse {
            events: Vec::new(),
            seq_high_water: None,
        })
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

    async fn load_a2p_abstractions(
        &self,
        _owner: &Owner,
        _sidecars: &[proxima_core::operators::SidecarSpec],
        limit: usize,
    ) -> Result<Vec<AbstractionRow>, StorageError> {
        Ok(self.abstractions.iter().take(limit).cloned().collect())
    }

    async fn consolidate_a2p(
        &self,
        req: &ConsolidateA2PRequest<'_>,
    ) -> Result<ConsolidateA2POutcome, StorageError> {
        self.consolidate_a2p_calls.fetch_add(1, Ordering::SeqCst);
        *self.last_a2p_model_id.lock().expect("poisoned") = Some(req.model_id.to_string());
        Ok(ConsolidateA2POutcome {
            perspective_ids: req
                .perspectives
                .iter()
                .map(|_| MemoryId::new(Uuid::now_v7()))
                .collect(),
            already_consolidated: false,
        })
    }

    async fn has_a2p_invocation(
        &self,
        _owner: &Owner,
        _key: &A2PInvocationKey<'_>,
    ) -> Result<bool, StorageError> {
        Ok(self.has_a2p_invocation)
    }

    async fn lookup_prior_a2p_head(
        &self,
        _owner: &Owner,
        _key: &A2PLineageKey<'_>,
    ) -> Result<Option<MemoryId>, StorageError> {
        Ok(self.prior_a2p_head)
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
struct StaticPerspectiveOp {
    outputs: Vec<NewPerspective>,
}

#[async_trait::async_trait]
impl A2POperator for StaticPerspectiveOp {
    fn operator_id(&self) -> &'static str {
        "test/a2p"
    }

    fn output_schema_id(&self) -> &'static str {
        TestPerspective::SCHEMA_ID
    }

    fn output_schema_version(&self) -> u32 {
        TestPerspective::SCHEMA_VERSION
    }

    fn prompt_version(&self) -> &'static str {
        "v1"
    }

    fn consumes(&self, schema_id: &SchemaId) -> bool {
        schema_id.as_str() == TestAbs::SCHEMA_ID
    }

    fn context(&self) -> A2PContextSpec {
        A2PContextSpec {
            kind: "on_ingest".into(),
            key: "test/a2p".into(),
            label: "test A2P".into(),
        }
    }

    async fn run(&self, _ctx: A2PContext<'_>) -> Result<Vec<NewPerspective>, OperatorError> {
        Ok(self.outputs.clone())
    }
}

#[derive(Debug)]
struct DeepPerspectiveOp {
    outputs: Vec<NewPerspective>,
}

#[async_trait::async_trait]
impl A2POperator for DeepPerspectiveOp {
    fn operator_id(&self) -> &'static str {
        "test/deep-a2p"
    }

    fn output_schema_id(&self) -> &'static str {
        TestPerspective::SCHEMA_ID
    }

    fn output_schema_version(&self) -> u32 {
        TestPerspective::SCHEMA_VERSION
    }

    fn prompt_version(&self) -> &'static str {
        "v1"
    }

    fn consumes(&self, schema_id: &SchemaId) -> bool {
        schema_id.as_str() == TestAbs::SCHEMA_ID
    }

    fn context(&self) -> A2PContextSpec {
        A2PContextSpec {
            kind: "on_ingest".into(),
            key: "test/deep-a2p".into(),
            label: "test Deep A2P".into(),
        }
    }

    async fn run(&self, _ctx: A2PContext<'_>) -> Result<Vec<NewPerspective>, OperatorError> {
        Ok(self.outputs.clone())
    }

    fn tier(&self) -> ModelTier {
        ModelTier::Deep
    }
}

#[derive(Debug)]
struct TestPersonality;

#[async_trait::async_trait]
impl PersonalityFlavor for TestPersonality {
    fn personality_id(&self) -> &'static str {
        "test/personality"
    }

    async fn snapshot(
        &self,
        _ctx: &PersonalityContext<'_>,
    ) -> Result<PersonalitySnapshot, proxima_core::error::ProtocolError> {
        Ok(PersonalitySnapshot {
            personality_id: PersonalityId::new("test/personality"),
            state_hash: PersonalityStateHash::new([7u8; 32]),
            captured_at: OffsetDateTime::now_utc(),
        })
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
struct NamedLlm(&'static str);

#[async_trait::async_trait]
impl LlmClient for NamedLlm {
    async fn complete_json(
        &self,
        _system_prompt: &str,
        _user_prompt: &str,
    ) -> Result<serde_json::Value, OperatorError> {
        Ok(serde_json::json!({}))
    }

    fn model_id(&self) -> &str {
        self.0
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

fn a2p_engine_with_output(
    principal: Principal,
    owner: Owner,
    source_abs: MemoryId,
    output: NewPerspective,
) -> (Engine, Arc<FakeStorage>) {
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<TestAbs>();
    registry.add_perspective_schema::<TestPerspective>();
    registry.add_personality(TestPersonality);

    let storage = Arc::new(FakeStorage::with_abstractions(vec![AbstractionRow {
        memory_id: source_abs,
        schema_id: TestAbs::schema_id(),
        schema_version: SchemaVersion::new(TestAbs::SCHEMA_VERSION),
        text: "source abstraction".into(),
        payload_json: serde_json::json!({ "summary": "source" }),
    }]));

    let mut ops = OperatorRegistry::new();
    ops.register_a2p(StaticPerspectiveOp {
        outputs: vec![output],
    });

    let engine = Engine::new(
        registry.freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(principal, owner)),
    )
    .with_storage(storage.clone())
    .with_operators(ops)
    .with_llm(Arc::new(FakeLlm))
    .with_embed(Arc::new(FakeEmbed));

    (engine, storage)
}

fn valid_perspective_with(
    schema_id: SchemaId,
    schema_version: SchemaVersion,
    typed_payload: serde_json::Value,
    provenance: Vec<MemoryId>,
) -> NewPerspective {
    NewPerspective {
        schema_id,
        schema_version,
        text: "development view".into(),
        typed_payload,
        provenance,
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

#[tokio::test]
async fn a2p_rejects_output_schema_mismatch_before_storage_write() {
    let (principal, owner) = fresh_owner();
    let source_abs = MemoryId::new(Uuid::now_v7());
    let output = valid_perspective_with(
        SchemaId::new("test/wrong-perspective".into()),
        SchemaVersion::new(TestPerspective::SCHEMA_VERSION),
        serde_json::json!({ "summary": "ok" }),
        vec![source_abs],
    );
    let (engine, storage) = a2p_engine_with_output(principal, owner.clone(), source_abs, output);

    let err = engine
        .run_pending_a2p(&owner)
        .await
        .expect_err("schema mismatch must be rejected");

    assert_eq!(err.code, ErrorCode::Internal);
    assert!(
        err.message
            .contains("returned schema test/wrong-perspective v1")
    );
    assert_eq!(storage.consolidate_a2p_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a2p_rejects_invalid_output_payload_before_storage_write() {
    let (principal, owner) = fresh_owner();
    let source_abs = MemoryId::new(Uuid::now_v7());
    let output = valid_perspective_with(
        TestPerspective::schema_id(),
        SchemaVersion::new(TestPerspective::SCHEMA_VERSION),
        serde_json::json!({ "summary": 42 }),
        vec![source_abs],
    );
    let (engine, storage) = a2p_engine_with_output(principal, owner.clone(), source_abs, output);

    let err = engine
        .run_pending_a2p(&owner)
        .await
        .expect_err("invalid typed payload must be rejected");

    assert_eq!(err.code, ErrorCode::Internal);
    assert!(
        err.message
            .contains("returned invalid test/perspective v1 payload")
    );
    assert_eq!(storage.consolidate_a2p_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a2p_rejects_provenance_outside_input_before_storage_write() {
    let (principal, owner) = fresh_owner();
    let source_abs = MemoryId::new(Uuid::now_v7());
    let output = valid_perspective_with(
        TestPerspective::schema_id(),
        SchemaVersion::new(TestPerspective::SCHEMA_VERSION),
        serde_json::json!({ "summary": "ok" }),
        vec![MemoryId::new(Uuid::now_v7())],
    );
    let (engine, storage) = a2p_engine_with_output(principal, owner.clone(), source_abs, output);

    let err = engine
        .run_pending_a2p(&owner)
        .await
        .expect_err("foreign provenance must be rejected");

    assert_eq!(err.code, ErrorCode::Internal);
    assert!(err.message.contains("not in A2P input"));
    assert_eq!(storage.consolidate_a2p_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a2p_persists_valid_perspective() {
    let (principal, owner) = fresh_owner();
    let source_abs = MemoryId::new(Uuid::now_v7());
    let output = valid_perspective_with(
        TestPerspective::schema_id(),
        SchemaVersion::new(TestPerspective::SCHEMA_VERSION),
        serde_json::json!({ "summary": "ok" }),
        vec![source_abs],
    );
    let (engine, storage) = a2p_engine_with_output(principal, owner.clone(), source_abs, output);

    let consolidated = engine
        .run_pending_a2p(&owner)
        .await
        .expect("valid A2P output must persist");

    assert_eq!(consolidated.len(), 1);
    assert_eq!(consolidated[0].0, "test/a2p");
    assert_eq!(consolidated[0].1.len(), 1);
    assert_eq!(storage.consolidate_a2p_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a2p_uses_llm_bound_to_operator_tier() {
    let (principal, owner) = fresh_owner();
    let source_abs = MemoryId::new(Uuid::now_v7());
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<TestAbs>();
    registry.add_perspective_schema::<TestPerspective>();
    registry.add_personality(TestPersonality);
    let storage = Arc::new(FakeStorage::with_abstractions(vec![AbstractionRow {
        memory_id: source_abs,
        schema_id: TestAbs::schema_id(),
        schema_version: SchemaVersion::new(TestAbs::SCHEMA_VERSION),
        text: "source abstraction".into(),
        payload_json: serde_json::json!({ "summary": "source" }),
    }]));
    let mut ops = OperatorRegistry::new();
    ops.register_a2p(DeepPerspectiveOp {
        outputs: vec![valid_perspective_with(
            TestPerspective::schema_id(),
            SchemaVersion::new(TestPerspective::SCHEMA_VERSION),
            serde_json::json!({ "summary": "ok" }),
            vec![source_abs],
        )],
    });
    let engine = Engine::new(
        registry.freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(principal, owner.clone())),
    )
    .with_storage(storage.clone())
    .with_operators(ops)
    .with_llm_for_tier(ModelTier::Standard, Arc::new(NamedLlm("standard-model")))
    .with_llm_for_tier(ModelTier::Deep, Arc::new(NamedLlm("deep-model")))
    .with_embed(Arc::new(FakeEmbed));

    let consolidated = engine
        .run_pending_a2p(&owner)
        .await
        .expect("valid Deep A2P output must persist");

    assert_eq!(consolidated.len(), 1);
    assert_eq!(
        storage
            .last_a2p_model_id
            .lock()
            .expect("poisoned")
            .as_deref(),
        Some("deep-model")
    );
}

#[tokio::test]
async fn a2p_skips_existing_invocation_before_operator_run() {
    let (principal, owner) = fresh_owner();
    let source_abs = MemoryId::new(Uuid::now_v7());
    let mut registry = FlavorRegistry::new();
    registry.add_abstraction_schema::<TestAbs>();
    registry.add_perspective_schema::<TestPerspective>();
    registry.add_personality(TestPersonality);
    let storage = Arc::new(
        FakeStorage::with_abstractions(vec![AbstractionRow {
            memory_id: source_abs,
            schema_id: TestAbs::schema_id(),
            schema_version: SchemaVersion::new(TestAbs::SCHEMA_VERSION),
            text: "source abstraction".into(),
            payload_json: serde_json::json!({ "summary": "source" }),
        }])
        .with_existing_a2p_invocation(),
    );
    let mut ops = OperatorRegistry::new();
    ops.register_a2p(StaticPerspectiveOp {
        outputs: vec![valid_perspective_with(
            TestPerspective::schema_id(),
            SchemaVersion::new(TestPerspective::SCHEMA_VERSION),
            serde_json::json!({ "summary": "ok" }),
            vec![source_abs],
        )],
    });
    let engine = Engine::new(
        registry.freeze(),
        MemoryStore::new(),
        Box::new(NoAuth::new(principal, owner.clone())),
    )
    .with_storage(storage.clone())
    .with_operators(ops)
    .with_llm(Arc::new(FakeLlm))
    .with_embed(Arc::new(FakeEmbed));

    let consolidated = engine
        .run_pending_a2p(&owner)
        .await
        .expect("existing invocation is a no-op");

    assert!(consolidated.is_empty());
    assert_eq!(storage.consolidate_a2p_calls.load(Ordering::SeqCst), 0);
}

#[test]
fn operator_registry_tracks_a2p_operators() {
    let mut registry = OperatorRegistry::new();
    registry.register_a2p(StaticPerspectiveOp {
        outputs: Vec::new(),
    });
    assert_eq!(registry.a2p_operators().len(), 1);
    assert!(registry.f2a_operators().is_empty());
}
