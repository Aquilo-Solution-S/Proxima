//! Fact render-text and embedding coverage. Compiled by default; live
//! PG execution is left to the orchestrator.

use proxima_core::storage_ports::*;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

use proxima_core::access::AccessError;
use proxima_core::llm::{EMBEDDING_DIM, EMBEDDING_JOB_MAX_ATTEMPTS, EmbeddingClient, LlmError};
use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::{
    AuthPath, AuthzContext, ComplianceEraseOutcome, ComplianceEraseTarget, EntityKind, FactPayload,
    FlavorRegistry, GroupId, Owner, OwnerRef, PayloadKeyBuilder, SourceBatchId, UserId,
};
use proxima_storage_pg::{
    EmbeddingReconcileOptions, EmbeddingReconcileOutcome, EmbeddingReconcileScope,
};
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, owner_fixture, owner_write_permit};

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
        model_id: "stub-fact-embed",
        scope,
        limit: None,
    })
    .await
}

#[tokio::test]
async fn fact_ingest_with_embed_client_enqueues_pending_embedding_job_once()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        let draft = fact_draft(&owner, "rendered fact");
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                draft.clone(),
            )
            .await?;
        let replay = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                draft,
            )
            .await?;

        assert!(!outcome.idempotent_replay);
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, outcome.memory_id);
        assert_eq!(
            load_memory_text(pg.pool_for_tests(), outcome.memory_id).await?,
            Some("rendered fact".to_string()),
        );
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), outcome.memory_id).await?,
            1
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn embedding_tables_have_no_entity_foreign_keys() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let foreign_keys: Vec<(String, String, String)> = sqlx::query_as(
            "SELECT conrelid::regclass::text,
                    confrelid::regclass::text,
                    pg_get_constraintdef(oid)
               FROM pg_constraint
              WHERE contype = 'f'
                AND conrelid IN (
                    'proxima_core.embeddings'::regclass,
                    'proxima_core.embedding_heads'::regclass
                )
              ORDER BY conrelid::regclass::text, conname",
        )
        .fetch_all(pg.pool_for_tests())
        .await?;
        assert!(
            foreign_keys.is_empty(),
            "embedding infrastructure tables must not FK to entity rows: {foreign_keys:#?}"
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn drain_embedding_jobs_writes_embedding_and_deletes_job()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "drained fact"),
            )
            .await?;

        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), outcome.memory_id).await?,
            1
        );

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
            1
        );
        assert_eq!(
            load_embedding_head_version(
                pg.pool_for_tests(),
                EntityKind::Fact,
                outcome.memory_id.into_inner(),
                "stub-fact-embed",
            )
            .await?,
            Some(1)
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), outcome.memory_id).await?,
            0
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn reembedding_appends_version_and_advances_head_without_mutating_v1()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(SequenceEmbedding::new(vec![
                padded_embedding([1.0, 0.0, 0.0]),
                padded_embedding([0.0, 1.0, 0.0]),
            ]))),
        );
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "reembedded fact"),
            )
            .await?;
        let created_at = load_memory_created_at(pg.pool_for_tests(), outcome.memory_id).await?;

        let first_drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(first_drain.processed, 1);
        assert_eq!(first_drain.failed, 0);
        let v1_vec = load_embedding_vec_text(
            pg.pool_for_tests(),
            EntityKind::Fact,
            outcome.memory_id.into_inner(),
            "stub-fact-embed",
            1,
        )
        .await?;

        let reconcile =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::IncludeStale).await?;
        assert_eq!(reconcile.enqueued, 1);
        let second_drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(second_drain.processed, 1);
        assert_eq!(second_drain.failed, 0);

        assert_eq!(
            load_embedding_versions(
                pg.pool_for_tests(),
                EntityKind::Fact,
                outcome.memory_id.into_inner(),
                "stub-fact-embed",
            )
            .await?,
            vec![1, 2]
        );
        assert_eq!(
            load_embedding_head_version(
                pg.pool_for_tests(),
                EntityKind::Fact,
                outcome.memory_id.into_inner(),
                "stub-fact-embed",
            )
            .await?,
            Some(2)
        );
        assert_eq!(
            load_embedding_vec_text(
                pg.pool_for_tests(),
                EntityKind::Fact,
                outcome.memory_id.into_inner(),
                "stub-fact-embed",
                1,
            )
            .await?,
            v1_vec
        );
        assert_eq!(
            load_memory_created_at(pg.pool_for_tests(), outcome.memory_id).await?,
            created_at
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn compliance_erase_removes_embeddings_heads_and_jobs()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let permit = owner_write_permit(&owner, proxima_core::AccessKind::Fact).await?;
        let outcome = pg
            .ingest_fact_atomic(&permit, &fact_draft(&owner, "erase embedding rows"), None)
            .await?;
        seed_embedding_row_with_head(
            pg.pool_for_tests(),
            &owner,
            EntityKind::Fact,
            outcome.memory_id,
            "stub-fact-embed",
            &padded_embedding([1.0, 0.0, 0.0]),
        )
        .await?;
        let (owner_kind, owner_id) = owner.columns();
        sqlx::query(
            "INSERT INTO proxima_core.embedding_jobs
                (owner_kind, owner_id, entity_kind, entity_id, model_id, embedding_version)
             VALUES ($1, $2, 'Fact', $3, 'stub-fact-embed', 2)",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(outcome.memory_id.into_inner())
        .execute(pg.pool_for_tests())
        .await?;

        let compliance_engine = compliance_engine_for(pg.clone());
        let erase = compliance_engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = erase else {
            panic!("expected completed erase, got {erase:?}");
        };
        assert_eq!(counts.embedding_jobs, 1);
        assert_eq!(counts.embeddings, 2, "embedding row plus head row");

        let remaining_embeddings: i64 = sqlx::query_scalar(
            "SELECT
                 (SELECT count(*)::bigint FROM proxima_core.embeddings WHERE entity_id = $1)
               + (SELECT count(*)::bigint FROM proxima_core.embedding_heads WHERE entity_id = $1)
               + (SELECT count(*)::bigint FROM proxima_core.embedding_jobs WHERE entity_id = $1)",
        )
        .bind(outcome.memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await?;
        assert_eq!(remaining_embeddings, 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn failed_embedding_jobs_retry_until_attempt_cap() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(FailingEmbedding)));
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "failing fact"),
            )
            .await?;

        for attempt in 1..=EMBEDDING_JOB_MAX_ATTEMPTS {
            let drain = engine.drain_embedding_jobs(10).await?;
            assert_eq!(drain.processed, 1);
            assert_eq!(drain.failed, 1);
            let Some((status, attempts, last_error)) =
                load_embedding_job(pg.pool_for_tests(), outcome.memory_id).await?
            else {
                panic!("failed job must remain in embedding_jobs");
            };
            assert_eq!(attempts, attempt);
            assert_eq!(
                status,
                if attempt < EMBEDDING_JOB_MAX_ATTEMPTS {
                    "pending"
                } else {
                    "failed"
                }
            );
            assert!(
                last_error
                    .as_deref()
                    .is_some_and(|err| err.contains("forced embedding failure")),
                "last_error must preserve the embedding failure"
            );

            // P1.1 backoff: a pending retry is NOT immediately re-claimable, so
            // a second drain right away burns no attempt (the hot-loop that
            // previously spent all attempts in seconds). Only after the backoff
            // window elapses does the next attempt run.
            if attempt < EMBEDDING_JOB_MAX_ATTEMPTS {
                let immediate = engine.drain_embedding_jobs(10).await?;
                assert_eq!(
                    immediate.processed, 0,
                    "backoff must gate immediate re-claim of a pending retry"
                );
                clear_embedding_backoff(pg.pool_for_tests(), outcome.memory_id).await?;
            }
        }

        // `failed` is terminal: no drain reclaims it (requeue is reconcile-only).
        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 0);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
            0
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn reconcile_requeues_failed_embedding_jobs() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), Some(Arc::new(FailingEmbedding)));
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "reconcile fact"),
            )
            .await?;

        // Drive the job to the terminal `failed` state (backoff cleared between
        // attempts so we do not wait real time).
        for _ in 1..=EMBEDDING_JOB_MAX_ATTEMPTS {
            engine.drain_embedding_jobs(10).await?;
            clear_embedding_backoff(pg.pool_for_tests(), outcome.memory_id).await?;
        }
        let (status, attempts, _) = load_embedding_job(pg.pool_for_tests(), outcome.memory_id)
            .await?
            .expect("failed job must remain in embedding_jobs");
        assert_eq!(status, "failed");
        assert_eq!(attempts, EMBEDDING_JOB_MAX_ATTEMPTS);

        // Reconcile lifts the Fact out of the dead-end: status back to pending,
        // attempts reset, last_error cleared — so a fresh provider/model or a
        // process restart can retry it (analysis 2026-07-05 P1.1).
        let reconciled = pg
            .reconcile_embeddings(EmbeddingReconcileOptions {
                model_id: "stub-fact-embed",
                scope: EmbeddingReconcileScope::MissingOnly,
                limit: None,
            })
            .await?;
        assert_eq!(reconciled.enqueued, 1, "reconcile requeues the failed job");
        let (status, attempts, last_error) =
            load_embedding_job(pg.pool_for_tests(), outcome.memory_id)
                .await?
                .expect("job still present after requeue");
        assert_eq!(status, "pending");
        assert_eq!(attempts, 0);
        assert!(last_error.is_none(), "requeue clears last_error");
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn claimed_embedding_job_is_not_claimed_again() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "skip locked fact"),
            )
            .await?;

        let claims = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].entity_id, outcome.memory_id);
        assert_eq!(claims[0].attempts, 0);
        let second_claims = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert!(second_claims.is_empty());
        assert_eq!(
            load_embedding_job(pg.pool_for_tests(), outcome.memory_id)
                .await?
                .map(|(status, _, _)| status),
            Some("processing".to_string()),
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn stale_processing_embedding_job_is_reclaimed() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "stale processing fact"),
            )
            .await?;

        let claims = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].entity_id, outcome.memory_id);
        let second_claims = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert!(second_claims.is_empty());

        sqlx::query(
            "UPDATE proxima_core.embedding_jobs
                SET updated_at = now() - interval '20 minutes'
              WHERE entity_id = $1",
        )
        .bind(outcome.memory_id.into_inner())
        .execute(pg.pool_for_tests())
        .await?;

        let reclaimed = pg
            .claim_pending_embedding_jobs("stub-fact-embed", 1)
            .await?;
        assert_eq!(reclaimed.len(), 1);
        assert_eq!(reclaimed[0].entity_id, outcome.memory_id);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn fact_embedding_backfill_heals_no_client_ingest() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        let first = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "backfill fact one"),
            )
            .await?;
        let second = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "backfill fact two"),
            )
            .await?;

        assert_eq!(
            load_memory_text(pg.pool_for_tests(), first.memory_id).await?,
            Some("backfill fact one".to_string()),
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), first.memory_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), second.memory_id).await?,
            0
        );

        engine
            .set_embed_client(Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))))
            .await;
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        assert_eq!(engine.backfill_fact_embeddings(&authz, &owner, 1).await?, 1);
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), first.memory_id).await?
                + count_embedding_jobs(pg.pool_for_tests(), second.memory_id).await?,
            1
        );
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), first.memory_id).await?
                + count_fact_embeddings(pg.pool_for_tests(), second.memory_id).await?,
            0
        );

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), first.memory_id).await?
                + count_fact_embeddings(pg.pool_for_tests(), second.memory_id).await?,
            1
        );

        assert_eq!(
            engine.backfill_fact_embeddings(&authz, &owner, 10).await?,
            1
        );
        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), first.memory_id).await?
                + count_fact_embeddings(pg.pool_for_tests(), second.memory_id).await?,
            2
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), first.memory_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), second.memory_id).await?,
            0
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn reconcile_embeddings_enqueues_missing_facts_idempotently()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        for label in [
            "reconcile fact one",
            "reconcile fact two",
            "reconcile fact three",
        ] {
            engine
                .fact_ingest(
                    &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                    fact_draft(&owner, label),
                )
                .await?;
        }
        let stale = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "reconcile stale fact"),
            )
            .await?;
        let other_model_embedding = vec![0.125; EMBEDDING_DIM];
        seed_embedding_row_with_head(
            pg.pool_for_tests(),
            &owner,
            EntityKind::Fact,
            stale.memory_id,
            "other-model",
            &other_model_embedding,
        )
        .await?;

        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            0
        );
        let first =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::MissingOnly).await?;
        assert_eq!(first.scanned, 4);
        assert_eq!(first.enqueued, 4);
        assert_eq!(first.skipped, 0);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            4
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), stale.memory_id).await?,
            1
        );

        let second =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::MissingOnly).await?;
        assert_eq!(second.scanned, 0);
        assert_eq!(second.enqueued, 0);
        assert_eq!(second.skipped, 0);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            4
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), stale.memory_id).await?,
            1
        );

        let include_stale =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::IncludeStale).await?;
        assert_eq!(include_stale.scanned, 0);
        assert_eq!(include_stale.enqueued, 0);
        assert_eq!(include_stale.skipped, 0);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            4
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), stale.memory_id).await?,
            1
        );

        let include_stale_again =
            reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::IncludeStale).await?;
        assert_eq!(include_stale_again.scanned, 0);
        assert_eq!(include_stale_again.enqueued, 0);
        assert_eq!(include_stale_again.skipped, 0);
        assert_eq!(
            count_embedding_jobs_for_model(pg.pool_for_tests(), "stub-fact-embed").await?,
            4
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn reconcile_embedding_drain_writes_fact_embeddings() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "reconcile drain fact"),
            )
            .await?;
        reconcile_stub_fact_embeddings(&pg, EmbeddingReconcileScope::MissingOnly).await?;

        let client = ConstantEmbedding::prefixed("stub-fact-embed", &[0.25, 0.5, 0.75]);
        let drain = pg.drain_embedding_jobs_inline(&client, 10).await?;
        assert_eq!(drain.embedded, 1);
        assert_eq!(drain.failed, 0);
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
            1
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), outcome.memory_id).await?,
            0
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn count_pending_embedding_jobs_counts_outstanding() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let other_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        for label in ["pending count one", "pending count two"] {
            engine
                .fact_ingest(
                    &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                    fact_draft(&owner, label),
                )
                .await?;
        }
        engine
            .fact_ingest(
                &AuthzContext::single_owner(&other_owner, AuthPath::HostBearer),
                fact_draft(&other_owner, "other owner pending count"),
            )
            .await?;

        assert_eq!(pg.count_pending_embedding_jobs(&owner).await?, 2);
        assert_eq!(pg.count_pending_embedding_jobs(&other_owner).await?, 1);

        let drain = engine.drain_embedding_jobs(10).await?;
        assert_eq!(drain.processed, 3);
        assert_eq!(drain.failed, 0);
        assert_eq!(pg.count_pending_embedding_jobs(&owner).await?, 0);
        assert_eq!(pg.count_pending_embedding_jobs(&other_owner).await?, 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn fact_ingest_without_embed_client_still_succeeds() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        let outcome = engine
            .fact_ingest(
                &AuthzContext::single_owner(&owner, AuthPath::HostBearer),
                fact_draft(&owner, "no client fact"),
            )
            .await?;

        assert_eq!(
            load_memory_text(pg.pool_for_tests(), outcome.memory_id).await?,
            Some("no client fact".to_string()),
        );
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), outcome.memory_id).await?,
            0
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), outcome.memory_id).await?,
            0
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn reconcile_limit_skips_existing_heads_before_bounding()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let engine = engine_for(pg.clone(), None);
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let covered = engine
            .fact_ingest(&authz, fact_draft(&owner, "already covered"))
            .await?;
        let missing = engine
            .fact_ingest(&authz, fact_draft(&owner, "missing after covered"))
            .await?;
        seed_embedding_row_with_head(
            pg.pool_for_tests(),
            &owner,
            EntityKind::Fact,
            covered.memory_id,
            "stub-fact-embed",
            &vec![0.25; EMBEDDING_DIM],
        )
        .await?;

        let outcome = pg
            .reconcile_embeddings(EmbeddingReconcileOptions {
                model_id: "stub-fact-embed",
                scope: EmbeddingReconcileScope::MissingOnly,
                limit: Some(1),
            })
            .await?;
        assert_eq!(outcome.scanned, 1);
        assert_eq!(outcome.enqueued, 1);
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), missing.memory_id).await?,
            1
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}
