// Raw-owner write behavior tests live in-crate: `insert_embedding` /
// `insert_memory_embedding` are `pub(crate)` (below the proof gate), so
// external test binaries cannot reach them without a forgeable-proof surface.
#[cfg(test)]
mod pg_tests {
    use std::sync::Arc;

    use proxima_core::llm::{EmbeddingClient, EmbeddingRuntimePolicy, LlmError};
    use proxima_core::storage_ports::{EmbeddingWritePort, EmbeddingWriteProof, OwnerWritePermit};
    use proxima_core::test_fixtures::owner_fixture;
    use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
    use proxima_core::verbs::schema::MemorySearchProjection;
    use proxima_core::{
        AccessKind, AuthPath, AuthzContext, Engine, EntityKind, FactIngestPort, FlavorRegistry,
        GoalId, Owner, SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError,
    };
    use proxima_pg_testkit::drop_db;
    use uuid::Uuid;

    use proxima_core::EmbeddableEntityRef;
    use proxima_core::llm::EMBEDDING_DIM;

    use super::super::{
        EmbeddingReconcileOptions, EmbeddingReconcileScope, claim_pending_embedding_jobs,
        complete_embedding_job, count_embedding_job_status, drain_embedding_jobs_inline,
        embedding_ann_observability, fail_embedding_job, fail_embedding_job_permanently,
        insert_embedding, insert_memory_embedding, list_facts_missing_embedding,
        load_embedding_text, load_embedding_texts, reclaim_stale_embedding_jobs,
        reconcile_embeddings, release_embedding_jobs, renew_embedding_jobs,
    };
    use crate::core_pg_sidecars;
    use crate::test_fixtures::fresh_pg;
    use crate::verbs::forget::{MemoryColdStore, cold_object_key, forget_memory, owner_hash_hex};

    fn core_projections() -> Vec<MemorySearchProjection> {
        FlavorRegistry::new()
            .freeze_or_panic_for_tests()
            .search_projections()
            .to_vec()
    }

    fn stale_claim_seconds() -> i64 {
        EmbeddingRuntimePolicy::default().stale_claim_timeout_seconds()
    }

    fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
        let mut embedding = vec![0.0; EMBEDDING_DIM];
        embedding[..prefix.len()].copy_from_slice(&prefix);
        embedding
    }

    #[derive(Debug)]
    struct BlockingEmbedding {
        entered: Arc<tokio::sync::Semaphore>,
        release: Arc<tokio::sync::Semaphore>,
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for BlockingEmbedding {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            self.entered.add_permits(1);
            self.release
                .acquire()
                .await
                .map_err(|err| LlmError::Internal(err.to_string()))?
                .forget();
            Ok(padded_embedding([0.7, 0.8, 0.9]))
        }

        fn model_id(&self) -> &'static str {
            "stub-fact-embed"
        }

        fn dim(&self) -> usize {
            EMBEDDING_DIM
        }
    }

    #[derive(Debug)]
    struct RecordingBatchEmbedding {
        batch_widths: Arc<std::sync::Mutex<Vec<usize>>>,
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for RecordingBatchEmbedding {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            Ok(padded_embedding([0.2, 0.3, 0.4]))
        }

        async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            self.batch_widths
                .lock()
                .expect("test lock is not poisoned")
                .push(texts.len());
            Ok(texts
                .iter()
                .map(|_| padded_embedding([0.2, 0.3, 0.4]))
                .collect())
        }

        fn model_id(&self) -> &'static str {
            "stub-fact-embed"
        }

        fn dim(&self) -> usize {
            EMBEDDING_DIM
        }
    }

    #[derive(Debug)]
    struct InlineBatchEmbedding {
        pool: sqlx::PgPool,
        batch_widths: Arc<std::sync::Mutex<Vec<usize>>>,
        processing_widths: Arc<std::sync::Mutex<Vec<i64>>>,
    }

    #[derive(Debug)]
    struct InlinePoisonEmbedding {
        max_chars: usize,
        accepted_calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[derive(Debug)]
    struct MalformedBatchEmbedding {
        returned_vectors: usize,
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for MalformedBatchEmbedding {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            Ok(padded_embedding([0.8, 0.8, 0.8]))
        }

        async fn embed_many(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            Ok((0..self.returned_vectors)
                .map(|_| padded_embedding([0.8, 0.8, 0.8]))
                .collect())
        }

        fn model_id(&self) -> &'static str {
            "stub-fact-embed"
        }

        fn dim(&self) -> usize {
            EMBEDDING_DIM
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for InlinePoisonEmbedding {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
            if text.contains("always poison") || text.chars().count() > self.max_chars {
                return Err(LlmError::EmbedPermanent("input rejected".into()));
            }
            self.accepted_calls
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(padded_embedding([0.5, 0.6, 0.7]))
        }

        async fn embed_many(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            // A compatible endpoint may collapse an input-triggered process
            // failure into an ambiguous 400/EOF. The inline drain must use
            // its trivial probe to isolate this batch so an over-limit item
            // still reaches the existing chunk rescue.
            Err(LlmError::Embed("400 EOF".into()))
        }

        fn model_id(&self) -> &'static str {
            "stub-fact-embed"
        }

        fn dim(&self) -> usize {
            EMBEDDING_DIM
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for InlineBatchEmbedding {
        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            Ok(padded_embedding([0.2, 0.3, 0.4]))
        }

        async fn embed_many(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, LlmError> {
            let processing: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint
                   FROM proxima_core.embedding_jobs
                  WHERE model_id = $1 AND status = 'processing'",
            )
            .bind(self.model_id())
            .fetch_one(&self.pool)
            .await
            .map_err(|err| LlmError::Internal(err.to_string()))?;
            self.batch_widths
                .lock()
                .expect("test lock is not poisoned")
                .push(texts.len());
            self.processing_widths
                .lock()
                .expect("test lock is not poisoned")
                .push(processing);
            Ok(texts
                .iter()
                .map(|_| padded_embedding([0.2, 0.3, 0.4]))
                .collect())
        }

        fn model_id(&self) -> &'static str {
            "stub-fact-embed"
        }

        fn dim(&self) -> usize {
            EMBEDDING_DIM
        }
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

    async fn insert_claimed_fact_embedding(
        pg: &crate::PgStorage,
        claim: &proxima_core::EmbeddingJobClaim,
        prefix: [f32; 3],
    ) -> Result<proxima_core::EmbeddingWriteOutcome, StorageError> {
        pg.insert_embedding(
            &claim.owner,
            EmbeddableEntityRef::Memory {
                kind: claim.entity_kind,
                memory_id: claim.entity_id,
            },
            &claim.model_id,
            EMBEDDING_DIM,
            &padded_embedding(prefix),
            EmbeddingWriteProof::for_claim_for_tests(claim),
        )
        .await
    }

    /// `(status, last_error, claimed_at IS NULL)` for one entity's job.
    async fn job_state(
        pool: &sqlx::PgPool,
        entity_id: Uuid,
    ) -> Result<(String, Option<String>, bool), sqlx::Error> {
        sqlx::query_as(
            "SELECT status::text, last_error, claimed_at IS NULL
               FROM proxima_core.embedding_jobs
              WHERE entity_id = $1",
        )
        .bind(entity_id)
        .fetch_one(pool)
        .await
    }

    async fn job_claim_token(
        pool: &sqlx::PgPool,
        entity_id: Uuid,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT claim_token
               FROM proxima_core.embedding_jobs
              WHERE entity_id = $1",
        )
        .bind(entity_id)
        .fetch_one(pool)
        .await
    }

    fn missing_only(limit: i64) -> EmbeddingReconcileOptions<'static> {
        EmbeddingReconcileOptions {
            model_id: "stub-fact-embed",
            scope: EmbeddingReconcileScope::MissingOnly,
            limit: Some(limit),
            non_embeddable_schemas: &[],
        }
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
            let memory_rows: i64 =
                sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory WHERE t = $1")
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
                    &core_projections(),
                )
                .await?,
                None,
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
                    &core_projections(),
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

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn load_embedding_text_reads_the_schema_sidecar_projection()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let owner_id = owner.stored_owner_id();
            sqlx::query(
                "INSERT INTO proxima_core.owners (owner_id, kind)
                 VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
            )
            .bind(owner_id)
            .execute(pg.pool_for_tests())
            .await?;
            let note_handle = Uuid::now_v7();
            let note_t = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/agent-note-v1', $2, $3)",
            )
            .bind(note_handle)
            .bind(owner_id)
            .bind(note_t)
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
                 VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1')",
            )
            .bind(note_handle)
            .bind(note_t)
            .bind(owner_id)
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
                 VALUES ($1, $2, 'Hello', 'world', '{}')",
            )
            .bind(note_t)
            .bind(Uuid::now_v7())
            .execute(pg.pool_for_tests())
            .await?;

            let projections = core_projections();
            let text = load_embedding_text(
                pg.pool_for_tests(),
                &owner,
                EntityKind::Fact,
                proxima_core::MemoryId::new(note_t),
                &[],
                &projections,
            )
            .await?;
            assert_eq!(text.as_deref(), Some("Hello world"));

            let skipped = load_embedding_text(
                pg.pool_for_tests(),
                &owner,
                EntityKind::Fact,
                proxima_core::MemoryId::new(note_t),
                &["core/agent-note-v1".into()],
                &projections,
            )
            .await?;
            assert_eq!(skipped, None);

            let utter_handle = Uuid::now_v7();
            let utter_t = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/utterance-v1', $2, $3)",
            )
            .bind(utter_handle)
            .bind(owner_id)
            .bind(utter_t)
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
                 VALUES ($1, $2, 'fact', $3, 'core/utterance-v1')",
            )
            .bind(utter_handle)
            .bind(utter_t)
            .bind(owner_id)
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.utterance_v1 (t, speaker, conversation_id, text)
                 VALUES ($1, 'user', 'c1', 'said this')",
            )
            .bind(utter_t)
            .execute(pg.pool_for_tests())
            .await?;
            let uttered = load_embedding_text(
                pg.pool_for_tests(),
                &owner,
                EntityKind::Fact,
                proxima_core::MemoryId::new(utter_t),
                &[],
                &projections,
            )
            .await?;
            assert_eq!(
                uttered.as_deref(),
                Some("said this"),
                "utterance must not require the note/chunk hardcode"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn load_embedding_texts_aligns_mixed_schemas_and_misses()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let other = proxima_core::OwnerRef::Personal(proxima_core::UserId::new(Uuid::now_v7()));
            let owner_id = owner.stored_owner_id();
            sqlx::query(
                "INSERT INTO proxima_core.owners (owner_id, kind)
                 VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
            )
            .bind(owner_id)
            .execute(pg.pool_for_tests())
            .await?;
            let note_handle = Uuid::now_v7();
            let note_t = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/agent-note-v1', $2, $3)",
            )
            .bind(note_handle)
            .bind(owner_id)
            .bind(note_t)
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
                 VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1')",
            )
            .bind(note_handle)
            .bind(note_t)
            .bind(owner_id)
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
                 VALUES ($1, $2, 'Hello', 'world', '{}')",
            )
            .bind(note_t)
            .bind(Uuid::now_v7())
            .execute(pg.pool_for_tests())
            .await?;

            let utter_handle = Uuid::now_v7();
            let utter_t = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/utterance-v1', $2, $3)",
            )
            .bind(utter_handle)
            .bind(owner_id)
            .bind(utter_t)
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
                 VALUES ($1, $2, 'fact', $3, 'core/utterance-v1')",
            )
            .bind(utter_handle)
            .bind(utter_t)
            .bind(owner_id)
            .execute(pg.pool_for_tests())
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.utterance_v1 (t, speaker, conversation_id, text)
                 VALUES ($1, 'user', 'c1', 'said this')",
            )
            .bind(utter_t)
            .execute(pg.pool_for_tests())
            .await?;

            let missing = proxima_core::MemoryId::new(Uuid::now_v7());
            let projections = core_projections();
            let texts = load_embedding_texts(
                pg.pool_for_tests(),
                &[
                    (owner, EntityKind::Fact, proxima_core::MemoryId::new(note_t)),
                    (
                        owner,
                        EntityKind::Fact,
                        proxima_core::MemoryId::new(utter_t),
                    ),
                    (owner, EntityKind::Fact, missing),
                    (other, EntityKind::Fact, proxima_core::MemoryId::new(note_t)),
                ],
                &[],
                &projections,
            )
            .await?;
            assert_eq!(
                texts,
                vec![
                    Some("Hello world".into()),
                    Some("said this".into()),
                    None,
                    None,
                ]
            );

            let skipped = load_embedding_texts(
                pg.pool_for_tests(),
                &[
                    (owner, EntityKind::Fact, proxima_core::MemoryId::new(note_t)),
                    (
                        owner,
                        EntityKind::Fact,
                        proxima_core::MemoryId::new(utter_t),
                    ),
                ],
                &["core/agent-note-v1".into()],
                &projections,
            )
            .await?;
            assert_eq!(skipped, vec![None, Some("said this".into())]);
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn embedding_ann_observability_empty_db_has_no_canary()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let health =
                embedding_ann_observability(pg.pool_for_tests(), stale_claim_seconds()).await?;
            assert_eq!(health.embedding_rows, 0);
            assert_eq!(health.stale_processing_jobs, 0);
            assert!(health.recall_canary.is_none());
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn embedding_ann_observability_canary_recalls_one_head()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(&permit, &fact_draft("canary row"), Some("stub-fact-embed"))
                .await?;
            let mut tx = pg.pool_for_tests().begin().await?;
            insert_memory_embedding(
                &mut tx,
                &owner,
                EntityKind::Fact,
                outcome.memory_id,
                "stub-fact-embed",
                EMBEDDING_DIM,
                &padded_embedding([0.1, 0.2, 0.3]),
            )
            .await?;
            tx.commit().await?;

            let health =
                embedding_ann_observability(pg.pool_for_tests(), stale_claim_seconds()).await?;
            let canary = health
                .recall_canary
                .expect("one embedding head must produce a canary");
            assert_eq!(canary.model_id, "stub-fact-embed");
            assert_eq!(canary.exact_count, 1);
            assert_eq!(canary.ann_count, 1);
            assert_eq!(canary.overlap_count, 1);
            assert!(
                (canary.recall_at_k - 1.0).abs() < f64::EPSILON,
                "recall_at_k={}",
                canary.recall_at_k
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn list_facts_missing_embedding_is_one_head_antijoin()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let missing = pg
                .ingest_fact_atomic(&permit, &fact_draft("needs vec"), Some("stub-fact-embed"))
                .await?;
            let present = pg
                .ingest_fact_atomic(&permit, &fact_draft("has vec"), Some("stub-fact-embed"))
                .await?;
            let mut tx = pg.pool_for_tests().begin().await?;
            insert_memory_embedding(
                &mut tx,
                &owner,
                EntityKind::Fact,
                present.memory_id,
                "stub-fact-embed",
                EMBEDDING_DIM,
                &padded_embedding([0.2, 0.3, 0.4]),
            )
            .await?;
            tx.commit().await?;

            let listed = list_facts_missing_embedding(
                pg.pool_for_tests(),
                &owner,
                "stub-fact-embed",
                20,
                &[],
            )
            .await?;
            assert_eq!(listed, vec![missing.memory_id]);

            let skipped = list_facts_missing_embedding(
                pg.pool_for_tests(),
                &owner,
                "stub-fact-embed",
                20,
                &["proxima-test/fact-embedding-v1".into()],
            )
            .await?;
            assert!(skipped.is_empty(), "non_embeddable_schemas must be applied");
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// A provider that rejects an input will reject it again. Requeueing such
    /// a job spins claim → reject → requeue forever, which is what a shared
    /// `failed` status for both failure causes produced.
    #[tokio::test]
    async fn permanently_failed_job_is_never_requeued() -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("rejected forever"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let pool = pg.pool_for_tests();
            let entity_id = outcome.memory_id.into_inner();

            let claims = claim_pending_embedding_jobs(pool, "stub-fact-embed", 1).await?;
            assert_eq!(claims.len(), 1);
            let (status, _, claim_unstamped) = job_state(pool, entity_id).await?;
            assert_eq!(status, "processing");
            assert!(!claim_unstamped, "the claim must stamp claimed_at");
            assert_eq!(
                job_claim_token(pool, entity_id).await?,
                Some(claims[0].claim_token)
            );

            fail_embedding_job_permanently(pool, &claims[0], "embed memory text: over token limit")
                .await?;
            assert_eq!(
                job_state(pool, entity_id).await?,
                (
                    "failed_permanent".to_owned(),
                    Some("embed memory text: over token limit".to_owned()),
                    true
                ),
            );

            let outcome =
                reconcile_embeddings(pool, missing_only(100), stale_claim_seconds()).await?;
            assert_eq!(outcome.enqueued, 0, "a permanent rejection is not requeued");
            assert_eq!(job_state(pool, entity_id).await?.0, "failed_permanent");

            // The terminal backlog is still visible to an operator.
            assert_eq!(count_embedding_job_status(pool, &owner).await?.failed, 1);
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn permanently_failed_earlier_candidate_does_not_starve_later_missing_one()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let permanently_rejected = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("earlier permanent rejection"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let later_missing = pg
                .ingest_fact_atomic(&permit, &fact_draft("later missing embedding"), None)
                .await?;
            let pool = pg.pool_for_tests();

            let claims = claim_pending_embedding_jobs(pool, "stub-fact-embed", 1).await?;
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].entity_id, permanently_rejected.memory_id);
            fail_embedding_job_permanently(pool, &claims[0], "provider rejects forever").await?;

            let reconciled = reconcile_embeddings(
                pool,
                EmbeddingReconcileOptions {
                    model_id: "stub-fact-embed",
                    scope: EmbeddingReconcileScope::MissingOnly,
                    limit: Some(1),
                    non_embeddable_schemas: &[],
                },
                stale_claim_seconds(),
            )
            .await?;
            assert_eq!(reconciled.scanned, 1);
            assert_eq!(reconciled.enqueued, 1);
            assert_eq!(
                job_state(pool, later_missing.memory_id.into_inner())
                    .await?
                    .0,
                "pending"
            );
            assert_eq!(
                job_state(pool, permanently_rejected.memory_id.into_inner())
                    .await?
                    .0,
                "failed_permanent"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn reconcile_cannot_recreate_job_after_concurrent_forget()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let written = pg
                .ingest_fact_atomic(&permit, &fact_draft("forget during reconcile"), None)
                .await?;
            let pool = pg.pool_for_tests();
            let t = written.memory_id.into_inner();

            let mut forget_tx = pool.begin().await?;
            sqlx::query("SELECT t FROM proxima_core.memory WHERE t = $1 FOR UPDATE")
                .bind(t)
                .fetch_one(forget_tx.as_mut())
                .await?;

            let mut reconcile = {
                let pool = pool.clone();
                tokio::spawn(async move {
                    reconcile_embeddings(
                        &pool,
                        EmbeddingReconcileOptions {
                            model_id: "stub-fact-embed",
                            scope: EmbeddingReconcileScope::MissingOnly,
                            limit: Some(10),
                            non_embeddable_schemas: &[],
                        },
                        stale_claim_seconds(),
                    )
                    .await
                })
            };
            assert!(
                tokio::time::timeout(std::time::Duration::from_millis(50), &mut reconcile)
                    .await
                    .is_err(),
                "reconcile must wait on the memory row selected for enqueue"
            );

            let cold = MemoryColdStore::default();
            let object_key = cold_object_key(&owner_hash_hex(&owner), written.handle, t);
            forget_memory(
                &mut forget_tx,
                &core_pg_sidecars(),
                &cold,
                &object_key,
                t,
                owner.stored_owner_id(),
            )
            .await?;
            forget_tx.commit().await?;

            let reconciled = reconcile.await??;
            assert_eq!(reconciled.enqueued, 0);
            let jobs: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint
                   FROM proxima_core.embedding_jobs
                  WHERE entity_id = $1",
            )
            .bind(t)
            .fetch_one(pool)
            .await?;
            assert_eq!(jobs, 0, "forget must not leave a recreated orphan job");
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// The retryable dead-end reconcile exists to lift a memory out of.
    #[tokio::test]
    async fn retryably_failed_job_is_requeued_with_its_error_cleared()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("provider blipped"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let pool = pg.pool_for_tests();
            let entity_id = outcome.memory_id.into_inner();

            let claims = claim_pending_embedding_jobs(pool, "stub-fact-embed", 1).await?;
            fail_embedding_job(pool, &claims[0], "embed memory text: 503").await?;
            assert_eq!(
                job_state(pool, entity_id).await?,
                (
                    "failed".to_owned(),
                    Some("embed memory text: 503".to_owned()),
                    true
                ),
            );

            let reconciled =
                reconcile_embeddings(pool, missing_only(100), stale_claim_seconds()).await?;
            assert_eq!(reconciled.enqueued, 1);
            assert_eq!(
                job_state(pool, entity_id).await?,
                ("pending".to_owned(), None, true),
                "a requeued job carries no stale error"
            );
            let claimed_again = claim_pending_embedding_jobs(pool, "stub-fact-embed", 1).await?;
            assert_eq!(claimed_again.len(), 1, "the requeued job is claimable");
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn engine_drain_uses_host_configured_provider_batch_width()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            for label in ["one", "two", "three", "four", "five"] {
                let mut draft = fact_draft(label);
                draft.schema_id = SchemaId::new("core/agent-note-v1".into());
                let written = pg
                    .ingest_fact_atomic(&permit, &draft, Some("stub-fact-embed"))
                    .await?;
                sqlx::query(
                    "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
                     VALUES ($1, $2, $3, $3)",
                )
                .bind(written.memory_id.into_inner())
                .bind(Uuid::now_v7())
                .bind(label)
                .execute(pg.pool_for_tests())
                .await?;
            }

            let widths = Arc::new(std::sync::Mutex::new(Vec::new()));
            let policy = EmbeddingRuntimePolicy::new(
                std::time::Duration::from_secs(1),
                2,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(3),
            )?;
            let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
            let pg = pg
                .clone()
                .with_search_projections(registry.search_projections().to_vec());
            let engine = Engine::new(registry)
                .with_storage_ports(Arc::new(pg.clone()).storage_ports())
                .with_embedding_runtime_policy(policy)
                .with_embed(Arc::new(RecordingBatchEmbedding {
                    batch_widths: widths.clone(),
                }));

            let outcome = engine.drain_embedding_jobs(5).await?;
            assert_eq!(outcome.processed, 5);
            assert_eq!(
                *widths.lock().expect("test lock is not poisoned"),
                [2, 2, 1],
                "provider calls must follow the host policy, including the tail"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn inline_drain_batches_provider_calls_and_claims_by_host_policy()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            for label in ["one", "two", "three", "four", "five"] {
                let mut draft = fact_draft(label);
                draft.schema_id = SchemaId::new("core/agent-note-v1".into());
                let written = pg
                    .ingest_fact_atomic(&permit, &draft, Some("stub-fact-embed"))
                    .await?;
                sqlx::query(
                    "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
                     VALUES ($1, $2, $3, $3)",
                )
                .bind(written.memory_id.into_inner())
                .bind(Uuid::now_v7())
                .bind(label)
                .execute(pg.pool_for_tests())
                .await?;
            }

            let batch_widths = Arc::new(std::sync::Mutex::new(Vec::new()));
            let processing_widths = Arc::new(std::sync::Mutex::new(Vec::new()));
            let policy = EmbeddingRuntimePolicy::new(
                std::time::Duration::from_secs(1),
                2,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(3),
            )?;
            let client = InlineBatchEmbedding {
                pool: pg.pool_for_tests().clone(),
                batch_widths: batch_widths.clone(),
                processing_widths: processing_widths.clone(),
            };

            let outcome = drain_embedding_jobs_inline(
                pg.pool_for_tests(),
                &client,
                5,
                &core_projections(),
                policy,
            )
            .await?;
            assert_eq!(outcome.embedded, 5);
            assert_eq!(outcome.failed, 0);
            assert_eq!(
                *batch_widths.lock().expect("test lock is not poisoned"),
                [2, 2, 1],
                "provider calls must follow policy, including the tail"
            );
            assert_eq!(
                *processing_widths.lock().expect("test lock is not poisoned"),
                [2, 2, 1],
                "inline maintenance must not claim beyond the active provider batch"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn inline_ambiguous_batch_failure_uses_probe_and_rescues_long_input()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let mut ids = Vec::new();
            for (kind, text) in [
                ("good", "ordinary text".to_owned()),
                ("poison", "always poison".to_owned()),
                ("long", "x".repeat(12_000)),
            ] {
                let mut draft = fact_draft(&text);
                draft.schema_id = SchemaId::new("core/agent-note-v1".into());
                let written = pg
                    .ingest_fact_atomic(&permit, &draft, Some("stub-fact-embed"))
                    .await?;
                sqlx::query(
                    "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
                     VALUES ($1, $2, $3, $3)",
                )
                .bind(written.memory_id.into_inner())
                .bind(Uuid::now_v7())
                .bind(&text)
                .execute(pg.pool_for_tests())
                .await?;
                ids.push((kind, written.memory_id));
            }

            let policy = EmbeddingRuntimePolicy::new(
                std::time::Duration::from_secs(1),
                3,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(3),
            )?;
            let accepted_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let outcome = drain_embedding_jobs_inline(
                pg.pool_for_tests(),
                &InlinePoisonEmbedding {
                    max_chars: proxima_core::llm::MIN_EMBED_INPUT_CAP_CHARS,
                    accepted_calls: accepted_calls.clone(),
                },
                3,
                &core_projections(),
                policy,
            )
            .await?;

            assert_eq!(outcome.embedded, 2);
            assert_eq!(outcome.failed, 1);
            let good = ids
                .iter()
                .find(|(kind, _)| *kind == "good")
                .expect("good id")
                .1;
            let poison = ids
                .iter()
                .find(|(kind, _)| *kind == "poison")
                .expect("poison id")
                .1;
            let long = ids
                .iter()
                .find(|(kind, _)| *kind == "long")
                .expect("long id")
                .1;
            assert_eq!(count_fact_embeddings(pg.pool_for_tests(), good).await?, 1);
            assert_eq!(count_fact_embeddings(pg.pool_for_tests(), poison).await?, 0);
            assert_eq!(count_fact_embeddings(pg.pool_for_tests(), long).await?, 1);
            assert!(
                accepted_calls.load(std::sync::atomic::Ordering::SeqCst) > 2,
                "long input must be accepted through multiple rescued chunk calls"
            );
            assert_eq!(
                job_state(pg.pool_for_tests(), poison.into_inner()).await?.0,
                "failed_permanent"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn engine_malformed_batch_cardinality_releases_every_claim_before_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let mut ids = Vec::new();
            for label in ["short-one", "short-two"] {
                let mut draft = fact_draft(label);
                draft.schema_id = SchemaId::new("core/agent-note-v1".into());
                let written = pg
                    .ingest_fact_atomic(&permit, &draft, Some("stub-fact-embed"))
                    .await?;
                sqlx::query(
                    "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
                     VALUES ($1, $2, $3, $3)",
                )
                .bind(written.memory_id.into_inner())
                .bind(Uuid::now_v7())
                .bind(label)
                .execute(pg.pool_for_tests())
                .await?;
                ids.push(written.memory_id);
            }

            let policy = EmbeddingRuntimePolicy::new(
                std::time::Duration::from_secs(1),
                2,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(3),
            )?;
            let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
            let configured_pg = pg
                .clone()
                .with_search_projections(registry.search_projections().to_vec());
            let engine = Engine::new(registry)
                .with_storage_ports(Arc::new(configured_pg).storage_ports())
                .with_embedding_runtime_policy(policy)
                .with_embed(Arc::new(MalformedBatchEmbedding {
                    returned_vectors: 1,
                }));

            let outcome = engine.drain_embedding_jobs(2).await?;
            assert_eq!(outcome.processed, 0);
            assert_eq!(outcome.failed, 0);
            for id in ids {
                assert_eq!(count_fact_embeddings(pg.pool_for_tests(), id).await?, 0);
                let state = job_state(pg.pool_for_tests(), id.into_inner()).await?;
                assert_eq!(state.0, "pending");
                assert!(
                    state
                        .1
                        .as_deref()
                        .is_some_and(|error| error.contains("cardinality mismatch"))
                );
            }
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn inline_malformed_batch_cardinality_releases_every_claim_before_writes()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let mut ids = Vec::new();
            for label in ["extra-one", "extra-two"] {
                let mut draft = fact_draft(label);
                draft.schema_id = SchemaId::new("core/agent-note-v1".into());
                let written = pg
                    .ingest_fact_atomic(&permit, &draft, Some("stub-fact-embed"))
                    .await?;
                sqlx::query(
                    "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
                     VALUES ($1, $2, $3, $3)",
                )
                .bind(written.memory_id.into_inner())
                .bind(Uuid::now_v7())
                .bind(label)
                .execute(pg.pool_for_tests())
                .await?;
                ids.push(written.memory_id);
            }

            let policy = EmbeddingRuntimePolicy::new(
                std::time::Duration::from_secs(1),
                2,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(3),
            )?;
            let outcome = drain_embedding_jobs_inline(
                pg.pool_for_tests(),
                &MalformedBatchEmbedding {
                    returned_vectors: 3,
                },
                2,
                &core_projections(),
                policy,
            )
            .await?;
            assert_eq!(outcome.embedded, 0);
            assert_eq!(outcome.failed, 0);
            for id in ids {
                assert_eq!(count_fact_embeddings(pg.pool_for_tests(), id).await?, 0);
                let state = job_state(pg.pool_for_tests(), id.into_inner()).await?;
                assert_eq!(state.0, "pending");
                assert!(
                    state
                        .1
                        .as_deref()
                        .is_some_and(|error| error.contains("cardinality mismatch"))
                );
            }
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn engine_heartbeat_keeps_a_live_provider_call_out_of_reclaim()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let mut draft = fact_draft("heartbeat protected");
            draft.schema_id = SchemaId::new("core/agent-note-v1".into());
            let written = pg
                .ingest_fact_atomic(&permit, &draft, Some("stub-fact-embed"))
                .await?;
            sqlx::query(
                "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
                 VALUES ($1, $2, 'heartbeat', 'heartbeat protected')",
            )
            .bind(written.memory_id.into_inner())
            .bind(Uuid::now_v7())
            .execute(pg.pool_for_tests())
            .await?;

            let client = Arc::new(BlockingEmbedding {
                entered: Arc::new(tokio::sync::Semaphore::new(0)),
                release: Arc::new(tokio::sync::Semaphore::new(0)),
            });
            let policy = EmbeddingRuntimePolicy::new(
                std::time::Duration::from_secs(2),
                1,
                std::time::Duration::from_secs(1),
                std::time::Duration::from_secs(3),
            )?;
            let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
            let configured_pg = pg
                .clone()
                .with_search_projections(registry.search_projections().to_vec());
            let engine = Arc::new(
                Engine::new(registry)
                    .with_storage_ports(Arc::new(configured_pg).storage_ports())
                    .with_embedding_runtime_policy(policy)
                    .with_embed(client.clone()),
            );
            let drain = {
                let engine = engine.clone();
                tokio::spawn(async move { engine.drain_embedding_jobs(1).await })
            };
            client.entered.acquire().await?.forget();

            let forced_claimed_at: time::OffsetDateTime = sqlx::query_scalar(
                "UPDATE proxima_core.embedding_jobs
                    SET claimed_at = now() - make_interval(secs => 30)
                  WHERE entity_id = $1
              RETURNING claimed_at",
            )
            .bind(written.memory_id.into_inner())
            .fetch_one(pg.pool_for_tests())
            .await?;
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let claimed_at: time::OffsetDateTime = sqlx::query_scalar(
                        "SELECT claimed_at
                           FROM proxima_core.embedding_jobs
                          WHERE entity_id = $1",
                    )
                    .bind(written.memory_id.into_inner())
                    .fetch_one(pg.pool_for_tests())
                    .await?;
                    if claimed_at > forced_claimed_at {
                        return Ok::<(), sqlx::Error>(());
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                }
            })
            .await??;
            assert_eq!(
                reclaim_stale_embedding_jobs(
                    pg.pool_for_tests(),
                    policy.stale_claim_timeout_seconds(),
                )
                .await?,
                0,
                "heartbeat must refresh the claim while the provider future is pending"
            );

            client.release.add_permits(1);
            assert_eq!(drain.await??.processed, 1);
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// A drainer that dies holding a claim leaves `processing` forever, and
    /// the job's unique key blocks any re-enqueue — so the memory stops being
    /// embeddable with nothing reporting it.
    #[tokio::test]
    async fn stale_processing_job_is_reclaimed_and_a_fresh_claim_is_not()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let abandoned = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("abandoned claim"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let live = pg
                .ingest_fact_atomic(&permit, &fact_draft("live claim"), Some("stub-fact-embed"))
                .await?;
            let pool = pg.pool_for_tests();

            let claims = claim_pending_embedding_jobs(pool, "stub-fact-embed", 2).await?;
            assert_eq!(claims.len(), 2);

            sqlx::query(
                "UPDATE proxima_core.embedding_jobs
                    SET claimed_at = now() - make_interval(secs => $2::double precision)
                  WHERE entity_id = $1",
            )
            .bind(abandoned.memory_id.into_inner())
            .bind(f64::from(u32::try_from(stale_claim_seconds())?) * 2.0)
            .execute(pool)
            .await?;

            let health = embedding_ann_observability(pool, stale_claim_seconds()).await?;
            assert_eq!(health.stale_processing_jobs, 1);
            assert_eq!(health.backlog.processing, 2);

            let reclaimed = reclaim_stale_embedding_jobs(pool, stale_claim_seconds()).await?;
            assert_eq!(reclaimed, 1, "only the abandoned claim is reclaimed");
            assert_eq!(
                job_state(pool, abandoned.memory_id.into_inner()).await?,
                ("pending".to_owned(), None, true),
            );
            assert_eq!(
                job_claim_token(pool, abandoned.memory_id.into_inner()).await?,
                None,
                "reclaim clears the fencing token"
            );
            assert_eq!(
                job_state(pool, live.memory_id.into_inner()).await?.0,
                "processing",
                "a claim inside the window belongs to a live drainer"
            );

            let reclaimable = claim_pending_embedding_jobs(pool, "stub-fact-embed", 2).await?;
            assert_eq!(
                reclaimable.len(),
                1,
                "the reclaimed job is claimable again; the live one is not"
            );
            assert_eq!(reclaimable[0].entity_id, abandoned.memory_id);
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn claim_renewal_prevents_reclaim_without_changing_fence()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let written = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("renewed live claim"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let pool = pg.pool_for_tests();
            let claims = claim_pending_embedding_jobs(pool, "stub-fact-embed", 1).await?;
            let claim = &claims[0];

            sqlx::query(
                "UPDATE proxima_core.embedding_jobs
                    SET claimed_at = now() - make_interval(secs => $2::double precision)
                  WHERE entity_id = $1",
            )
            .bind(written.memory_id.into_inner())
            .bind(f64::from(u32::try_from(stale_claim_seconds())?) * 2.0)
            .execute(pool)
            .await?;

            assert_eq!(renew_embedding_jobs(pool, &claims).await?, 1);
            assert_eq!(
                job_claim_token(pool, written.memory_id.into_inner()).await?,
                Some(claim.claim_token),
                "heartbeat must retain the fencing token"
            );
            assert_eq!(
                embedding_ann_observability(pool, stale_claim_seconds())
                    .await?
                    .stale_processing_jobs,
                0
            );
            assert_eq!(
                reclaim_stale_embedding_jobs(pool, stale_claim_seconds()).await?,
                0,
                "renewed live claim must not be reclaimed"
            );
            complete_embedding_job(pool, claim).await?;
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn reclaimed_claim_cannot_mutate_successor_job() -> Result<(), Box<dyn std::error::Error>>
    {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("fenced claim"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let pool = pg.pool_for_tests();
            let old_claims = claim_pending_embedding_jobs(pool, "stub-fact-embed", 1).await?;
            let old_claim = old_claims[0].clone();

            sqlx::query(
                "UPDATE proxima_core.embedding_jobs
                    SET claimed_at = now() - make_interval(secs => $2::double precision)
                  WHERE entity_id = $1",
            )
            .bind(outcome.memory_id.into_inner())
            .bind(f64::from(u32::try_from(stale_claim_seconds())?) * 2.0)
            .execute(pool)
            .await?;
            assert_eq!(
                reclaim_stale_embedding_jobs(pool, stale_claim_seconds()).await?,
                1
            );

            let successor_claims = claim_pending_embedding_jobs(pool, "stub-fact-embed", 1).await?;
            assert_eq!(successor_claims.len(), 1);
            let successor = &successor_claims[0];
            assert_eq!(old_claim.job_id, successor.job_id);
            assert_ne!(old_claim.claim_token, successor.claim_token);
            assert_eq!(
                job_claim_token(pool, outcome.memory_id.into_inner()).await?,
                Some(successor.claim_token)
            );
            assert_eq!(
                job_state(pool, outcome.memory_id.into_inner()).await?.0,
                "processing"
            );

            let stale_write = insert_claimed_fact_embedding(&pg, &old_claim, [0.1, 0.2, 0.3]).await;
            assert!(
                matches!(stale_write, Err(StorageError::Conflict(_))),
                "stale worker embedding write must be fenced: {stale_write:?}"
            );
            assert_eq!(count_fact_embeddings(pool, outcome.memory_id).await?, 0);

            for stale in [
                complete_embedding_job(pool, &old_claim).await,
                fail_embedding_job(pool, &old_claim, "stale failure").await,
                release_embedding_jobs(pool, std::slice::from_ref(&old_claim), "stale release")
                    .await,
            ] {
                assert!(
                    matches!(stale, Err(StorageError::Conflict(_))),
                    "stale worker transition must be fenced: {stale:?}"
                );
            }
            assert_eq!(
                job_state(pool, outcome.memory_id.into_inner()).await?.0,
                "processing"
            );
            assert_eq!(
                job_claim_token(pool, outcome.memory_id.into_inner()).await?,
                Some(successor.claim_token)
            );

            insert_claimed_fact_embedding(&pg, successor, [0.4, 0.5, 0.6]).await?;
            complete_embedding_job(pool, successor).await?;
            assert_eq!(count_fact_embeddings(pool, outcome.memory_id).await?, 1);
            assert_eq!(
                load_embedding_head_version(
                    pool,
                    EntityKind::Fact,
                    outcome.memory_id.into_inner(),
                    "stub-fact-embed",
                )
                .await?,
                Some(1)
            );
            let remaining: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.embedding_jobs WHERE entity_id = $1",
            )
            .bind(outcome.memory_id.into_inner())
            .fetch_one(pool)
            .await?;
            assert_eq!(remaining, 0, "the successor can finalize its own claim");
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn reclaimed_inline_drain_cannot_write_after_successor_claim()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let mut draft = fact_draft("blocked inline drainer");
            draft.schema_id = SchemaId::new("core/agent-note-v1".into());
            let written = pg
                .ingest_fact_atomic(&permit, &draft, Some("stub-fact-embed"))
                .await?;
            let pool = pg.pool_for_tests();
            sqlx::query(
                "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
                 VALUES ($1, $2, 'blocked', 'inline drainer')",
            )
            .bind(written.memory_id.into_inner())
            .bind(Uuid::now_v7())
            .execute(pool)
            .await?;
            let client = Arc::new(BlockingEmbedding {
                entered: Arc::new(tokio::sync::Semaphore::new(0)),
                release: Arc::new(tokio::sync::Semaphore::new(0)),
            });
            let drain = {
                let pool = pool.clone();
                let client = client.clone();
                tokio::spawn(async move {
                    drain_embedding_jobs_inline(
                        &pool,
                        client.as_ref(),
                        1,
                        &core_projections(),
                        EmbeddingRuntimePolicy::default(),
                    )
                    .await
                })
            };
            client.entered.acquire().await?.forget();

            sqlx::query(
                "UPDATE proxima_core.embedding_jobs
                    SET claimed_at = now() - make_interval(secs => $2::double precision)
                  WHERE entity_id = $1",
            )
            .bind(written.memory_id.into_inner())
            .bind(f64::from(u32::try_from(stale_claim_seconds())?) * 2.0)
            .execute(pool)
            .await?;
            assert_eq!(
                reclaim_stale_embedding_jobs(pool, stale_claim_seconds()).await?,
                1
            );
            let successor = claim_pending_embedding_jobs(pool, "stub-fact-embed", 1).await?;
            assert_eq!(successor.len(), 1);

            client.release.add_permits(1);
            let stale_error = drain
                .await?
                .expect_err("the reclaimed inline drainer must lose its write fence");
            assert!(matches!(stale_error, StorageError::Conflict(_)));
            assert_eq!(count_fact_embeddings(pool, written.memory_id).await?, 0);
            assert_eq!(
                job_claim_token(pool, written.memory_id.into_inner()).await?,
                Some(successor[0].claim_token)
            );
            complete_embedding_job(pool, &successor[0]).await?;
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// Reconcile is the maintenance entry point, so it is what has to carry
    /// the reclaim: nothing else runs on the `maintain-embeddings` path.
    #[tokio::test]
    async fn reconcile_reclaims_an_abandoned_claim() -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("abandoned by reconcile"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let pool = pg.pool_for_tests();
            let entity_id = outcome.memory_id.into_inner();
            claim_pending_embedding_jobs(pool, "stub-fact-embed", 1).await?;
            sqlx::query(
                "UPDATE proxima_core.embedding_jobs
                    SET claimed_at = now() - make_interval(secs => $2::double precision)
                  WHERE entity_id = $1",
            )
            .bind(entity_id)
            .bind(f64::from(u32::try_from(stale_claim_seconds())?) * 2.0)
            .execute(pool)
            .await?;

            reconcile_embeddings(pool, missing_only(100), stale_claim_seconds()).await?;
            assert_eq!(job_state(pool, entity_id).await?.0, "pending");
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }
}
