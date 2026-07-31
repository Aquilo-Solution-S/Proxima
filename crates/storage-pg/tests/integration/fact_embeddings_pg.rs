//! Fact render-text and embedding coverage. Compiled by default; live
//! PG execution is left to the orchestrator.

use proxima_core::storage_ports::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use proxima_core::access::AccessError;
use proxima_core::llm::{EMBEDDING_DIM, EmbeddingClient, LlmError};
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AuthzContext, ComplianceEraseTarget, EntityKind, FactPayload, FlavorRegistry, Owner,
    PayloadKeyBuilder, SourceBatchId,
};
use proxima_storage_pg::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
};
use uuid::Uuid;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct TestFactV1 {
    label: String,
}

impl FactPayload for TestFactV1 {
    const SCHEMA_ID: &'static str = "proxima-test/fact-embedding-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("label", &self.label);
        key.finish()
    }

    fn render(&self) -> String {
        self.label.clone()
    }
}

/// A Fact schema that declines a vector. Identical to [`TestFactV1`] in
/// every respect that could affect embedding except the declaration, so a
/// test pairing them isolates exactly the flag.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct DeclinesVectorV1 {
    label: String,
}

impl FactPayload for DeclinesVectorV1 {
    const SCHEMA_ID: &'static str = "proxima-test/declines-vector-v1";
    const SCHEMA_VERSION: u32 = 1;
    const EMBEDDABLE: bool = false;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("label", &self.label);
        key.finish()
    }

    fn render(&self) -> String {
        self.label.clone()
    }
}

fn declining_draft(label: &str) -> FactWriteCommand {
    FactWriteCommand::from_payload(
        "proxima-test/fact-embedding",
        SourceBatchId::new(Uuid::now_v7()),
        &DeclinesVectorV1 {
            label: label.to_string(),
        },
        time::OffsetDateTime::now_utc(),
    )
}

#[derive(Debug)]
struct FailingEmbedding;

#[async_trait::async_trait]
impl EmbeddingClient for FailingEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Err(LlmError::Embed("forced embedding failure".into()))
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

#[derive(Debug)]
struct SequenceEmbedding {
    values: Mutex<VecDeque<Vec<f32>>>,
}

impl SequenceEmbedding {
    fn new(values: Vec<Vec<f32>>) -> Self {
        Self {
            values: Mutex::new(values.into()),
        }
    }
}

#[async_trait::async_trait]
impl EmbeddingClient for SequenceEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        let mut values = self
            .values
            .lock()
            .expect("sequence embedding mutex poisoned");
        Ok(values
            .pop_front()
            .unwrap_or_else(|| vec![0.0; EMBEDDING_DIM]))
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

/// Batch call always rejected as permanent, per-item calls transiently
/// failing: forces the drain's per-item isolation fallback so each pass
/// records exactly one ordinary attempt per job — the path that walks a
/// job to the attempt cap under the batched drain.
#[derive(Debug)]
struct PoisonBatchTransientItemEmbedding;

#[async_trait::async_trait]
impl EmbeddingClient for PoisonBatchTransientItemEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Err(LlmError::Embed("forced embedding failure".into()))
    }

    async fn embed_many(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        Err(LlmError::EmbedPermanent("some input rejected".into()))
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

/// Rejects any text containing `poison` as permanent (both batch and
/// per-item), embeds everything else. Mirrors a provider's token-limit
/// 400: the batch call cannot say which input is at fault.
#[derive(Debug)]
struct PoisonTextEmbedding;

#[async_trait::async_trait]
impl EmbeddingClient for PoisonTextEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        if text.contains("poison") {
            Err(LlmError::EmbedPermanent("input exceeds token limit".into()))
        } else {
            Ok(vec![0.5; EMBEDDING_DIM])
        }
    }

    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        if texts.iter().any(|text| text.contains("poison")) {
            return Err(LlmError::EmbedPermanent("input exceeds token limit".into()));
        }
        Ok(texts.iter().map(|_| vec![0.5; EMBEDDING_DIM]).collect())
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

/// A provider that *dies* on one input rather than rejecting it: the batch
/// call fails **transiently**, and so does the per-item call for that one
/// text, while everything else — including a trivial probe — succeeds.
///
/// This is a local model runner crashing on a pathological input, observed
/// with a scanned page whose OCR hallucinated a 300-row CJK table: ollama
/// answers `400 {"error": "… EOF"}`, which is correctly classified
/// transient because nothing looked at the input. Indistinguishable from an
/// outage by the response alone, and the difference is what decides whether
/// the batch's other 31 jobs make progress.
#[derive(Debug)]
struct CrashOnInputEmbedding;

#[async_trait::async_trait]
impl EmbeddingClient for CrashOnInputEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        if text.contains("poison") {
            Err(LlmError::Embed("runner process no longer running".into()))
        } else {
            Ok(vec![0.25; EMBEDDING_DIM])
        }
    }

    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        if texts.iter().any(|text| text.contains("poison")) {
            return Err(LlmError::Embed("runner process no longer running".into()));
        }
        Ok(texts.iter().map(|_| vec![0.25; EMBEDDING_DIM]).collect())
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

/// Mirrors a provider token cap: rejects any input over `CAP` bytes as
/// permanent (batch and per-item), embeds shorter inputs. Exercises the
/// chunked-embedding rescue — an over-limit memory should end up with
/// one embedding version of multiple chunk rows instead of a terminal
/// job.
#[derive(Debug)]
struct TokenCapEmbedding;

const TOKEN_CAP_BYTES: usize = 3000;

#[async_trait::async_trait]
impl EmbeddingClient for TokenCapEmbedding {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
        if text.len() > TOKEN_CAP_BYTES {
            Err(LlmError::EmbedPermanent("input exceeds token limit".into()))
        } else {
            Ok(vec![0.5; EMBEDDING_DIM])
        }
    }

    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        if texts.iter().any(|text| text.len() > TOKEN_CAP_BYTES) {
            return Err(LlmError::EmbedPermanent("input exceeds token limit".into()));
        }
        Ok(texts.iter().map(|_| vec![0.5; EMBEDDING_DIM]).collect())
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

/// Counts provider calls so tests can assert the drain batches texts into
/// one request instead of one request per memory.
#[derive(Debug, Default)]
struct CountingBatchEmbedding {
    batch_calls: Mutex<Vec<usize>>,
    single_calls: Mutex<usize>,
}

#[async_trait::async_trait]
impl EmbeddingClient for CountingBatchEmbedding {
    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        *self.single_calls.lock().expect("counter mutex") += 1;
        Ok(vec![0.5; EMBEDDING_DIM])
    }

    async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
        self.batch_calls
            .lock()
            .expect("counter mutex")
            .push(texts.len());
        Ok(texts.iter().map(|_| vec![0.5; EMBEDDING_DIM]).collect())
    }

    fn model_id(&self) -> &'static str {
        "stub-fact-embed"
    }

    fn dim(&self) -> usize {
        EMBEDDING_DIM
    }
}

#[derive(Debug)]
struct AllowComplianceAdmin;

#[async_trait::async_trait]
impl ComplianceAdminPort for AllowComplianceAdmin {
    async fn may_perform_compliance_erase(
        &self,
        _authz: &AuthzContext,
        _target: &ComplianceEraseTarget,
    ) -> Result<bool, AccessError> {
        Ok(true)
    }
}

fn engine_for(
    pg: proxima_storage_pg::PgStorage,
    embed: Option<Arc<dyn EmbeddingClient>>,
) -> proxima_core::Engine {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema_or_panic_for_tests::<TestFactV1>();
    registry.add_fact_schema_or_panic_for_tests::<DeclinesVectorV1>();
    let engine = proxima_core::Engine::new(registry.freeze_or_panic_for_tests())
        .with_storage_ports(Arc::new(pg).storage_ports());
    if let Some(embed) = embed {
        engine.with_embed(embed)
    } else {
        engine
    }
}

fn compliance_engine_for(pg: proxima_storage_pg::PgStorage) -> proxima_core::Engine {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema_or_panic_for_tests::<TestFactV1>();
    let pg = Arc::new(pg);
    let ports = StoragePorts::builder()
        .fact_ingest(pg.clone())
        .mcp_call_write(pg.clone())
        .mcp_call_read(pg.clone())
        .memory_authoring(pg.clone())
        .memory_read(pg.clone())
        .memory_inspect(pg.clone())
        .embedding_text(pg.clone())
        .embedding_write(pg.clone())
        .embedding_job(pg.clone())
        .embedding_maintenance(pg.clone())
        .goal_write(pg.clone())
        .goal_read(pg.clone())
        .goal_wake_candidate(pg.clone())
        .change_event(pg.clone())
        .edge_read(pg.clone())
        .citation(pg.clone())
        .owner_access_read(pg.clone())
        .owner_membership_admin(pg.clone())
        .owner_transfer(pg.clone())
        .source_batch(pg.clone())
        .source_cursor(pg.clone())
        .fact_retention(pg.clone())
        .compliance_erase(pg.clone())
        .compliance_admin(Arc::new(AllowComplianceAdmin))
        .registry_projection(pg)
        .build();
    proxima_core::Engine::new(registry.freeze_or_panic_for_tests()).with_storage_ports(ports)
}

fn fact_draft(_owner: &Owner, label: &str) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    let payload = TestFactV1 {
        label: label.to_string(),
    };
    FactWriteCommand::from_payload(
        "proxima-test/fact-embedding",
        SourceBatchId::new(Uuid::now_v7()),
        &payload,
        now,
    )
}

fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
    let mut embedding = vec![0.0; EMBEDDING_DIM];
    embedding[..prefix.len()].copy_from_slice(&prefix);
    embedding
}

async fn count_fact_embeddings(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embeddings
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
            AND model_id = 'stub-fact-embed'",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn count_embedding_jobs(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embedding_jobs
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
            AND model_id = 'stub-fact-embed'",
    )
    .bind(memory_id.into_inner())
    .fetch_one(pool)
    .await
}

async fn count_embedding_jobs_for_model(
    pool: &sqlx::PgPool,
    model_id: &str,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.embedding_jobs
          WHERE model_id = $1",
    )
    .bind(model_id)
    .fetch_one(pool)
    .await
}

async fn load_embedding_job(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<Option<(String, i32, Option<String>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT status::text, attempts, last_error
           FROM proxima_core.embedding_jobs
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
            AND model_id = 'stub-fact-embed'",
    )
    .bind(memory_id.into_inner())
    .fetch_optional(pool)
    .await
}

/// Simulate the retry backoff window elapsing so a `pending` job becomes
/// claimable again without waiting real time.
async fn clear_embedding_backoff(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE proxima_core.embedding_jobs
            SET next_attempt_at = NULL
          WHERE entity_kind = 'Fact'
            AND entity_id = $1
            AND model_id = 'stub-fact-embed'",
    )
    .bind(memory_id.into_inner())
    .execute(pool)
    .await?;
    Ok(())
}

async fn load_memory_text(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT text FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pool)
        .await
}

async fn load_memory_created_at(
    pool: &sqlx::PgPool,
    memory_id: proxima_core::MemoryId,
) -> Result<time::OffsetDateTime, sqlx::Error> {
    sqlx::query_scalar("SELECT created_at FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pool)
        .await
}

async fn load_embedding_versions(
    pool: &sqlx::PgPool,
    entity_kind: EntityKind,
    entity_id: Uuid,
    model_id: &str,
) -> Result<Vec<i32>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT embedding_version
           FROM proxima_core.embeddings
          WHERE entity_kind = $1
            AND entity_id = $2
            AND model_id = $3
          ORDER BY embedding_version",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .fetch_all(pool)
    .await
}

async fn load_embedding_head_version(
    pool: &sqlx::PgPool,
    entity_kind: EntityKind,
    entity_id: Uuid,
    model_id: &str,
) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT embedding_version
           FROM proxima_core.embedding_heads
          WHERE entity_kind = $1
            AND entity_id = $2
            AND model_id = $3",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .fetch_optional(pool)
    .await
}

async fn load_embedding_vec_text(
    pool: &sqlx::PgPool,
    entity_kind: EntityKind,
    entity_id: Uuid,
    model_id: &str,
    version: i32,
) -> Result<String, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT vec::text
           FROM proxima_core.embeddings
          WHERE entity_kind = $1
            AND entity_id = $2
            AND model_id = $3
            AND embedding_version = $4",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .bind(version)
    .fetch_one(pool)
    .await
}

fn vector_literal(vec: &[f32]) -> String {
    let mut out = String::with_capacity(vec.len().saturating_mul(8).saturating_add(2));
    out.push('[');
    for (idx, value) in vec.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&value.to_string());
    }
    out.push(']');
    out
}

/// Seed one embedding row plus its head row directly. The proof-gated write
/// path is exercised by the in-crate `verbs::fact_embeddings` tests; external
/// tests only need pre-existing rows as fixtures.
async fn seed_embedding_row_with_head(
    pool: &sqlx::PgPool,
    owner: &Owner,
    entity_kind: EntityKind,
    memory_id: proxima_core::MemoryId,
    model_id: &str,
    vec: &[f32],
) -> Result<(), sqlx::Error> {
    let (owner_kind, owner_id) = owner.columns();
    let literal = vector_literal(vec);
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_kind, owner_id)
         VALUES ($1, $2, 1, $3, $4::vector, $5, $6)",
    )
    .bind(entity_kind)
    .bind(memory_id.into_inner())
    .bind(model_id)
    .bind(&literal)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_kind, entity_id, model_id, embedding_version,
             owner_kind, owner_id)
         VALUES ($1, $2, $3, 1, $4, $5)",
    )
    .bind(entity_kind)
    .bind(memory_id.into_inner())
    .bind(model_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await?;
    Ok(())
}

async fn reconcile_stub_fact_embeddings(
    pg: &proxima_storage_pg::PgStorage,
    scope: EmbeddingReconcileScope,
) -> Result<EmbeddingReconcileOutcome, proxima_core::StorageError> {
    pg.reconcile_embeddings(EmbeddingReconcileOptions {
        non_embeddable_schemas: &[],
        model_id: "stub-fact-embed",
        scope,
        limit: None,
    })
    .await
}

mod claims;
mod counts;
mod drain;
mod failures;
mod lifecycle;
mod reconcile;
