//! Embedding lifecycle compliance proofs.

use std::sync::Arc;

use crate::common::{
    drop_db, engine_with_registry, fresh_pg, owner_write_permit, storage_ports_with_compliance,
};
use proxima_core::access::{AccessError, Role};
use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::storage_ports::{ComplianceAdminPort, FactIngestPort};
use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
use proxima_core::verbs::schema::{FlavorRegistryFrozen, PayloadKind, SchemaInfo};
use proxima_core::{
    AccessKind, AuthPath, AuthzContext, ComplianceEraseOutcome, ComplianceEraseRefusal,
    ComplianceEraseTarget, EntityKind, ErrorCode, GoalId, GroupId, MemoryId, OwnerRef, SchemaId,
    SchemaVersion, SourceBatchId, SourceId, UserId,
};
use proxima_storage_pg::PgStorage;
use uuid::Uuid;

const MODEL_ID: &str = "stub-embedding-lifecycle";
const SCHEMA_ID: &str = "test/embedding-lifecycle-fact";

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

    async fn may_perform_operator_maintenance(
        &self,
        _authz: &AuthzContext,
    ) -> Result<bool, AccessError> {
        Ok(true)
    }
}

#[derive(Debug)]
struct DenyOperatorMaintenanceAdmin;

#[async_trait::async_trait]
impl ComplianceAdminPort for DenyOperatorMaintenanceAdmin {
    async fn may_perform_compliance_erase(
        &self,
        _authz: &AuthzContext,
        _target: &ComplianceEraseTarget,
    ) -> Result<bool, AccessError> {
        Ok(true)
    }

    async fn may_perform_operator_maintenance(
        &self,
        _authz: &AuthzContext,
    ) -> Result<bool, AccessError> {
        Ok(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EmbeddingInfraCounts {
    embeddings: i64,
    heads: i64,
    jobs: i64,
}

fn embedding_registry() -> FlavorRegistryFrozen {
    FlavorRegistryFrozen::with_schemas(vec![SchemaInfo {
        schema_id: SchemaId::new(SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Fact,
        filter_keys: vec![],
        sidecar_table: None,
        natural_key_columns: vec![],
        tombstone: None,
        has_typed_ingress: false,
        cited_object_schema: None,
        embeddable: true,
    }])
}

fn compliance_engine(pg: &PgStorage) -> proxima_core::Engine {
    proxima_core::Engine::new(embedding_registry()).with_storage_ports(
        storage_ports_with_compliance(pg, Arc::new(AllowComplianceAdmin)),
    )
}

fn engine_without_operator_maintenance(pg: &PgStorage) -> proxima_core::Engine {
    engine_with_registry(pg, embedding_registry())
}

fn compliance_without_operator_engine(pg: &PgStorage) -> proxima_core::Engine {
    proxima_core::Engine::new(embedding_registry()).with_storage_ports(
        storage_ports_with_compliance(pg, Arc::new(DenyOperatorMaintenanceAdmin)),
    )
}

fn admin_authz_for(owner: OwnerRef) -> AuthzContext {
    match owner {
        OwnerRef::Group(_) => AuthzContext::for_subject_with_role(
            UserId::new(Uuid::now_v7()),
            [(owner, Role::admin())],
            AuthPath::HostBearer,
        ),
        OwnerRef::Personal(user_id) => AuthzContext::for_subject(user_id, AuthPath::HostBearer),
        OwnerRef::World => AuthzContext::denied(),
    }
}

fn receipt_draft(source_id: &str, payload: &[u8]) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new(SCHEMA_ID.into()),
        schema_version: SchemaVersion::new(1),
        payload: payload.to_vec(),
        rendered_text: Some(String::from_utf8_lossy(payload).to_string()),
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new(source_id),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        derived_from: None,
    }
}

async fn seed_fact(
    pg: &PgStorage,
    owner: OwnerRef,
    source_id: &str,
    payload: &[u8],
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let permit = owner_write_permit(&owner, AccessKind::Fact).await?;
    Ok(pg
        .ingest_fact_atomic(&permit, &receipt_draft(source_id, payload), None)
        .await?
        .memory_id)
}

fn vector_literal(version: i32) -> String {
    let mut vec = vec![0.0_f32; EMBEDDING_DIM];
    vec[0] = if version == 1 { 1.0 } else { 2.0 };
    let mut out = String::with_capacity(vec.len().saturating_mul(4).saturating_add(2));
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

async fn seed_embedding_infra(
    pg: &PgStorage,
    owner: OwnerRef,
    memory_id: MemoryId,
) -> Result<EmbeddingInfraCounts, sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    for version in [1_i32, 2_i32] {
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_kind, entity_id, embedding_version, model_id, vec,
                 owner_kind, owner_id)
             VALUES ('Fact', $1, $2, $3, $4::vector, $5, $6)",
        )
        .bind(memory_id.into_inner())
        .bind(version)
        .bind(MODEL_ID)
        .bind(vector_literal(version))
        .bind(owner_kind)
        .bind(owner_id)
        .execute(pg.pool_for_tests())
        .await?;
    }
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_kind, entity_id, model_id, embedding_version, owner_kind, owner_id)
         VALUES ('Fact', $1, $2, 2, $3, $4)",
    )
    .bind(memory_id.into_inner())
    .bind(MODEL_ID)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    for (version, status) in [(3_i32, "pending"), (4_i32, "processing"), (5_i32, "failed")] {
        sqlx::query(
            "INSERT INTO proxima_core.embedding_jobs
                (owner_kind, owner_id, entity_kind, entity_id, model_id,
                 embedding_version, status, attempts, updated_at)
             VALUES ($1, $2, 'Fact', $3, $4, $5,
                     $6::proxima_core.embedding_job_status, 1, now())",
        )
        .bind(owner_kind)
        .bind(owner_id)
        .bind(memory_id.into_inner())
        .bind(MODEL_ID)
        .bind(version)
        .bind(status)
        .execute(pg.pool_for_tests())
        .await?;
    }
    embedding_infra_counts(pg, memory_id).await
}

async fn embedding_infra_counts(
    pg: &PgStorage,
    memory_id: MemoryId,
) -> Result<EmbeddingInfraCounts, sqlx::Error> {
    embedding_entity_infra_counts(pg, EntityKind::Fact, memory_id.into_inner()).await
}

async fn embedding_entity_infra_counts(
    pg: &PgStorage,
    entity_kind: EntityKind,
    entity_id: Uuid,
) -> Result<EmbeddingInfraCounts, sqlx::Error> {
    let (embeddings, heads, jobs): (i64, i64, i64) = sqlx::query_as(
        "SELECT
            (SELECT count(*)::bigint
               FROM proxima_core.embeddings
              WHERE entity_kind = $1 AND entity_id = $2),
            (SELECT count(*)::bigint
               FROM proxima_core.embedding_heads
              WHERE entity_kind = $1 AND entity_id = $2),
            (SELECT count(*)::bigint
               FROM proxima_core.embedding_jobs
              WHERE entity_kind = $1 AND entity_id = $2)",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    Ok(EmbeddingInfraCounts {
        embeddings,
        heads,
        jobs,
    })
}

async fn seed_orphan_embedding_infra(
    pg: &PgStorage,
    owner: OwnerRef,
) -> Result<MemoryId, sqlx::Error> {
    let orphan_id = MemoryId::new(Uuid::now_v7());
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_kind, owner_id)
         VALUES ('Fact', $1, 1, $2, $3::vector, $4, $5)",
    )
    .bind(orphan_id.into_inner())
    .bind(MODEL_ID)
    .bind(vector_literal(1))
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_kind, entity_id, model_id, embedding_version, owner_kind, owner_id)
         VALUES ('Fact', $1, $2, 1, $3, $4)",
    )
    .bind(orphan_id.into_inner())
    .bind(MODEL_ID)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs
            (owner_kind, owner_id, entity_kind, entity_id, model_id,
             embedding_version, status, attempts, updated_at)
         VALUES ($1, $2, 'Fact', $3, $4, 2,
                 'pending'::proxima_core.embedding_job_status, 0, now())",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(orphan_id.into_inner())
    .bind(MODEL_ID)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(orphan_id)
}

async fn seed_goal(pg: &PgStorage, owner: OwnerRef) -> Result<GoalId, sqlx::Error> {
    let goal_id = GoalId::new(Uuid::now_v7());
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    sqlx::query(
        "INSERT INTO proxima_core.goals
            (goal_id, owner_kind, owner_id, schema_id, schema_version,
             title, text, payload, state, authorship_kind, request_id,
             idempotency_key)
         VALUES ($1, $2, $3, 'test/embedding-lifecycle-goal', 1,
                 'Embedding lifecycle goal', 'Embedding lifecycle goal text', $4,
                 'Active', 'User', $5, $5)",
    )
    .bind(goal_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .bind(br#"{"goal":true}"#.to_vec())
    .bind(format!("embedding-lifecycle-goal:{}", goal_id.into_inner()))
    .execute(pg.pool_for_tests())
    .await?;
    Ok(goal_id)
}

async fn seed_goal_embedding_infra(
    pg: &PgStorage,
    owner: OwnerRef,
    goal_id: GoalId,
) -> Result<EmbeddingInfraCounts, sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(&owner);
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_kind, owner_id)
         VALUES ('Goal', $1, 1, $2, $3::vector, $4, $5)",
    )
    .bind(goal_id.into_inner())
    .bind(MODEL_ID)
    .bind(vector_literal(1))
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_kind, entity_id, model_id, embedding_version, owner_kind, owner_id)
         VALUES ('Goal', $1, $2, 1, $3, $4)",
    )
    .bind(goal_id.into_inner())
    .bind(MODEL_ID)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_jobs
            (owner_kind, owner_id, entity_kind, entity_id, model_id,
             embedding_version, status, attempts, updated_at)
         VALUES ($1, $2, 'Goal', $3, $4, 2,
                 'pending'::proxima_core.embedding_job_status, 0, now())",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(goal_id.into_inner())
    .bind(MODEL_ID)
    .execute(pg.pool_for_tests())
    .await?;
    embedding_entity_infra_counts(pg, EntityKind::Goal, goal_id.into_inner()).await
}

async fn memory_count(pg: &PgStorage, memory_id: MemoryId) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memories WHERE memory_id = $1")
        .bind(memory_id.into_inner())
        .fetch_one(pg.pool_for_tests())
        .await
}

#[tokio::test]
async fn embedding_lifecycle_owner_erase_removes_rows_at_commit()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let memory_id = seed_fact(&pg, owner, "embedding-life/owner", b"owner erase").await?;
        let seeded = seed_embedding_infra(&pg, owner, memory_id).await?;
        assert_eq!(
            seeded,
            EmbeddingInfraCounts {
                embeddings: 2,
                heads: 1,
                jobs: 3
            }
        );

        let engine = compliance_engine(&pg);
        let outcome = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        let ComplianceEraseOutcome::Completed { counts, .. } = outcome else {
            panic!("expected completed owner erase, got {outcome:?}");
        };
        assert_eq!(counts.embedding_jobs, 3);
        assert_eq!(counts.embeddings, 3, "embedding rows plus head row");
        assert_eq!(memory_count(&pg, memory_id).await?, 0);
        assert_eq!(
            embedding_infra_counts(&pg, memory_id).await?,
            EmbeddingInfraCounts {
                embeddings: 0,
                heads: 0,
                jobs: 0
            },
            "erase return implies transaction committed; no embedding infra row may survive"
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn embedding_ops_observability_is_operator_gated() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let memory_id = seed_fact(&pg, owner, "embedding-life/ops", b"ops stats").await?;
        seed_embedding_infra(&pg, owner, memory_id).await?;
        let operator_engine = compliance_engine(&pg);
        let ordinary_engine = engine_without_operator_maintenance(&pg);
        let compliance_deny_engine = compliance_without_operator_engine(&pg);
        let authz = admin_authz_for(owner);

        let denied = ordinary_engine
            .embedding_ann_observability(&authz)
            .await
            .expect_err("ordinary owner/admin authz is not operator maintenance authz");
        assert_eq!(denied.code, ErrorCode::Forbidden);
        let denied = compliance_deny_engine
            .embedding_ann_observability(&authz)
            .await
            .expect_err("present compliance admin can still deny operator maintenance");
        assert_eq!(denied.code, ErrorCode::Forbidden);

        let stats = operator_engine.embedding_ann_observability(&authz).await?;
        assert!(stats.embedding_rows >= 2);
        assert!(stats.embedding_head_rows >= 1);
        assert!(stats.embedding_job_rows >= 3);
        assert!(stats.embedding_table_bytes > 0);
        assert!(stats.embedding_total_relation_bytes >= stats.embedding_table_bytes);
        assert!(stats.hnsw_index_bytes > 0);
        assert!(stats.backlog.pending >= 1);
        assert!(stats.backlog.processing >= 1);
        assert!(stats.backlog.failed >= 1);
        let recall = stats
            .recall_canary
            .expect("current embedding head should produce recall canary");
        assert_eq!(recall.model_id, MODEL_ID);
        assert!(recall.exact_count > 0);
        assert!(recall.ann_count > 0);
        assert!((0.0..=1.0).contains(&recall.recall_at_k));
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn embedding_ops_orphan_sweep_is_operator_gated_and_not_lawful_wipe()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let valid_id = seed_fact(&pg, owner, "embedding-life/sweep", b"kept source").await?;
        seed_embedding_infra(&pg, owner, valid_id).await?;
        let orphan_id = seed_orphan_embedding_infra(&pg, owner).await?;
        let operator_engine = compliance_engine(&pg);
        let ordinary_engine = engine_without_operator_maintenance(&pg);
        let compliance_deny_engine = compliance_without_operator_engine(&pg);
        let authz = admin_authz_for(owner);

        let denied = ordinary_engine
            .sweep_orphan_embedding_rows(&authz)
            .await
            .expect_err("ordinary owner/admin authz is not operator maintenance authz");
        assert_eq!(denied.code, ErrorCode::Forbidden);
        let denied = compliance_deny_engine
            .sweep_orphan_embedding_rows(&authz)
            .await
            .expect_err("present compliance admin can still deny operator maintenance");
        assert_eq!(denied.code, ErrorCode::Forbidden);

        let before = operator_engine.embedding_ann_observability(&authz).await?;
        assert_eq!(before.orphan_rows.embeddings, 1);
        assert_eq!(before.orphan_rows.heads, 1);
        assert_eq!(before.orphan_rows.jobs, 1);

        let outcome = operator_engine.sweep_orphan_embedding_rows(&authz).await?;
        assert_eq!(outcome.embeddings_deleted, 1);
        assert_eq!(outcome.heads_deleted, 1);
        assert_eq!(outcome.jobs_deleted, 1);
        assert_eq!(
            embedding_infra_counts(&pg, valid_id).await?,
            EmbeddingInfraCounts {
                embeddings: 2,
                heads: 1,
                jobs: 3
            },
            "operator sweep must not touch embedding rows for live source facts"
        );
        assert_eq!(
            embedding_infra_counts(&pg, orphan_id).await?,
            EmbeddingInfraCounts {
                embeddings: 0,
                heads: 0,
                jobs: 0
            }
        );

        let second = operator_engine.sweep_orphan_embedding_rows(&authz).await?;
        assert_eq!(second.embeddings_deleted, 0);
        assert_eq!(second.heads_deleted, 0);
        assert_eq!(second.jobs_deleted, 0);
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn embedding_ops_orphan_sweep_handles_goal_sources() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let live_goal = seed_goal(&pg, owner).await?;
        let orphan_goal = GoalId::new(Uuid::now_v7());
        seed_goal_embedding_infra(&pg, owner, live_goal).await?;
        seed_goal_embedding_infra(&pg, owner, orphan_goal).await?;
        let engine = compliance_engine(&pg);
        let authz = admin_authz_for(owner);

        let before = engine.embedding_ann_observability(&authz).await?;
        assert_eq!(before.orphan_rows.embeddings, 1);
        assert_eq!(before.orphan_rows.heads, 1);
        assert_eq!(before.orphan_rows.jobs, 1);

        let outcome = engine.sweep_orphan_embedding_rows(&authz).await?;
        assert_eq!(outcome.embeddings_deleted, 1);
        assert_eq!(outcome.heads_deleted, 1);
        assert_eq!(outcome.jobs_deleted, 1);
        assert_eq!(
            embedding_entity_infra_counts(&pg, EntityKind::Goal, live_goal.into_inner()).await?,
            EmbeddingInfraCounts {
                embeddings: 1,
                heads: 1,
                jobs: 1
            },
            "operator sweep must preserve embedding rows for live Goal sources"
        );
        assert_eq!(
            embedding_entity_infra_counts(&pg, EntityKind::Goal, orphan_goal.into_inner()).await?,
            EmbeddingInfraCounts {
                embeddings: 0,
                heads: 0,
                jobs: 0
            }
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn embedding_lifecycle_source_scope_erases_only_selected_fact_rows()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let erased = seed_fact(&pg, owner, "embedding-life/source-a", b"erase source").await?;
        let kept = seed_fact(&pg, owner, "embedding-life/source-b", b"keep source").await?;
        let erased_counts = seed_embedding_infra(&pg, owner, erased).await?;
        let kept_counts = seed_embedding_infra(&pg, owner, kept).await?;

        let engine = compliance_engine(&pg);
        let outcome = engine
            .erase_abandoned_group_source_scope(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
                SourceId::new("embedding-life/source-a"),
            )
            .await?;
        assert!(matches!(outcome, ComplianceEraseOutcome::Completed { .. }));
        assert_eq!(memory_count(&pg, erased).await?, 0);
        assert_eq!(memory_count(&pg, kept).await?, 1);
        assert_eq!(
            embedding_infra_counts(&pg, erased).await?,
            EmbeddingInfraCounts {
                embeddings: 0,
                heads: 0,
                jobs: 0
            }
        );
        assert_eq!(
            embedding_infra_counts(&pg, kept).await?,
            kept_counts,
            "source-scope erase must not sweep embedding infra for surviving facts"
        );
        assert_eq!(
            erased_counts,
            EmbeddingInfraCounts {
                embeddings: 2,
                heads: 1,
                jobs: 3
            }
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

#[tokio::test]
async fn embedding_lifecycle_legal_hold_blocks_cascade() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let group = GroupId::new(Uuid::now_v7());
        let owner = OwnerRef::Group(group);
        let memory_id = seed_fact(&pg, owner, "embedding-life/hold", b"held fact").await?;
        let seeded = seed_embedding_infra(&pg, owner, memory_id).await?;
        let engine = compliance_engine(&pg);
        engine
            .set_legal_hold(&admin_authz_for(owner), &owner)
            .await?;

        let outcome = engine
            .erase_abandoned_group_owner(
                &AuthzContext::for_subject(UserId::new(Uuid::now_v7()), AuthPath::HostBearer),
                group,
            )
            .await?;
        assert!(matches!(
            outcome,
            ComplianceEraseOutcome::Refused {
                reason: ComplianceEraseRefusal::LegalHoldActive,
                ..
            }
        ));
        assert_eq!(memory_count(&pg, memory_id).await?, 1);
        assert_eq!(
            embedding_infra_counts(&pg, memory_id).await?,
            seeded,
            "legal hold refusal must happen before embedding cascade deletes"
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

/// `EMBEDDING_DIM` is the single source of truth for the vector width,
/// and every Rust check already references it. The one place that
/// cannot is the DDL: `0001_init.sql` declares `vec vector(1024)` as a
/// literal, because SQL has no way to see a Rust const. That makes the
/// migration a genuine second statement of the same fact, and a
/// divergence would be silent — the constant would be raised, every
/// in-process length check would happily accept the wider vector, and
/// Postgres would reject each insert at runtime with a type error.
///
/// Reading the width back out of the live column closes that loop. For
/// pgvector `atttypmod` *is* the declared dimension (no offset, unlike
/// varchar), which the assertion below relies on; `format_type` is
/// carried alongside so a failure reports `vector(N)` in the message
/// rather than a bare integer.
#[tokio::test]
async fn embeddings_column_width_matches_the_embedding_dim_constant()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let (typmod, rendered): (i32, String) = sqlx::query_as(
            "SELECT atttypmod, format_type(atttypid, atttypmod)
               FROM pg_attribute
              WHERE attrelid = 'proxima_core.embeddings'::regclass
                AND attname = 'vec'
                AND NOT attisdropped",
        )
        .fetch_one(pg.pool_for_tests())
        .await?;

        assert_eq!(
            usize::try_from(typmod).expect("a declared vector width is positive"),
            EMBEDDING_DIM,
            "proxima_core.embeddings.vec is {rendered} but EMBEDDING_DIM is \
             {EMBEDDING_DIM}; the migration and the constant must agree, and a \
             shipped migration is never edited — change the constant back, or \
             add a new migration that alters the column"
        );
        Ok(())
    }
    .await;

    drop_db(&db_name).await?;
    result
}
