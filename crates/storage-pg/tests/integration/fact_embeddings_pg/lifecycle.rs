//! Embedding lifecycle alongside memory writes: ingest enqueueing, re-embedding versions, table independence, and erase.

use super::{
    SequenceEmbedding, compliance_engine_for, count_embedding_jobs, count_fact_embeddings,
    declining_draft, engine_for, fact_draft, load_embedding_head_version, load_embedding_vec_text,
    load_embedding_versions, load_memory_created_at, load_memory_text, padded_embedding,
    reconcile_stub_fact_embeddings, seed_embedding_row_with_head,
};

use proxima_core::storage_ports::*;
use std::sync::Arc;

use proxima_core::test_fixtures::ConstantEmbedding;
use proxima_core::{
    AuthPath, AuthzContext, ComplianceEraseOutcome, EntityKind, GroupId, OwnerRef, UserId,
};
use proxima_storage_pg::EmbeddingReconcileScope;
use uuid::Uuid;

use crate::common::{drop_db, fresh_pg, owner_fixture, owner_write_permit};

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

/// `Engine::fact_ingest` honours the schema's declaration.
///
/// The paired control is the point: both Facts are written by the same call
/// with the same embedder configured, and only the declaration differs, so
/// this fails if the gate is dropped rather than merely reporting that
/// nothing was embedded. `fact_ingest` computes
/// `embed_client().map(model_id)` — "is there an embedder" — which the
/// schema overrides; it was the one verb of four that did not consult
/// `vector_model_for`, because it does not share the `ingest_fact_*` name.
#[tokio::test]
async fn fact_ingest_does_not_embed_a_schema_that_declined()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );

        let declined = engine
            .fact_ingest(&authz, declining_draft("a fact that declined a vector"))
            .await?;
        let embedded = engine
            .fact_ingest(&authz, fact_draft(&owner, "a fact that wants one"))
            .await?;

        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), declined.memory_id).await?,
            0,
            "EMBEDDABLE = false still queued a vector through fact_ingest"
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), embedded.memory_id).await?,
            1,
            "the control must be queued, or this test proves only that \
             nothing is ever embedded"
        );

        // The row is written and readable either way: declining a vector is
        // not declining the Fact.
        assert_eq!(
            load_memory_text(pg.pool_for_tests(), declined.memory_id).await?,
            Some("a fact that declined a vector".to_string()),
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

/// Asking for one row's embedding by id does not override the schema.
///
/// This path holds a `MemoryId` and never passes through the job queue, so
/// none of the enqueue-side exclusions apply to it and it writes the vector
/// directly — no `embedding_jobs` row is ever created, which is why the SQL
/// filters cannot see it. It was the second way a declined vector could be
/// written anyway.
#[tokio::test]
async fn ensure_fact_embedding_leaves_a_declined_schema_alone()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
        let engine = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );

        let declined = engine
            .fact_ingest(&authz, declining_draft("declines a vector"))
            .await?;
        let embedded = engine
            .fact_ingest(&authz, fact_draft(&owner, "wants a vector"))
            .await?;

        engine
            .ensure_fact_embedding(&owner, declined.memory_id)
            .await?;
        engine
            .ensure_fact_embedding(&owner, embedded.memory_id)
            .await?;

        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), declined.memory_id).await?,
            0,
            "a direct embed-this-row request wrote the vector the schema declined"
        );
        assert_eq!(
            count_fact_embeddings(pg.pool_for_tests(), embedded.memory_id).await?,
            1,
            "the control must be embedded, or this proves only that the call \
             does nothing at all"
        );
        Ok(())
    }
    .await;
    drop(pg);
    drop_db(&db_name).await?;
    result
}

/// The owner-scoped backfill does not undo the write path's decision.
///
/// Sibling of the reconcile exclusion, and separately reachable: this is the
/// operator-facing "heal anything missing a vector" sweep, and a row that
/// declined one looks exactly like a row that is missing one. The Facts are
/// written by a client-less engine so nothing is queued at write time and
/// the backfill is the only thing that could queue them.
#[tokio::test]
async fn backfill_does_not_queue_a_schema_that_declined() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let owner = owner_fixture();
        let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

        let writer = engine_for(pg.clone(), None);
        let declined = writer
            .fact_ingest(&authz, declining_draft("declines a vector"))
            .await?;
        let embedded = writer
            .fact_ingest(&authz, fact_draft(&owner, "wants a vector"))
            .await?;
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), embedded.memory_id).await?,
            0,
            "no client at write time means nothing is queued yet"
        );

        let healer = engine_for(
            pg.clone(),
            Some(Arc::new(ConstantEmbedding::prefixed(
                "stub-fact-embed",
                &[0.25, 0.5, 0.75],
            ))),
        );
        let enqueued = healer
            .backfill_missing_embeddings(&authz, &owner, 100)
            .await?;

        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), declined.memory_id).await?,
            0,
            "the backfill queued a vector the schema declined"
        );
        assert_eq!(
            count_embedding_jobs(pg.pool_for_tests(), embedded.memory_id).await?,
            1,
            "the control must be healed, or the backfill is simply inert"
        );
        assert_eq!(enqueued, 1, "the backfill reports only the row it queued");
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
