// Raw-owner write behavior tests live in-crate: `insert_embedding` is
// `pub(crate)` (below the proof gate), so
// external test binaries cannot reach them without a forgeable-proof surface.
#[cfg(test)]
mod pg_tests {
    use std::sync::Arc;

    use proxima_core::llm::{
        EmbeddingClient, EmbeddingInputRefusal, EmbeddingRuntimePolicy, LlmError,
    };
    use proxima_core::storage_ports::{
        EmbeddingWrite, EmbeddingWritePort, EmbeddingWriteProof, OwnerTransferPort,
        OwnerWritePermit,
    };
    use proxima_core::test_fixtures::authenticated_context;
    use proxima_core::test_fixtures::owner_fixture;
    use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
    use proxima_core::verbs::schema::MemoryEmbedUnit;
    use proxima_core::{
        AccessKind, AuthPath, AuthzContext, Engine, EntityId, EntityKind, FactIngestPort,
        FlavorRegistry, GroupId, InputContractId, MemoryTarget, OwnerId, ProtocolError, SchemaId,
        SchemaVersion, SourceId, StorageError,
    };
    use proxima_pg_testkit::drop_db;
    use uuid::Uuid;

    use proxima_core::EmbeddableEntityRef;
    use proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT;
    use proxima_core::llm::{CHUNKED_EMBED_MIN_BYTES, MIN_EMBED_INPUT_CAP_CHARS};
    use proxima_core::llm::{EmbeddingDim, SingleClientRouter};
    use proxima_core::{
        AgentDerivationV1, DerivationIdentity, DerivedMemory, MemoryId, OperatorId, SeriesHandle,
    };
    use proxima_core::{EmbeddingSpace, SpaceVector};

    use super::super::{
        EmbeddingReconcileOptions, EmbeddingReconcileScope, claim_pending_embedding_jobs,
        complete_embedding_job, count_embedding_job_status, embedding_ann_observability,
        fail_embedding_job, fail_embedding_job_permanently, insert_embedding, load_embedding_text,
        load_embedding_texts, reclaim_stale_embedding_jobs, reconcile_embeddings,
        release_embedding_jobs, renew_embedding_jobs,
    };
    use crate::VectorStorage;
    use crate::test_fixtures::fresh_pg;
    use crate::verbs::forget::forget_memory;

    // Memory fixture shorthand around the production entity-based writer.
    async fn insert_memory_embedding(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        owner: &OwnerId,
        entity_kind: EntityKind,
        memory_id: MemoryId,
        vectors: &[SpaceVector],
    ) -> Result<proxima_core::EmbeddingWriteOutcome, StorageError> {
        insert_embedding(
            tx,
            owner,
            EmbeddableEntityRef::Memory {
                kind: entity_kind,
                memory_id,
            },
            vectors,
        )
        .await
    }

    fn core_embed_units() -> Vec<MemoryEmbedUnit> {
        FlavorRegistry::new()
            .freeze_or_panic_for_tests()
            .embed_units()
            .to_vec()
    }

    const EMBEDDING_DIM: usize = EmbeddingDim::D1024.width();

    /// The space every stub client here embeds in.
    static STUB_SPACE: std::sync::LazyLock<EmbeddingSpace> =
        std::sync::LazyLock::new(|| EmbeddingSpace::new("stub-fact-embed", EmbeddingDim::D1024));

    fn stub_vector(values: Vec<f32>) -> SpaceVector {
        SpaceVector::new(STUB_SPACE.clone(), values).expect("1024-wide test vector")
    }

    /// Every Owner routed to `client`.
    fn routed(client: impl EmbeddingClient + 'static) -> Arc<SingleClientRouter> {
        Arc::new(SingleClientRouter::bind(Arc::new(client)).expect("stub clients embed in a lane"))
    }

    fn stale_claim_seconds() -> i64 {
        EmbeddingRuntimePolicy::default().stale_claim_timeout_seconds()
    }

    /// The operator read of a store that keeps its vectors in pgvector.
    async fn local_observability(
        pool: &sqlx::PgPool,
        platform: &crate::PgPlatformScope,
    ) -> Result<proxima_core::EmbeddingAnnObservability, proxima_core::StorageError> {
        embedding_ann_observability(pool, platform, stale_claim_seconds(), VectorStorage::Local)
            .await
    }

    fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
        let mut embedding = vec![0.0; EMBEDDING_DIM];
        embedding[..prefix.len()].copy_from_slice(&prefix);
        embedding
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

    fn fact_draft(label: &str) -> FactWriteCommand {
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
            }),
            occurred_at: None,
            citation: None,
            additional_references: Vec::new(),
            refs: Vec::new(),
            blob_id: None,
            kind: proxima_core::MemoryKind::Fact,
            request: None,
        }
    }

    async fn owner_fact_write_permit(owner: &OwnerId) -> Result<OwnerWritePermit, StorageError> {
        let user_id = owner.as_user();
        let engine = Engine::new(FlavorRegistry::new().freeze_or_panic_for_tests());
        let authz = authenticated_context(AuthzContext::for_subject(user_id, AuthPath::HostBearer));
        engine
            .authorize_owner_write(&authz, owner, AccessKind::Fact)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))
    }

    const AGENT_NOTE: &str = "proxima_core.agent_note_v1";

    /// A note Fact and its sidecar row, in one admission that DECLARES the
    /// table. `ingest_fact_atomic` declares none, so a note row hung off one
    /// is a row `forget` and the owner inverse can never reach — which
    /// `assert_memory_declares_sidecar` refuses.
    async fn ingest_note_fact(
        pg: &crate::PgStorage,
        owner: &OwnerId,
        draft: &FactWriteCommand,
        embedding_spaces: &[EmbeddingSpace],
        title: &str,
        body: &str,
    ) -> Result<proxima_core::verbs::fact_ingest::FactIngestOutcome, StorageError> {
        let authorized = proxima_core::verbs::fact_ingest::AuthorizedFactWrite::new_for_tests(
            OwnerWritePermit::new_for_tests(*owner, AccessKind::Fact),
            draft.clone(),
            Some(AGENT_NOTE.to_owned()),
            Vec::new(),
        );
        // The sidecar closure may borrow nothing shorter than the
        // transaction, so the row's text goes in owned.
        let (title, body) = (title.to_owned(), body.to_owned());
        let mut tx = pg
            .pool_for_tests()
            .begin()
            .await
            .map_err(crate::error::map_err)?;
        let outcome = crate::verbs::fact_ingest::ingest_fact_with_sidecar_in_tx(
            &mut tx,
            &authorized,
            embedding_spaces,
            crate::verbs::fact_ingest::FactAdmissionInput {
                store_number: 1,
                natural_key: None,
                sidecar_tables: &[AGENT_NOTE.to_owned()],
                scopes: &[],
                content: crate::verbs::fact_ingest::ContentResolution {
                    content_id: None,
                    payloads: Some(&[]),
                },
                publication: None,
            },
            move |tx, outcome| {
                Box::pin(async move {
                    sqlx::query(
                        "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body)
                         VALUES ($1, $2, $3, $4)",
                    )
                    .bind(outcome.memory_id.into_inner())
                    .bind(Uuid::now_v7())
                    .bind(title)
                    .bind(body)
                    .execute(&mut **tx)
                    .await
                    .map_err(crate::error::map_err)?;
                    Ok(())
                })
            },
        )
        .await?;
        tx.commit().await.map_err(crate::error::map_err)?;
        Ok(outcome)
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

    async fn assert_chunk_ordinals(
        pool: &sqlx::PgPool,
        memory_id: MemoryId,
        count: usize,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let ordinals: Vec<i32> = sqlx::query_scalar(
            "SELECT chunk_ordinal FROM proxima_core.embeddings
              WHERE entity_id = $1 ORDER BY chunk_ordinal",
        )
        .bind(memory_id.into_inner())
        .fetch_all(pool)
        .await?;
        assert_eq!(ordinals, (0..i32::try_from(count)?).collect::<Vec<_>>());
        Ok(())
    }

    #[tokio::test]
    async fn embedding_chunks_publish_atomically_in_text_order()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_chunks").await;
        let platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let memory = pg
                .ingest_fact_atomic(&permit, &fact_draft("chunks"), None)
                .await?;
            let pool = pg.pool_for_tests();
            let vectors: Vec<_> = [0.1, 0.2, 0.3]
                .into_iter()
                .map(|value| stub_vector(padded_embedding([value, 0.0, 0.0])))
                .collect();
            let mut tx = pool.begin().await?;
            let outcome = insert_memory_embedding(
                &mut tx,
                &owner,
                EntityKind::Fact,
                memory.memory_id,
                &vectors,
            )
            .await?;
            assert_eq!(outcome.embedding_version, 1);
            assert_eq!(count_fact_embeddings(pool, memory.memory_id).await?, 0);
            assert_eq!(
                load_embedding_head_version(
                    pool,
                    EntityKind::Fact,
                    memory.memory_id.into_inner(),
                    "stub-fact-embed"
                )
                .await?,
                None,
                "neither chunks nor head are visible before commit"
            );
            tx.commit().await?;
            assert_chunk_ordinals(pool, memory.memory_id, vectors.len()).await?;
            let stored: Vec<Vec<f32>> = sqlx::query_scalar(
                "SELECT vec::real[] FROM proxima_core.embeddings
                  WHERE entity_id = $1 ORDER BY chunk_ordinal",
            )
            .bind(memory.memory_id.into_inner())
            .fetch_all(pool)
            .await?;
            assert_eq!(
                stored,
                vectors
                    .iter()
                    .map(|vector| vector.values().to_vec())
                    .collect::<Vec<_>>()
            );
            let coverage = super::super::embedding_coverage(
                pool,
                &platform,
                &owner,
                std::slice::from_ref(&*STUB_SPACE),
                &[],
            )
            .await?;
            assert_eq!(coverage.len(), 1);
            assert_eq!(
                (
                    coverage[0].1.embeddable,
                    coverage[0].1.embedded,
                    coverage[0].1.vectors
                ),
                (1, 1, 3)
            );
            assert_eq!(
                reconcile_embeddings(pool, &platform, missing_only(100), stale_claim_seconds())
                    .await?
                    .enqueued,
                0
            );
            let observability = local_observability(pool, &platform).await?;
            assert_eq!(
                (
                    observability.embedding_rows,
                    observability.embedding_head_rows
                ),
                (3, 1)
            );
            let canary = observability
                .recall_canary
                .expect("stored chunks produce a canary");
            assert_eq!(
                (canary.exact_count, canary.ann_count, canary.overlap_count),
                (3, 3, 3)
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn invalid_later_embedding_chunk_preserves_the_complete_head()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_chunks").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let memory = pg
                .ingest_fact_atomic(&permit, &fact_draft("chunks"), None)
                .await?;
            let pool = pg.pool_for_tests();
            let valid = stub_vector(padded_embedding([0.1, 0.2, 0.3]));
            let mut tx = pool.begin().await?;
            insert_memory_embedding(
                &mut tx,
                &owner,
                EntityKind::Fact,
                memory.memory_id,
                std::slice::from_ref(&valid),
            )
            .await?;
            tx.commit().await?;
            let wrong_space = SpaceVector::new(
                EmbeddingSpace::new("other-model", EmbeddingDim::D1024),
                valid.values().to_vec(),
            )?;
            let nonfinite = stub_vector(padded_embedding([0.1, f32::NAN, 0.3]));
            for vectors in [
                vec![],
                vec![valid.clone(), wrong_space],
                vec![valid.clone(), nonfinite],
            ] {
                let mut tx = pool.begin().await?;
                let error = insert_memory_embedding(
                    &mut tx,
                    &owner,
                    EntityKind::Fact,
                    memory.memory_id,
                    &vectors,
                )
                .await
                .expect_err("invalid full version must fail");
                assert!(
                    matches!(error, StorageError::ConstraintViolation(_)),
                    "{error}"
                );
                tx.commit().await?;
                assert_eq!(count_fact_embeddings(pool, memory.memory_id).await?, 1);
                assert_eq!(
                    load_embedding_head_version(
                        pool,
                        EntityKind::Fact,
                        memory.memory_id.into_inner(),
                        "stub-fact-embed"
                    )
                    .await?,
                    Some(1)
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
    async fn later_chunk_sql_failure_rolls_back_the_version()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_chunks").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let memory = pg
                .ingest_fact_atomic(&permit, &fact_draft("chunks"), Some("stub-fact-embed"))
                .await?;
            let pool = pg.pool_for_tests();
            let claims = claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
            assert_eq!(claims.len(), 1);
            let claim = &claims[0];
            let mut first = pool.begin().await?;
            insert_memory_embedding(
                &mut first,
                &owner,
                EntityKind::Fact,
                memory.memory_id,
                &[stub_vector(padded_embedding([0.1, 0.2, 0.3]))],
            )
            .await?;
            first.commit().await?;
            sqlx::query(
                "ALTER TABLE proxima_core.embeddings
                 ADD CONSTRAINT fail_later_chunk
                 CHECK (embedding_version = 1 OR chunk_ordinal <> 1)",
            )
            .execute(pool)
            .await?;
            let entity = EmbeddableEntityRef::Memory {
                kind: EntityKind::Fact,
                memory_id: memory.memory_id,
            };
            let vectors = vec![stub_vector(padded_embedding([0.4, 0.5, 0.6])); 2];
            let error = pg
                .insert_embedding(
                    &owner,
                    entity,
                    &vectors,
                    EmbeddingWriteProof::for_claim_for_tests(claim),
                )
                .await
                .expect_err("the second chunk must fail after the first was inserted");
            assert!(error.to_string().contains("fail_later_chunk"), "{error}");
            assert_eq!(count_fact_embeddings(pool, memory.memory_id).await?, 1);
            assert_eq!(
                load_embedding_head_version(
                    pool,
                    EntityKind::Fact,
                    memory.memory_id.into_inner(),
                    "stub-fact-embed"
                )
                .await?,
                Some(1),
                "a failed version leaves neither partial chunks nor a new head"
            );
            assert_eq!(
                job_state(pool, memory.memory_id.into_inner()).await?.0,
                "processing",
                "the claim is completed with the version or not at all"
            );
            sqlx::query("ALTER TABLE proxima_core.embeddings DROP CONSTRAINT fail_later_chunk")
                .execute(pool)
                .await?;
            let retried = pg
                .insert_embedding(
                    &owner,
                    entity,
                    &vectors,
                    EmbeddingWriteProof::for_claim_for_tests(claim),
                )
                .await?;
            assert_eq!(retried.embedding_version, 2);
            assert_eq!(count_fact_embeddings(pool, memory.memory_id).await?, 3);
            assert!(
                job_state(pool, memory.memory_id.into_inner())
                    .await
                    .is_err(),
                "the version that landed completed its job"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn embedding_space_purge_limits_chunk_rows() -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_chunks").await;
        let platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let memory = pg
                .ingest_fact_atomic(&permit, &fact_draft("chunks"), None)
                .await?;
            let pool = pg.pool_for_tests();
            let vectors = vec![stub_vector(padded_embedding([0.1, 0.2, 0.3])); 3];
            let mut tx = pool.begin().await?;
            insert_memory_embedding(
                &mut tx,
                &owner,
                EntityKind::Fact,
                memory.memory_id,
                &vectors,
            )
            .await?;
            tx.commit().await?;
            let purged =
                super::super::purge_embedding_spaces(pool, &platform, &owner, &[], 2).await?;
            assert_eq!(
                purged.vectors, 2,
                "the row limit counts chunks, not versions"
            );
            assert_eq!(count_fact_embeddings(pool, memory.memory_id).await?, 1);
            let purged =
                super::super::purge_embedding_spaces(pool, &platform, &owner, &[], 2).await?;
            assert_eq!(purged.vectors, 1);
            assert_eq!(count_fact_embeddings(pool, memory.memory_id).await?, 0);
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
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
            &[
                SpaceVector::new(claim.space.clone(), padded_embedding(prefix))
                    .expect("1024-wide test vector"),
            ],
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
            space: &STUB_SPACE,
            owners: None,
            scope: EmbeddingReconcileScope::MissingOnly,
            limit: Some(limit),
            non_embeddable_schemas: &[],
        }
    }

    /// Heads that need a job, seeded below the write path: `count` heads of
    /// one owner, each with the memory row the reconcile reads.
    async fn seed_uncovered_heads(
        pool: &sqlx::PgPool,
        owner: &OwnerId,
        count: i32,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'group') ON CONFLICT DO NOTHING",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        // A write takes one advisory lock per memory, so the heads go in in
        // batches that fit the server's lock table.
        let mut remaining = count;
        while remaining > 0 {
            let batch = remaining.min(1_000);
            remaining -= batch;
            let mut tx = pool.begin().await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 SELECT uuidv7(), 'fact', 'core/agent-note-v1', $1, uuidv7()
                   FROM generate_series(1, $2)",
            )
            .bind(owner.into_inner())
            .bind(batch)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.memory
                     (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
                 SELECT h.handle, h.t, 'fact', $1, h.schema_id,
                        ARRAY['proxima_core.agent_note_v1'], uuid_extract_timestamp(h.t),
                        '00000000-0000-0000-0000-000000000509'::uuid, NULL
                   FROM proxima_core.memory_head h
                  WHERE h.owner_id = $1
                    AND NOT EXISTS (SELECT 1 FROM proxima_core.memory m WHERE m.t = h.t)",
            )
            .bind(owner.into_inner())
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
                 SELECT m.t, uuidv7(), 'note', 'body', '{}'
                   FROM proxima_core.memory m
                  WHERE m.owner_id = $1
                    AND NOT EXISTS (SELECT 1 FROM proxima_core.agent_note_v1 s WHERE s.t = m.t)",
            )
            .bind(owner.into_inner())
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        }
        Ok(())
    }

    /// The reconcile reads the heads a page at a time, and a page boundary
    /// loses and repeats none: every uncovered head gets one job, and a limit
    /// that falls inside a page stops exactly there (#491).
    #[tokio::test]
    async fn reconcile_pages_across_the_heads_without_losing_any()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pool = pg.pool_for_tests();
            let owner = owner_fixture();
            seed_uncovered_heads(pool, &owner, 4_500).await?;

            let capped =
                reconcile_embeddings(pool, &platform, missing_only(2_500), stale_claim_seconds())
                    .await?;
            assert_eq!((capped.scanned, capped.enqueued), (2_500, 2_500));
            let all =
                reconcile_embeddings(pool, &platform, missing_only(10_000), stale_claim_seconds())
                    .await?;
            assert_eq!(
                (all.scanned, all.enqueued),
                (2_000, 2_000),
                "the heads the capped pass left, and none it took"
            );
            let (jobs, entities): (i64, i64) = sqlx::query_as(
                "SELECT count(*), count(DISTINCT entity_id)
                   FROM proxima_core.embedding_jobs",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!((jobs, entities), (4_500, 4_500));
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// A reconcile step reads a page of heads, not the store (#491): with
    /// 20,000 uncovered heads, the statement that starts after the 10,000th
    /// reads about a page of `memory` rows and scans none of the table.
    #[tokio::test]
    async fn a_reconcile_step_reads_one_page_of_heads() -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let pool = pg.pool_for_tests();
            let owner = owner_fixture();
            seed_uncovered_heads(pool, &owner, 20_000).await?;
            sqlx::query("VACUUM (ANALYZE) proxima_core.memory")
                .execute(pool)
                .await?;
            sqlx::query("VACUUM (ANALYZE) proxima_core.memory_head")
                .execute(pool)
                .await?;
            let cursor: Uuid = sqlx::query_scalar(
                "SELECT t FROM proxima_core.memory ORDER BY t OFFSET 10000 LIMIT 1",
            )
            .fetch_one(pool)
            .await?;

            let mut tx = pool.begin().await?;
            let explain = format!(
                "EXPLAIN (ANALYZE, FORMAT JSON) {}",
                super::super::reconcile_embeddings_sql_for_tests()
            );
            // SQL-POLICY: fixed-fragment — the production reconcile page statement.
            let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(explain))
                .bind(STUB_SPACE.model_id())
                .bind(1_000_000_i64)
                .bind("missing_only")
                .bind(None::<time::OffsetDateTime>)
                .bind(Vec::<String>::new())
                .bind(1024_i16)
                .bind(None::<Vec<Uuid>>)
                .bind(cursor)
                .bind(2_000_i64)
                .fetch_one(&mut *tx)
                .await?;
            tx.rollback().await?;
            let (memory_rows, memory_seq_scans) = memory_work(&plan);
            assert!(
                memory_rows <= 5_000.0,
                "the step read {memory_rows} rows of the memory tables, a page is 2,000; plan: {plan}"
            );
            assert_eq!(
                memory_seq_scans, 0,
                "the step scanned the table; plan: {plan}"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// The rows a plan read of the memory tables (kept or filtered away, over
    /// every loop) and its sequential scans of them.
    fn memory_work(plan: &serde_json::Value) -> (f64, usize) {
        let mut rows = 0.0;
        let mut seq_scans = 0;
        let mut stack = vec![plan];
        while let Some(value) = stack.pop() {
            match value {
                serde_json::Value::Array(items) => stack.extend(items),
                serde_json::Value::Object(node) => {
                    if node
                        .get("Relation Name")
                        .and_then(serde_json::Value::as_str)
                        == Some("memory")
                    {
                        let metric = |key: &str| {
                            node.get(key)
                                .and_then(serde_json::Value::as_f64)
                                .unwrap_or_default()
                        };
                        rows += (metric("Actual Rows") + metric("Rows Removed by Filter"))
                            * metric("Actual Loops").max(1.0);
                        if node.get("Node Type").and_then(serde_json::Value::as_str)
                            == Some("Seq Scan")
                        {
                            seq_scans += 1;
                        }
                    }
                    stack.extend(node.values());
                }
                _ => {}
            }
        }
        (rows, seq_scans)
    }

    #[tokio::test]
    async fn insert_memory_embedding_noops_after_source_memory_deleted()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
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
            let claims = claim_pending_embedding_jobs(pg.pool_for_tests(), 1, &[], &[]).await?;
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].entity_id, outcome.memory_id);
            assert_eq!(
                load_embedding_text(
                    pg.pool_for_tests(),
                    &owner,
                    EntityKind::Fact,
                    outcome.memory_id,
                    &[],
                    &core_embed_units(),
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
                &[stub_vector(embedding.clone())],
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
                    &core_embed_units(),
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

    async fn forget_by_hand(pool: &sqlx::PgPool, memory_id: MemoryId) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE proxima_core.memory SET forgotten_at = now() WHERE t = $1")
            .bind(memory_id.into_inner())
            .execute(pool)
            .await
            .map(|_| ())
    }

    /// #488 S6: a job claimed before its memory is forgotten writes nothing.
    /// The claim commits first (the barrier is the claim's own transaction),
    /// then the flag commits, then the drainer's write arrives.
    #[tokio::test]
    async fn a_claimed_job_writes_no_vector_once_its_memory_is_forgotten()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let memory = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("forgotten mid-job"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let claims = claim_pending_embedding_jobs(pg.pool_for_tests(), 1, &[], &[]).await?;
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].entity_id, memory.memory_id);
            forget_by_hand(pg.pool_for_tests(), memory.memory_id).await?;
            let entity = EmbeddableEntityRef::Memory {
                kind: EntityKind::Fact,
                memory_id: memory.memory_id,
            };
            let written = pg
                .insert_embedding(
                    &owner,
                    entity,
                    &[stub_vector(padded_embedding([0.4, 0.5, 0.6]))],
                    EmbeddingWriteProof::for_claim_for_tests(&claims[0]),
                )
                .await?;
            assert_eq!(written.embedding_version, 0, "the write is refused");
            assert_eq!(
                count_fact_embeddings(pg.pool_for_tests(), memory.memory_id).await?,
                0
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    fn claimed_writes<'a>(
        claims: &'a [proxima_core::EmbeddingJobClaim],
        vectors: &'a [SpaceVector],
    ) -> Vec<EmbeddingWrite<'a>> {
        claims
            .iter()
            .map(|claim| EmbeddingWrite {
                owner: &claim.owner,
                entity: EmbeddableEntityRef::Memory {
                    kind: claim.entity_kind,
                    memory_id: claim.entity_id,
                },
                vectors,
                proof: EmbeddingWriteProof::for_claim_for_tests(claim),
            })
            .collect()
    }

    /// A batch of claimed writes is one transaction: every vector lands and
    /// every claim completes, or a stale claim among them stops the batch
    /// before anything is written.
    #[tokio::test]
    async fn a_batch_write_lands_with_its_claims_or_not_at_all()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            for note in ["batch one", "batch two", "batch three"] {
                pg.ingest_fact_atomic(&permit, &fact_draft(note), Some("stub-fact-embed"))
                    .await?;
            }
            let pool = pg.pool_for_tests();
            let claims = claim_pending_embedding_jobs(pool, 3, &[], &[]).await?;
            assert_eq!(claims.len(), 3);
            let vectors = [stub_vector(padded_embedding([0.1, 0.2, 0.3]))];
            // The last claim lost its job to another drainer.
            let mut stale = claims.clone();
            stale[2].claim_token = Uuid::now_v7();
            let refused = pg
                .insert_embeddings(&claimed_writes(&stale, &vectors))
                .await;
            assert!(
                matches!(refused, Err(StorageError::Conflict(_))),
                "a stale claim stops the batch: {refused:?}"
            );
            let vectors_written: i64 =
                sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.embeddings")
                    .fetch_one(pool)
                    .await?;
            assert_eq!(vectors_written, 0, "nothing of the batch was written");
            let processing: i64 = sqlx::query_scalar(
                "SELECT count(*)::bigint FROM proxima_core.embedding_jobs
                  WHERE status = 'processing'",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!(processing, 3, "no claim of the batch was completed");

            let outcomes = pg
                .insert_embeddings(&claimed_writes(&claims, &vectors))
                .await?;
            assert_eq!(
                outcomes
                    .iter()
                    .map(|outcome| outcome.embedding_version)
                    .collect::<Vec<_>>(),
                [1, 1, 1]
            );
            let (vectors_written, jobs_left): (i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM proxima_core.embeddings),
                        (SELECT count(*) FROM proxima_core.embedding_jobs)",
            )
            .fetch_one(pool)
            .await?;
            assert_eq!((vectors_written, jobs_left), (3, 0));
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// #488 S6: the drain does not claim a forgotten memory's pending job.
    #[tokio::test]
    async fn the_drain_does_not_claim_a_forgotten_memorys_job()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let memory = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("forgotten before claim"),
                    Some("stub-fact-embed"),
                )
                .await?;
            forget_by_hand(pg.pool_for_tests(), memory.memory_id).await?;
            assert!(
                claim_pending_embedding_jobs(pg.pool_for_tests(), 1, &[], &[])
                    .await?
                    .is_empty()
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// A provider that rejects an input will reject it again. Requeueing such a
    /// job spins claim → reject → requeue forever, which is why the two failure
    /// causes need separate statuses.
    #[tokio::test]
    async fn permanently_failed_job_is_never_requeued() -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let platform = pg.platform_scope_for_host();
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

            let claims = claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
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
                reconcile_embeddings(pool, &platform, missing_only(100), stale_claim_seconds())
                    .await?;
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
        let platform = pg.platform_scope_for_host();
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

            let claims = claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].entity_id, permanently_rejected.memory_id);
            fail_embedding_job_permanently(pool, &claims[0], "provider rejects forever").await?;

            let reconciled = reconcile_embeddings(
                pool,
                &platform,
                EmbeddingReconcileOptions {
                    space: &STUB_SPACE,
                    owners: None,
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

    #[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
    struct BackfillJobSnapshot {
        job_id: Uuid,
        status: String,
        last_error: Option<String>,
        claim_token: Option<Uuid>,
        unclaimed: bool,
    }

    #[derive(Debug)]
    struct BackfillProgress {
        enqueued: Vec<usize>,
        permanent_before: BackfillJobSnapshot,
        permanent_after: BackfillJobSnapshot,
        other_model_before: BackfillJobSnapshot,
        other_model_after: BackfillJobSnapshot,
        later_target_jobs: i64,
        foreign_owner_jobs: i64,
    }

    /// A permanent rejection must not consume every bounded owner backfill
    /// pass while a later, still-unqueued Fact needs the active model.
    #[tokio::test]
    async fn owner_backfill_skips_permanently_failed_jobs_before_its_limit()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
        let observed = observe_owner_backfill_progress(&pg).await;
        drop(pg);
        let cleanup = drop_db(&db_name).await;
        cleanup?;
        let observed = observed?;
        assert_eq!(observed.permanent_before, observed.permanent_after);
        assert_eq!(observed.permanent_after.status, "failed_permanent");
        assert_eq!(observed.other_model_before, observed.other_model_after);
        assert_eq!(observed.other_model_after.status, "pending");
        assert_eq!(observed.foreign_owner_jobs, 0);
        assert_eq!(
            observed.enqueued,
            [1, 0, 0],
            "an existing permanent failure must not hide the later missing job"
        );
        assert_eq!(observed.later_target_jobs, 1);
        Ok(())
    }

    async fn observe_owner_backfill_progress(
        pg: &crate::PgStorage,
    ) -> Result<BackfillProgress, Box<dyn std::error::Error>> {
        let owner = owner_fixture();
        let foreign_owner = OwnerId::from(proxima_core::UserId::new(Uuid::now_v7()));
        let foreign = backfill_note(pg, &foreign_owner, "foreign earlier Fact", None).await?;
        let earlier = backfill_note(
            pg,
            &owner,
            "permanent earlier Fact",
            Some("stub-fact-embed"),
        )
        .await?;
        let later = backfill_note(
            pg,
            &owner,
            "later missing target model",
            Some("other-model"),
        )
        .await?;
        if foreign.into_inner() >= earlier.into_inner()
            || earlier.into_inner() >= later.into_inner()
        {
            return Err(
                "backfill fixture Facts must be ordered by their real admission IDs".into(),
            );
        }
        let pool = pg.pool_for_tests();
        let claims = claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
        if claims.len() != 1 || claims[0].entity_id != earlier {
            return Err("the real claim must target only the earlier Fact".into());
        }
        fail_embedding_job_permanently(pool, &claims[0], "provider rejects this input forever")
            .await?;
        let permanent_before = backfill_job_snapshot(pool, earlier, "stub-fact-embed").await?;
        let other_model_before = backfill_job_snapshot(pool, later, "other-model").await?;
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let configured_pg = pg.clone().with_flavors(&registry);
        let engine = Engine::new(registry)
            .with_storage_ports(Arc::new(configured_pg).storage_ports())
            .with_embedding_router(routed(RecordingBatchEmbedding {
                batch_widths: Arc::default(),
            }));
        let user_id = owner.as_user();
        let authz = authenticated_context(AuthzContext::for_subject(user_id, AuthPath::HostBearer));
        let mut enqueued = Vec::new();
        for _ in 0..3 {
            enqueued.push(
                engine
                    .backfill_missing_embeddings(&authz, &owner, 1)
                    .await?,
            );
        }
        Ok(BackfillProgress {
            enqueued,
            permanent_before,
            permanent_after: backfill_job_snapshot(pool, earlier, "stub-fact-embed").await?,
            other_model_before,
            other_model_after: backfill_job_snapshot(pool, later, "other-model").await?,
            later_target_jobs: sqlx::query_scalar(
                "SELECT count(*) FROM proxima_core.embedding_jobs
                  WHERE owner_id = $1 AND entity_id = $2 AND model_id = 'stub-fact-embed'",
            )
            .bind(owner.into_inner())
            .bind(later.into_inner())
            .fetch_one(pool)
            .await?,
            foreign_owner_jobs: count_jobs(pool, foreign.into_inner()).await?,
        })
    }

    async fn backfill_note(
        pg: &crate::PgStorage,
        owner: &OwnerId,
        text: &str,
        model: Option<&str>,
    ) -> Result<MemoryId, StorageError> {
        let mut draft = fact_draft(text);
        draft.schema_id = SchemaId::new("core/agent-note-v1".into());
        let spaces: Vec<EmbeddingSpace> = model
            .map(|model| EmbeddingSpace::new(model, EmbeddingDim::D1024))
            .into_iter()
            .collect();
        let written = ingest_note_fact(pg, owner, &draft, &spaces, text, text).await?;
        let loaded = load_embedding_text(
            pg.pool_for_tests(),
            owner,
            EntityKind::Fact,
            written.memory_id,
            &[],
            &core_embed_units(),
        )
        .await?;
        if loaded.is_none_or(|text| text.is_empty()) {
            return Err(StorageError::Internal(
                "backfill fixture must have real embedding text".into(),
            ));
        }
        Ok(written.memory_id)
    }

    async fn backfill_job_snapshot(
        pool: &sqlx::PgPool,
        memory_id: MemoryId,
        model: &str,
    ) -> Result<BackfillJobSnapshot, sqlx::Error> {
        sqlx::query_as(
            "SELECT job_id, status::text, last_error, claim_token, claimed_at IS NULL AS unclaimed
               FROM proxima_core.embedding_jobs WHERE entity_id = $1 AND model_id = $2",
        )
        .bind(memory_id.into_inner())
        .bind(model)
        .fetch_one(pool)
        .await
    }

    #[tokio::test]
    async fn reconcile_cannot_recreate_job_after_concurrent_forget()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let platform = pg.platform_scope_for_host();
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
                        &platform,
                        EmbeddingReconcileOptions {
                            space: &STUB_SPACE,
                            owners: None,
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

            forget_memory(
                &mut forget_tx,
                &crate::core_pg_sidecars(),
                &proxima_core::owner_inverse::OwnerSurfaces::for_registry(
                    &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
                ),
                t,
                owner.into_inner(),
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

    #[tokio::test]
    async fn claim_renewal_prevents_reclaim_without_changing_fence()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let platform = pg.platform_scope_for_host();
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
            let claims = claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
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
                local_observability(pool, &platform)
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
        let _platform = pg.platform_scope_for_host();
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
            let old_claims = claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
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

            let successor_claims = claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
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
    async fn source_claim_cannot_finalize_a_job_after_owner_transfer()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let source = owner_fixture();
            let permit = owner_fact_write_permit(&source).await?;
            let written = pg
                .ingest_fact_atomic(
                    &permit,
                    &fact_draft("claim crosses transfer"),
                    Some("stub-fact-embed"),
                )
                .await?;
            let pool = pg.pool_for_tests();
            let claims = claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
            let stale = claims[0].clone();
            let destination = OwnerId::from(GroupId::new(Uuid::now_v7()));
            let transfer_permit =
                OwnerWritePermit::transfer_for_tests(source, AccessKind::Fact, destination);
            let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
            let surfaces = proxima_core::owner_inverse::OwnerSurfaces::for_registry(&registry);
            let mut wake = proxima_core::write_wake::WriteWake::subscribe();

            assert!(
                pg.transfer_to_owner(
                    &transfer_permit,
                    EntityId::Memory(written.memory_id),
                    destination,
                    &surfaces,
                    std::slice::from_ref(&*STUB_SPACE),
                )
                .await?,
                "the transfer moves the processing job with its Memory"
            );
            // The transfer enqueues the destination's series-head job: it
            // wakes the embedding drain like any other enqueuing write.
            tokio::time::timeout(std::time::Duration::from_secs(1), wake.changed())
                .await
                .map_err(|_| "a committed transfer wakes the drains")?;
            let (job_owner, job_token): (Uuid, Option<Uuid>) = sqlx::query_as(
                "SELECT owner_id, claim_token
                   FROM proxima_core.embedding_jobs
                  WHERE job_id = $1",
            )
            .bind(stale.job_id)
            .fetch_one(pool)
            .await?;
            assert_eq!(job_owner, destination.into_inner());
            assert_eq!(job_token, Some(stale.claim_token));

            assert_eq!(
                renew_embedding_jobs(pool, std::slice::from_ref(&stale)).await?,
                0,
                "a source-owner heartbeat cannot renew the destination's job"
            );
            for transition in [
                complete_embedding_job(pool, &stale).await,
                fail_embedding_job(pool, &stale, "stale source failure").await,
                release_embedding_jobs(pool, std::slice::from_ref(&stale), "stale source release")
                    .await,
            ] {
                assert!(
                    matches!(transition, Err(StorageError::Conflict(_))),
                    "a pre-transfer claim cannot mutate the rehomed job: {transition:?}"
                );
            }

            sqlx::query(
                "UPDATE proxima_core.embedding_jobs
                    SET claimed_at = now() - make_interval(secs => $2::double precision)
                  WHERE job_id = $1",
            )
            .bind(stale.job_id)
            .bind(f64::from(u32::try_from(stale_claim_seconds())?) * 2.0)
            .execute(pool)
            .await?;
            assert_eq!(
                reclaim_stale_embedding_jobs(pool, stale_claim_seconds()).await?,
                1,
                "the destination can reclaim the abandoned source claim"
            );
            let successor = claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
            assert_eq!(successor.len(), 1);
            assert_eq!(successor[0].owner, destination);
            insert_claimed_fact_embedding(&pg, &successor[0], [0.4, 0.5, 0.6]).await?;
            assert_eq!(count_fact_embeddings(pool, written.memory_id).await?, 1);
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
        let platform = pg.platform_scope_for_host();
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
            claim_pending_embedding_jobs(pool, 1, &[], &[]).await?;
            sqlx::query(
                "UPDATE proxima_core.embedding_jobs
                    SET claimed_at = now() - make_interval(secs => $2::double precision)
                  WHERE entity_id = $1",
            )
            .bind(entity_id)
            .bind(f64::from(u32::try_from(stale_claim_seconds())?) * 2.0)
            .execute(pool)
            .await?;

            reconcile_embeddings(pool, &platform, missing_only(100), stale_claim_seconds()).await?;
            assert_eq!(job_state(pool, entity_id).await?.0, "pending");
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// A sidecar may key its memory on a column of its own naming, and the
    /// embedding drain still finds its text.
    ///
    /// The twin of the projection lane's renamed-key fixture. The drain is
    /// driven by `EmbeddingRecipe::resolve`, which binds a unit to its
    /// sidecar TABLE and never sees a `Surface`, so before
    /// `MemoryEmbedUnit::key_column` existed both statements here spelled
    /// the memory column `t`: a flavor that keyed its sidecar on anything
    /// else got no text, its job completed with nothing embedded, and the
    /// memory was semantically invisible with no error anywhere.
    ///
    /// The unit is hand-built rather than frozen from a contract for the
    /// reason the projection fixture states: reaching this statement from a
    /// registration would exercise the registry, and the statement is what
    /// is under test. Freeze's own refusal for a unit whose sidecar
    /// declares no memory key is `EmbeddedSidecarNotMemoryKeyed`.
    #[tokio::test]
    async fn the_drain_reads_text_through_the_declared_memory_key_column()
    -> Result<(), Box<dyn std::error::Error>> {
        const RENAMED_TABLE: &str = "proxima_core.renamed_embed_note_v1";
        const RENAMED_KEY: &str = "note_memory_id";

        let (pg, db_name) = fresh_pg("proxima_spg_embed_key").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let permit = owner_fact_write_permit(&owner).await?;
            let outcome = pg
                .ingest_fact_atomic(&permit, &fact_draft("renamed key note"), None)
                .await?;
            let pool = pg.pool_for_tests();

            sqlx::query(
                "CREATE TABLE proxima_core.renamed_embed_note_v1 (
                     note_memory_id uuid PRIMARY KEY
                                    REFERENCES proxima_core.memory (t) ON DELETE CASCADE,
                     embed_text     text NOT NULL
                 )",
            )
            .execute(pool)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.renamed_embed_note_v1 (note_memory_id, embed_text)
                 VALUES ($1, 'the pilings under the north quay are sound')",
            )
            .bind(outcome.memory_id.into_inner())
            .execute(pool)
            .await?;

            let unit = |key_column: &str| MemoryEmbedUnit {
                schema_id: SchemaId::new("proxima-test/fact-embedding-v1".into()),
                schema_version: SchemaVersion::new(1),
                kind: proxima_core::verbs::schema::PayloadKind::Fact,
                sidecar_table: RENAMED_TABLE.to_owned(),
                column: "embed_text".to_owned(),
                key_column: key_column.to_owned(),
            };
            let items = [(owner, EntityKind::Fact, outcome.memory_id)];

            let batched = load_embedding_texts(pool, &items, &[], &[unit(RENAMED_KEY)]).await?;
            assert_eq!(
                batched,
                vec![Some(
                    "the pilings under the north quay are sound".to_owned()
                )],
                "the batch read filters on the column the unit declares"
            );

            let single = load_embedding_text(
                pool,
                &owner,
                EntityKind::Fact,
                outcome.memory_id,
                &[],
                &[unit(RENAMED_KEY)],
            )
            .await?;
            assert_eq!(
                single,
                Some("the pilings under the north quay are sound".to_owned()),
                "and so does the single-row read behind it"
            );

            // The control. `t` is what both statements used to spell
            // unconditionally, and against this table it is not a column at
            // all: without it the assertions above could pass on a fixture
            // that happened to be keyed on `t` after all.
            let wrong = load_embedding_texts(pool, &items, &[], &[unit("t")]).await;
            assert!(
                wrong.is_err(),
                "spelling the key `t` reaches no column of a sidecar keyed otherwise"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// Refuses whole inputs over `max_chars` the way a client-side cap
    /// does, and records every length offered — so a test can assert what
    /// was *sent*, not merely what came back.
    #[derive(Debug)]
    struct CappedEmbedding {
        max_chars: usize,
        model_id: &'static str,
        provider: Arc<AdmissionProvider>,
    }

    #[derive(Debug, Default)]
    struct AdmissionProvider {
        enabled: std::sync::atomic::AtomicBool,
        offered: std::sync::Mutex<Vec<usize>>,
    }

    impl AdmissionProvider {
        fn open_for_drain(&self) {
            self.enabled
                .store(true, std::sync::atomic::Ordering::SeqCst);
        }

        fn offered(&self) -> Vec<usize> {
            self.offered.lock().expect("provider recording").clone()
        }
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for CappedEmbedding {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
            assert!(
                self.provider
                    .enabled
                    .load(std::sync::atomic::Ordering::SeqCst),
                "memory admission must not request a provider vector"
            );
            let chars = text.chars().count();
            self.provider
                .offered
                .lock()
                .expect("provider recording")
                .push(chars);
            if chars > self.max_chars {
                return Err(LlmError::InputRefused(EmbeddingInputRefusal::LocalCap {
                    actual_chars: chars,
                    max_chars: self.max_chars,
                }));
            }
            Ok(padded_embedding([0.5, 0.6, 0.7]))
        }

        fn model_id(&self) -> &'static str {
            self.model_id
        }

        fn dim(&self) -> usize {
            EMBEDDING_DIM
        }
    }

    fn derived_request(
        owner: OwnerId,
        origins: &[MemoryId],
        text: String,
    ) -> Result<DerivedMemory, ProtocolError> {
        derived_request_at(
            owner,
            origins,
            text,
            MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
        )
    }

    fn derived_request_at(
        owner: OwnerId,
        origins: &[MemoryId],
        text: String,
        target: MemoryTarget,
    ) -> Result<DerivedMemory, ProtocolError> {
        DerivedMemory::abstraction(
            target,
            owner,
            text.clone(),
            AgentDerivationV1 {
                title: "long derivation".into(),
                body: text,
                tags: Vec::new(),
                idempotency_key: None,
                source_memory_ids: origins.iter().copied().map(MemoryId::into_inner).collect(),
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "1".into(),
            },
            origins.iter().copied(),
            DerivationIdentity::new(
                OperatorId::new(Uuid::now_v7()),
                InputContractId::new(Uuid::now_v7()),
            ),
        )
        .map(|memory| memory.lexical_language(LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT))
    }

    /// Engine over this storage with a capped provider, plus one origin
    /// Fact for the derived write to declare.
    async fn capped_authoring_fixture(
        pg: &crate::PgStorage,
        owner: &OwnerId,
        max_chars: usize,
    ) -> Result<(Engine, AuthzContext, MemoryId, Arc<AdmissionProvider>), Box<dyn std::error::Error>>
    {
        let user_id = owner.as_user();
        let mut draft = fact_draft("origin");
        draft.schema_id = SchemaId::new("core/agent-note-v1".into());
        let origin = ingest_note_fact(pg, owner, &draft, &[], "origin", "origin").await?;
        let provider = Arc::new(AdmissionProvider::default());
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let configured_pg = pg.clone().with_flavors(&registry);
        let engine = Engine::new(registry)
            .with_storage_ports(Arc::new(configured_pg).storage_ports())
            .with_embedding_router(routed(CappedEmbedding {
                max_chars,
                model_id: "stub-fact-embed",
                provider: provider.clone(),
            }));
        let authz = authenticated_context(AuthzContext::for_subject(user_id, AuthPath::HostBearer));
        Ok((engine, authz, origin.memory_id, provider))
    }

    async fn count_jobs(pool: &sqlx::PgPool, entity_id: Uuid) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT count(*)::bigint
               FROM proxima_core.embedding_jobs
              WHERE entity_id = $1",
        )
        .bind(entity_id)
        .fetch_one(pool)
        .await
    }

    /// A long derived unit is admitted with a durable job before any provider
    /// request. The drain retains complete, ordered chunk rescue.
    #[tokio::test]
    async fn author_derived_queues_before_the_provider_then_drains_all_chunks()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let cap = MIN_EMBED_INPUT_CAP_CHARS;
            let (engine, authz, origin, provider) =
                capped_authoring_fixture(&pg, &owner, cap).await?;
            let origins = [origin];
            let pool = pg.pool_for_tests();

            let outcome = engine
                .derive_memory(
                    &authz,
                    derived_request(owner, &origins, "a".repeat(cap * 3))?,
                )
                .await?;

            assert!(
                outcome.embedding_deferred,
                "an admitted embeddable text reports its queued work"
            );
            assert_eq!(
                count_jobs(pool, outcome.memory_id.into_inner()).await?,
                1,
                "the memory and its job land together"
            );
            assert_eq!(
                load_embedding_head_version(
                    pool,
                    EntityKind::Abstraction,
                    outcome.memory_id.into_inner(),
                    "stub-fact-embed"
                )
                .await?,
                None,
                "admission publishes no provider vector"
            );
            assert!(provider.offered().is_empty());
            provider.open_for_drain();
            let drained = engine.drain_embedding_jobs(1).await?;
            assert_eq!((drained.processed, drained.failed), (1, 0));
            assert_eq!(count_jobs(pool, outcome.memory_id.into_inner()).await?, 0);
            let offered = provider.offered();
            assert!(
                offered.iter().any(|chars| *chars > cap),
                "the whole text is offered first: {offered:?}"
            );
            let accepted = offered.iter().filter(|chars| **chars <= cap).count();
            assert!(
                accepted > 1,
                "the text came back split into provider-acceptable pieces: {offered:?}"
            );
            assert_eq!(
                count_fact_embeddings(pool, outcome.memory_id).await?,
                i64::try_from(accepted)?,
                "every successful provider chunk is stored in the drained version"
            );
            assert_chunk_ordinals(pool, outcome.memory_id, accepted).await?;
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// Ordinary derived admission never calls the provider. Queued text is
    /// embedded by the drain after the row has committed.
    #[tokio::test]
    async fn author_derived_queues_the_vector_without_a_mode_choice()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let cap = MIN_EMBED_INPUT_CAP_CHARS;
            let (engine, authz, origin, provider) =
                capped_authoring_fixture(&pg, &owner, cap).await?;
            let origins = [origin];
            let pool = pg.pool_for_tests();

            let outcome = engine
                .derive_memory(
                    &authz,
                    derived_request(owner, &origins, "q".repeat(cap * 3))?,
                )
                .await?;

            assert!(outcome.embedding_deferred, "the write reports the queue");
            assert!(
                provider.offered().is_empty(),
                "no provider call before the write"
            );
            assert_eq!(
                job_state(pool, outcome.memory_id.into_inner()).await?,
                ("pending".to_owned(), None, true),
                "the job is enqueued with the row"
            );
            provider.open_for_drain();
            let drained = engine.drain_embedding_jobs(1).await?;
            assert_eq!((drained.processed, drained.failed), (1, 0));
            let accepted = provider
                .offered()
                .iter()
                .filter(|chars| **chars <= cap)
                .count();
            assert!(accepted > 1, "the queued text requires multiple chunks");
            assert_eq!(
                count_fact_embeddings(pool, outcome.memory_id).await?,
                i64::try_from(accepted)?
            );
            assert_chunk_ordinals(pool, outcome.memory_id, accepted).await?;
            assert_eq!(
                load_embedding_head_version(
                    pool,
                    EntityKind::Abstraction,
                    outcome.memory_id.into_inner(),
                    "stub-fact-embed"
                )
                .await?,
                Some(1),
                "the drain landed the vector"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// A provider's eventual input refusal cannot affect admission. Only the
    /// drain classifies a queued text that is refused at every segment length.
    #[tokio::test]
    async fn author_derived_defers_text_refused_at_every_length()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let _platform = pg.platform_scope_for_host();
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            // Refuses every segment the minimum split floor can produce.
            let (engine, authz, origin, provider) =
                capped_authoring_fixture(&pg, &owner, 1).await?;
            let origins = [origin];
            let pool = pg.pool_for_tests();

            let outcome = engine
                .derive_memory(
                    &authz,
                    derived_request(owner, &origins, "a".repeat(CHUNKED_EMBED_MIN_BYTES * 3))?,
                )
                .await?;

            assert!(
                outcome.embedding_deferred,
                "a text refused at every length still defers to a job"
            );
            assert_eq!(
                job_state(pool, outcome.memory_id.into_inner()).await?,
                ("pending".to_owned(), None, true),
                "the job is enqueued with the row"
            );
            assert_eq!(
                load_embedding_head_version(
                    pool,
                    EntityKind::Abstraction,
                    outcome.memory_id.into_inner(),
                    "stub-fact-embed"
                )
                .await?,
                None,
                "no vector was written"
            );
            assert!(provider.offered().is_empty());
            provider.open_for_drain();
            let drained = engine.drain_embedding_jobs(1).await?;
            assert_eq!((drained.processed, drained.failed), (1, 1));
            assert_eq!(
                job_state(pool, outcome.memory_id.into_inner()).await?.0,
                "failed_permanent"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }
    fn interpretation_request(owner: OwnerId, subject: MemoryId) -> DerivedMemory {
        DerivedMemory::interpretation(
            MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
            owner,
            "a grounded claim",
            proxima_core::InterpretationV1 {
                claim: "a grounded claim".into(),
                confidence: 80,
                subject_memory_ids: vec![subject.into_inner()],
                subject_kinds: vec![proxima_core::InterpretationSubjectKind::Abstraction],
                model_id: "test".into(),
                client_name: "test".into(),
                client_version: "1".into(),
            },
        )
    }

    async fn memory_count(pool: &sqlx::PgPool) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT count(*)::bigint FROM proxima_core.memory")
            .fetch_one(pool)
            .await
    }

    async fn assert_pending_admissions(
        pool: &sqlx::PgPool,
        outcomes: &[proxima_core::DerivedMemoryOutcome],
    ) -> Result<(), Box<dyn std::error::Error>> {
        for outcome in outcomes {
            assert!(outcome.embedding_deferred);
            assert!(!outcome.idempotent_replay);
            assert_eq!(
                job_state(pool, outcome.memory_id.into_inner()).await?,
                ("pending".into(), None, true)
            );
            assert_eq!(count_fact_embeddings(pool, outcome.memory_id).await?, 0);
        }
        Ok(())
    }

    #[tokio::test]
    async fn author_derived_batch_revision_and_open_units_never_call_provider()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_admission").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let (engine, authz, origin, provider) =
                capped_authoring_fixture(&pg, &owner, MIN_EMBED_INPUT_CAP_CHARS).await?;
            let pool = pg.pool_for_tests();
            let request = derived_request(owner, &[origin], "first conclusion".into())?;
            let first = engine.derive_memory(&authz, request.clone()).await?;
            let revision = engine
                .derive_memory(
                    &authz,
                    derived_request_at(
                        owner,
                        &[origin],
                        "revised conclusion".into(),
                        MemoryTarget::Revision(first.memory_id),
                    )?,
                )
                .await?;
            assert_ne!(first.memory_id, revision.memory_id);
            assert_eq!((first.edge_count, revision.edge_count), (1, 1));
            let replay = engine.derive_memory(&authz, request).await?;
            assert!(replay.idempotent_replay);
            assert!(!replay.embedding_deferred, "replay queues no new work");
            assert_eq!(replay.memory_id, revision.memory_id);

            let mut unit = engine.unit_of_work(&authz).await?;
            let batch = unit
                .derive_memories([
                    derived_request(owner, &[origin], "batch one".into())?,
                    derived_request(owner, &[origin], "batch two".into())?,
                ])
                .await?;
            unit.commit().await?;
            assert!(batch.iter().all(|outcome| outcome.edge_count == 1));

            let mut unit = engine.unit_of_work(&authz).await?;
            unit.advisory_xact_lock(487).await?;
            let open = unit
                .derive_memory(derived_request(
                    owner,
                    &[origin],
                    "open transaction".into(),
                )?)
                .await?;
            let claim = unit
                .derive_memory(interpretation_request(owner, open.memory_id))
                .await?;
            assert_eq!((open.edge_count, claim.edge_count), (1, 1));
            unit.commit().await?;
            let mut accepted = vec![first, revision, open, claim];
            accepted.extend(batch);
            assert_pending_admissions(pool, &accepted).await?;

            let mut rollback = engine.unit_of_work(&authz).await?;
            let abandoned = rollback
                .derive_memory(derived_request(owner, &[origin], "rolled back".into())?)
                .await?;
            assert!(abandoned.embedding_deferred);
            drop(rollback);
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM proxima_core.memory WHERE t = $1)",
            )
            .bind(abandoned.memory_id.into_inner())
            .fetch_one(pool)
            .await?;
            assert!(!exists);
            assert_eq!(count_jobs(pool, abandoned.memory_id.into_inner()).await?, 0);
            assert!(provider.offered().is_empty());
            provider.open_for_drain();
            let drained = engine.drain_embedding_jobs(16).await?;
            assert_eq!((drained.processed, drained.failed), (accepted.len(), 0));
            for outcome in accepted {
                assert_eq!(count_fact_embeddings(pool, outcome.memory_id).await?, 1);
                assert_eq!(count_jobs(pool, outcome.memory_id.into_inner()).await?, 0);
            }
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    fn moving_admission_router(
        owner: OwnerId,
        provider: &Arc<AdmissionProvider>,
    ) -> Result<Arc<proxima_core::test_fixtures::TestEmbeddingRouter>, Box<dyn std::error::Error>>
    {
        let client = |model_id| {
            proxima_core::BoundEmbeddingClient::bind(Arc::new(CappedEmbedding {
                max_chars: MIN_EMBED_INPUT_CAP_CHARS,
                model_id,
                provider: provider.clone(),
            }))
        };
        let router = Arc::new(proxima_core::test_fixtures::TestEmbeddingRouter::default());
        router.set_route(
            owner,
            proxima_core::EmbeddingRoute::moving(
                Some(client("stub-fact-embed")?),
                client("next-fact-embed")?,
            )?,
        );
        Ok(router)
    }

    #[tokio::test]
    async fn author_derived_and_fact_queue_both_spaces_but_schema_and_no_route_queue_none()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_admission").await;
        let result: Result<(), Box<dyn std::error::Error>> =
            async {
                let owner = owner_fixture();
                let (engine, authz, origin, provider) =
                    capped_authoring_fixture(&pg, &owner, MIN_EMBED_INPUT_CAP_CHARS).await?;
                let router = moving_admission_router(owner, &provider)?;
                let moving = proxima_core::EmbeddingRouter::route(router.as_ref(), &owner).await?;
                let engine = engine.with_embedding_router(router.clone());
                let derived = engine
                    .derive_memory(
                        &authz,
                        derived_request(owner, &[origin], "moving conclusion".into())?,
                    )
                    .await?;
                assert!(derived.embedding_deferred);
                let payload = proxima_core::AgentNoteV1 {
                    note_id: Uuid::now_v7(),
                    title: "moving fact".into(),
                    body: "a fact".into(),
                    tags: Vec::new(),
                    idempotency_key: None,
                };
                let fact = engine
                    .ingest_fact(
                        &authz,
                        proxima_core::FactWrite::new(owner, "admission", &payload),
                    )
                    .await?;
                let pool = pg.pool_for_tests();
                for memory_id in [derived.memory_id, fact.memory_id] {
                    let jobs: Vec<(String, i64, String)> = sqlx::query_as(
                    "SELECT model_id, dim::bigint, status::text FROM proxima_core.embedding_jobs
                     WHERE entity_id = $1 ORDER BY model_id",
                ).bind(memory_id.into_inner()).fetch_all(pool).await?;
                    assert_eq!(
                        jobs,
                        vec![
                            ("next-fact-embed".into(), 1024, "pending".into()),
                            ("stub-fact-embed".into(), 1024, "pending".into()),
                        ]
                    );
                }
                let episode = proxima_core::WriteActV1 {
                    episode_id: Uuid::now_v7(),
                };
                let no_embed = engine
                    .ingest_fact(
                        &authz,
                        proxima_core::FactWrite::new(owner, "admission", &episode),
                    )
                    .await?;
                assert_eq!(count_jobs(pool, no_embed.memory_id.into_inner()).await?, 0);
                router.set_owner(owner, None);
                let no_route = engine
                    .derive_memory(
                        &authz,
                        derived_request(owner, &[origin], "lexical only".into())?,
                    )
                    .await?;
                assert!(!no_route.embedding_deferred);
                assert_eq!(count_jobs(pool, no_route.memory_id.into_inner()).await?, 0);
                router.refuse(owner, "route policy refuses this owner");
                let before = memory_count(pool).await?;
                assert!(engine.derive_memory(&authz,
                derived_request(owner, &[origin], "refused".into())?,
            ).await.is_err());
                assert_eq!(memory_count(pool).await?, before);
                assert!(provider.offered().is_empty());
                router.set_route(owner, moving);
                provider.open_for_drain();
                let drained = engine.drain_embedding_jobs(16).await?;
                assert_eq!((drained.processed, drained.failed), (4, 0));
                Ok(())
            }
            .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    #[tokio::test]
    async fn author_derived_job_failure_rolls_back_memory_provenance_and_sidecar()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_admission").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let (engine, authz, origin, provider) =
                capped_authoring_fixture(&pg, &owner, MIN_EMBED_INPUT_CAP_CHARS).await?;
            let pool = pg.pool_for_tests();
            sqlx::query("CREATE FUNCTION public.refuse_admission_job() RETURNS trigger
                         LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'job insertion refused'; END $$")
                .execute(pool).await?;
            sqlx::query("CREATE TRIGGER refuse_admission_job BEFORE INSERT
                         ON proxima_core.embedding_jobs FOR EACH ROW
                         EXECUTE FUNCTION public.refuse_admission_job()")
                .execute(pool).await?;
            let before: (i64, i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM proxima_core.memory),
                        (SELECT count(*) FROM proxima_core.agent_derivation_v1),
                        (SELECT count(*) FROM proxima_core.content),
                        (SELECT count(*) FROM proxima_core.embedding_jobs)",
            ).fetch_one(pool).await?;
            let refused = engine.derive_memory(&authz,
                derived_request(owner, &[origin], "atomic conclusion".into())?,
            ).await;
            let error = refused.expect_err("the job trigger must refuse this admission");
            assert!(error.message.contains("job insertion refused"), "{error}");
            let after: (i64, i64, i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM proxima_core.memory),
                        (SELECT count(*) FROM proxima_core.agent_derivation_v1),
                        (SELECT count(*) FROM proxima_core.content),
                        (SELECT count(*) FROM proxima_core.embedding_jobs)",
            ).fetch_one(pool).await?;
            assert_eq!(after, before, "job failure rolls back the entire admission");
            assert!(provider.offered().is_empty());
            Ok(())
        }.await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }
}
