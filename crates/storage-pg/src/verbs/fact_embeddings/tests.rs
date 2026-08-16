// Raw-owner write behavior tests live in-crate: `insert_embedding` /
// `insert_memory_embedding` are `pub(crate)` (below the proof gate), so
// external test binaries cannot reach them without a forgeable-proof surface.
#[cfg(test)]
mod pg_tests {
    use proxima_core::storage_ports::OwnerWritePermit;
    use proxima_core::test_fixtures::owner_fixture;
    use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
    use proxima_core::{
        AccessKind, AuthPath, AuthzContext, Engine, EntityKind, FactIngestPort, FlavorRegistry,
        GoalId, Owner, SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError,
    };
    use proxima_pg_testkit::drop_db;
    use uuid::Uuid;

    use proxima_core::EmbeddableEntityRef;
    use proxima_core::llm::EMBEDDING_DIM;

    use super::super::{
        claim_pending_embedding_jobs, insert_embedding, insert_memory_embedding,
        load_embedding_text,
    };
    use crate::test_fixtures::fresh_pg;

    fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
        let mut embedding = vec![0.0; EMBEDDING_DIM];
        embedding[..prefix.len()].copy_from_slice(&prefix);
        embedding
    }

    fn fact_draft(label: &str) -> FactWriteCommand {
        let now = time::OffsetDateTime::now_utc();
        FactWriteCommand {
            schema_id: SchemaId::new("proxima-test/fact-embedding-v1".into()),
            schema_version: SchemaVersion::new(1),
            handle: None,
            source_id: None,
            ingest_key: None,
            payload: label.as_bytes().to_vec(),
            rendered_text: Some(label.to_string()),
            lexical_language: None,
            receipt: Some(FactReceiptDraft {
                source_id: SourceId::new("proxima-test/fact-embedding"),
                source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                observed_at: now,
                occurred_at: now,
            }),
            citation: None,
            derived_from: Vec::new(),
            refs: Vec::new(),
            blob_id: None,
            kind: "fact".into(),
        }
    }

    async fn owner_fact_write_permit(owner: &Owner) -> Result<OwnerWritePermit, StorageError> {
        let Owner::Personal(user_id) = owner else {
            return Err(StorageError::Internal(
                "fact embedding test helper expects a personal owner".into(),
            ));
        };
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let authz = AuthzContext::for_subject(*user_id, AuthPath::HostBearer);
        engine
            .authorize_owner_write(&authz, owner, AccessKind::Fact)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))
    }

    async fn load_embedding_versions(
        pool: &sqlx::PgPool,
        entity_kind: EntityKind,
        entity_id: Uuid,
        model_id: &str,
    ) -> Result<Vec<i32>, sqlx::Error> {
        let _ = entity_kind;
        sqlx::query_scalar(
            "SELECT embedding_version
               FROM proxima_core.embeddings
              WHERE entity_id = $1
                AND model_id = $2
              ORDER BY embedding_version",
        )
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
        let _ = entity_kind;
        sqlx::query_scalar(
            "SELECT embedding_version
               FROM proxima_core.embedding_heads
              WHERE entity_id = $1
                AND model_id = $2",
        )
        .bind(entity_id)
        .bind(model_id)
        .fetch_optional(pool)
        .await
    }

    async fn count_fact_embeddings(
        pool: &sqlx::PgPool,
        memory_id: proxima_core::MemoryId,
    ) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_core.embeddings
              WHERE entity_id = $1
                AND model_id = 'stub-fact-embed'",
        )
        .bind(memory_id.into_inner())
        .fetch_one(pool)
        .await
    }

    async fn insert_goal_for_embedding(
        pool: &sqlx::PgPool,
        owner: &Owner,
        goal_id: Uuid,
    ) -> Result<GoalId, sqlx::Error> {
        let owner_id = owner.stored_owner_id();
        let owner_kind = proxima_core::OwnerRefKind::of(owner).as_str();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, $2::proxima_core.owner_kind)
             ON CONFLICT (owner_id) DO NOTHING",
        )
        .bind(owner_id)
        .bind(owner_kind)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.goal_head (handle, schema_id, owner_id, t)
             VALUES ($1, 'proxima-test/goal-embedding-v1', $2, $1)",
        )
        .bind(goal_id)
        .bind(owner_id)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.goal
                (handle, t, owner_id, title, state, request_id)
             VALUES ($1, $1, $2, 'Embedding goal', 'Active', $3)",
        )
        .bind(goal_id)
        .bind(owner_id)
        .bind(format!("goal-embedding:{goal_id}"))
        .execute(pool)
        .await?;
        Ok(GoalId::new(goal_id))
    }

    #[tokio::test]
    async fn concurrent_reembedding_allocates_contiguous_versions()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(&permit, &fact_draft("concurrent embedding fact"), None)
                .await?;
            let pool_a = pg.pool_for_tests().clone();
            let pool_b = pg.pool_for_tests().clone();
            let owner_a = owner;
            let owner_b = owner;
            let memory_id = outcome.memory_id;
            let first_vec = padded_embedding([1.0, 0.0, 0.0]);
            let second_vec = padded_embedding([0.0, 1.0, 0.0]);
            let (first, second) = tokio::try_join!(
                async move {
                    let mut tx = pool_a.begin().await.map_err(|err| {
                        StorageError::Internal(format!("begin embedding insert tx: {err}"))
                    })?;
                    let outcome = insert_memory_embedding(
                        &mut tx,
                        &owner_a,
                        EntityKind::Fact,
                        memory_id,
                        "stub-fact-embed",
                        EMBEDDING_DIM,
                        &first_vec,
                    )
                    .await?;
                    tx.commit().await.map_err(|err| {
                        StorageError::Internal(format!("commit embedding insert tx: {err}"))
                    })?;
                    Ok::<_, StorageError>(outcome)
                },
                async move {
                    let mut tx = pool_b.begin().await.map_err(|err| {
                        StorageError::Internal(format!("begin embedding insert tx: {err}"))
                    })?;
                    let outcome = insert_memory_embedding(
                        &mut tx,
                        &owner_b,
                        EntityKind::Fact,
                        memory_id,
                        "stub-fact-embed",
                        EMBEDDING_DIM,
                        &second_vec,
                    )
                    .await?;
                    tx.commit().await.map_err(|err| {
                        StorageError::Internal(format!("commit embedding insert tx: {err}"))
                    })?;
                    Ok::<_, StorageError>(outcome)
                }
            )?;
            let mut outcome_versions = vec![first.embedding_version, second.embedding_version];
            outcome_versions.sort_unstable();
            assert_eq!(outcome_versions, vec![1, 2]);
            assert_eq!(
                load_embedding_versions(
                    pg.pool_for_tests(),
                    EntityKind::Fact,
                    memory_id.into_inner(),
                    "stub-fact-embed",
                )
                .await?,
                vec![1, 2]
            );
            assert_eq!(
                load_embedding_head_version(
                    pg.pool_for_tests(),
                    EntityKind::Fact,
                    memory_id.into_inner(),
                    "stub-fact-embed",
                )
                .await?,
                Some(2)
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn goal_embedding_uses_goal_id_not_memory_id() -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let goal_uuid = Uuid::now_v7();
            let goal_id = insert_goal_for_embedding(pg.pool_for_tests(), &owner, goal_uuid).await?;
            let mut tx = pg.pool_for_tests().begin().await?;
            let outcome = insert_embedding(
                &mut tx,
                &owner,
                EmbeddableEntityRef::Goal(goal_id),
                "stub-fact-embed",
                EMBEDDING_DIM,
                &padded_embedding([0.25, 0.5, 0.75]),
            )
            .await?;
            tx.commit().await?;

            assert_eq!(outcome.embedding_version, 1);
            assert_eq!(
                load_embedding_versions(
                    pg.pool_for_tests(),
                    EntityKind::Goal,
                    goal_uuid,
                    "stub-fact-embed",
                )
                .await?,
                vec![1]
            );
            assert_eq!(
                load_embedding_head_version(
                    pg.pool_for_tests(),
                    EntityKind::Goal,
                    goal_uuid,
                    "stub-fact-embed",
                )
                .await?,
                Some(1)
            );
            let memory_rows: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1",
            )
            .bind(goal_uuid)
            .fetch_one(pg.pool_for_tests())
            .await?;
            assert_eq!(
                memory_rows, 0,
                "goal embedding validation must not use memory"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn insert_memory_embedding_noops_after_source_memory_deleted()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("deleted before embedding write"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let claims =
                claim_pending_embedding_jobs(pg.pool_for_tests(), "stub-fact-embed", 1).await?;
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].entity_id, outcome.memory_id);
            assert_eq!(
                load_embedding_text(
                    pg.pool_for_tests(),
                    &owner,
                    EntityKind::Fact,
                    outcome.memory_id,
                    &[],
                )
                .await?,
                Some("proxima-test/fact-embedding-v1".to_string()),
            );

            sqlx::query(
                "DELETE FROM proxima_core.embedding_jobs
                  WHERE entity_id = $1",
            )
            .bind(outcome.memory_id.into_inner())
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query("DELETE FROM proxima_core.memory WHERE t = $1")
                .bind(outcome.memory_id.into_inner())
                .execute(pg.pool_for_tests())
                .await?;

            let embedding = vec![0.125; EMBEDDING_DIM];
            let mut tx = pg.pool_for_tests().begin().await?;
            insert_memory_embedding(
                &mut tx,
                &owner,
                EntityKind::Fact,
                outcome.memory_id,
                "stub-fact-embed",
                EMBEDDING_DIM,
                &embedding,
            )
            .await?;
            tx.commit().await?;

            assert_eq!(
                load_embedding_text(
                    pg.pool_for_tests(),
                    &owner,
                    EntityKind::Fact,
                    outcome.memory_id,
                    &[],
                )
                .await?,
                None,
            );
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
}
