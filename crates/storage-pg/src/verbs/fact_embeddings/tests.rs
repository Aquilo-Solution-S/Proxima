// Raw-owner write behavior tests live in-crate: `insert_embedding` /
// `insert_memory_embedding` are `pub(crate)` (below the proof gate), so
// external test binaries cannot reach them without a forgeable-proof surface.
#[cfg(test)]
mod pg_tests {
    use std::sync::Arc;

    use proxima_core::llm::{EmbeddingClient, EmbeddingRuntimePolicy, LlmError};
    use proxima_core::storage_ports::{
        EmbeddingWritePort, EmbeddingWriteProof, OwnerTransferPort, OwnerWritePermit,
    };
    use proxima_core::test_fixtures::owner_fixture;
    use proxima_core::verbs::fact_ingest::{FactReceiptDraft, FactWriteCommand};
    use proxima_core::verbs::schema::MemoryEmbedUnit;
    use proxima_core::{
        AccessKind, AuthPath, AuthzContext, Engine, EntityId, EntityKind, FactIngestPort,
        FlavorRegistry, GroupId, InputContractId, MemoryTarget, Owner, ProtocolError, SchemaId,
        SchemaVersion, SourceId, StorageError,
    };
    use proxima_pg_testkit::drop_db;
    use uuid::Uuid;

    use proxima_core::EmbeddableEntityRef;
    use proxima_core::lexical_language::LEXICAL_LANGUAGE_DEPLOYMENT_DEFAULT;
    use proxima_core::llm::{
        CHUNKED_EMBED_MIN_BYTES, EMBED_LIVENESS_PROBE, MIN_EMBED_INPUT_CAP_CHARS,
    };
    use proxima_core::llm::{EmbeddingDim, SingleClientRouter};
    use proxima_core::{
        AgentDerivationV1, DerivationIdentity, DerivedMemory, MemoryId, OperatorId, SeriesHandle,
    };
    use proxima_core::{EmbeddingSpace, SpaceVector};

    use super::super::{
        EmbeddingReconcileOptions, EmbeddingReconcileScope, claim_pending_embedding_jobs,
        complete_embedding_job, count_embedding_job_status, embedding_ann_observability,
        fail_embedding_job, fail_embedding_job_permanently, insert_memory_embedding,
        load_embedding_text, load_embedding_texts, reclaim_stale_embedding_jobs,
        reconcile_embeddings, release_embedding_jobs, renew_embedding_jobs,
    };
    use crate::core_pg_sidecars;
    use crate::test_fixtures::fresh_pg;
    use crate::verbs::forget::{MemoryColdStore, cold_object_key, forget_memory};

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
                observed_at: now,
                occurred_at: now,
            }),
            citation: None,
            additional_references: Vec::new(),
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

    const AGENT_NOTE: &str = "proxima_core.agent_note_v1";

    /// A note Fact and its sidecar row, in one admission that DECLARES the
    /// table. `ingest_fact_atomic` declares none, so a note row hung off one
    /// is a row `forget` and the owner inverse can never reach — which
    /// `assert_memory_declares_sidecar` refuses.
    async fn ingest_note_fact(
        pg: &crate::PgStorage,
        owner: &Owner,
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
            &SpaceVector::new(claim.space.clone(), padded_embedding(prefix))
                .expect("1024-wide test vector"),
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
            let claims = claim_pending_embedding_jobs(pg.pool_for_tests(), 1, &[]).await?;
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
                &stub_vector(embedding.clone()),
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

    /// A provider that rejects an input will reject it again. Requeueing such a
    /// job spins claim → reject → requeue forever, which is why the two failure
    /// causes need separate statuses.
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

            let claims = claim_pending_embedding_jobs(pool, 1, &[]).await?;
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

            let claims = claim_pending_embedding_jobs(pool, 1, &[]).await?;
            assert_eq!(claims.len(), 1);
            assert_eq!(claims[0].entity_id, permanently_rejected.memory_id);
            fail_embedding_job_permanently(pool, &claims[0], "provider rejects forever").await?;

            let reconciled = reconcile_embeddings(
                pool,
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
        let observed = observe_owner_backfill_progress(&pg).await;
        eprintln!("owner backfill progress: {observed:?}");
        drop(pg);
        let cleanup = drop_db(&db_name).await;
        eprintln!("owner backfill database cleanup: {cleanup:?}");
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
        let foreign_owner = Owner::Personal(proxima_core::UserId::new(Uuid::now_v7()));
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
        let claims = claim_pending_embedding_jobs(pool, 1, &[]).await?;
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
        let Owner::Personal(user_id) = owner else {
            return Err("backfill fixture owner must be personal".into());
        };
        let authz = AuthzContext::for_subject(user_id, AuthPath::HostBearer);
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
            .bind(owner.stored_owner_id())
            .bind(later.into_inner())
            .fetch_one(pool)
            .await?,
            foreign_owner_jobs: count_jobs(pool, foreign.into_inner()).await?,
        })
    }

    async fn backfill_note(
        pg: &crate::PgStorage,
        owner: &Owner,
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

            let cold = MemoryColdStore::default();
            let object_key = cold_object_key(t);
            forget_memory(
                &mut forget_tx,
                &core_pg_sidecars(),
                &proxima_core::owner_inverse::OwnerSurfaces::for_registry(
                    &proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests(),
                ),
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
            let claims = claim_pending_embedding_jobs(pool, 1, &[]).await?;
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
                embedding_ann_observability(pool, None, stale_claim_seconds())
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
            let old_claims = claim_pending_embedding_jobs(pool, 1, &[]).await?;
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

            let successor_claims = claim_pending_embedding_jobs(pool, 1, &[]).await?;
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
    async fn source_claim_cannot_finalize_a_job_after_owner_transfer()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
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
            let claims = claim_pending_embedding_jobs(pool, 1, &[]).await?;
            let stale = claims[0].clone();
            let destination = Owner::Group(GroupId::new(Uuid::now_v7()));
            let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
            let surfaces = proxima_core::owner_inverse::OwnerSurfaces::for_registry(&registry);

            assert!(
                pg.transfer_to_owner(
                    &permit,
                    EntityId::Memory(written.memory_id),
                    destination,
                    &surfaces,
                    std::slice::from_ref(&*STUB_SPACE),
                )
                .await?,
                "the transfer moves the processing job with its Memory"
            );
            let (job_owner, job_token): (Uuid, Option<Uuid>) = sqlx::query_as(
                "SELECT owner_id, claim_token
                   FROM proxima_core.embedding_jobs
                  WHERE job_id = $1",
            )
            .bind(stale.job_id)
            .fetch_one(pool)
            .await?;
            assert_eq!(job_owner, destination.stored_owner_id());
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
            let successor = claim_pending_embedding_jobs(pool, 1, &[]).await?;
            assert_eq!(successor.len(), 1);
            assert_eq!(successor[0].owner, destination);
            insert_claimed_fact_embedding(&pg, &successor[0], [0.4, 0.5, 0.6]).await?;
            complete_embedding_job(pool, &successor[0]).await?;
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
            claim_pending_embedding_jobs(pool, 1, &[]).await?;
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
        offered: Arc<std::sync::Mutex<Vec<usize>>>,
    }

    #[async_trait::async_trait]
    impl EmbeddingClient for CappedEmbedding {
        async fn embed(&self, text: &str) -> Result<Vec<f32>, LlmError> {
            let chars = text.chars().count();
            self.offered
                .lock()
                .expect("test lock is not poisoned")
                .push(chars);
            if chars > self.max_chars {
                return Err(LlmError::EmbedPermanent(format!(
                    "input of {chars} chars exceeds the {}-char limit",
                    self.max_chars
                )));
            }
            Ok(padded_embedding([0.5, 0.6, 0.7]))
        }

        fn model_id(&self) -> &'static str {
            "stub-fact-embed"
        }

        fn dim(&self) -> usize {
            EMBEDDING_DIM
        }
    }

    fn derived_request(
        owner: Owner,
        origins: &[MemoryId],
        text: String,
    ) -> Result<DerivedMemory, ProtocolError> {
        DerivedMemory::abstraction(
            MemoryTarget::Series(SeriesHandle::new(Uuid::now_v7())),
            owner,
            text,
            AgentDerivationV1 {
                title: "long derivation".into(),
                body: "long derivation".into(),
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
        owner: &Owner,
        max_chars: usize,
    ) -> Result<
        (
            Engine,
            AuthzContext,
            MemoryId,
            Arc<std::sync::Mutex<Vec<usize>>>,
        ),
        Box<dyn std::error::Error>,
    > {
        let Owner::Personal(user_id) = owner else {
            return Err("fixture owner is personal".into());
        };
        let mut draft = fact_draft("origin");
        draft.schema_id = SchemaId::new("core/agent-note-v1".into());
        let origin = ingest_note_fact(pg, owner, &draft, &[], "origin", "origin").await?;
        let offered = Arc::new(std::sync::Mutex::new(Vec::new()));
        let registry = FlavorRegistry::new().freeze_or_panic_for_tests();
        let configured_pg = pg.clone().with_flavors(&registry);
        let engine = Engine::new(registry)
            .with_storage_ports(Arc::new(configured_pg).storage_ports())
            .with_embedding_router(routed(CappedEmbedding {
                max_chars,
                offered: offered.clone(),
            }));
        let authz = AuthzContext::for_subject(*user_id, AuthPath::HostBearer);
        Ok((engine, authz, origin.memory_id, offered))
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

    /// One long unit used to be refused at authoring, written without a
    /// vector, and rescued into chunks by a later drain — a warning, a
    /// second round trip, and a window of semantic invisibility per unit,
    /// for what is routine in a corpus of long texts. The rescue now runs
    /// inline: one vector lands with the row and no job is filed.
    #[tokio::test]
    async fn author_derived_embeds_over_limit_text_inline() -> Result<(), Box<dyn std::error::Error>>
    {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            let cap = MIN_EMBED_INPUT_CAP_CHARS;
            let (engine, authz, origin, offered) =
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
                !outcome.embedding_deferred,
                "an over-limit text is rescued inline, not deferred"
            );
            assert_eq!(
                count_jobs(pool, outcome.memory_id.into_inner()).await?,
                0,
                "no job is filed for a text the chunked rescue covered"
            );
            assert_eq!(
                count_fact_embeddings(pool, outcome.memory_id).await?,
                1,
                "storage keeps one vec per version"
            );
            assert_eq!(
                load_embedding_head_version(
                    pool,
                    EntityKind::Abstraction,
                    outcome.memory_id.into_inner(),
                    "stub-fact-embed"
                )
                .await?,
                Some(1),
                "the vector landed in the same write as the row"
            );
            let offered = offered.lock().expect("test lock is not poisoned").clone();
            assert!(
                offered.iter().any(|chars| *chars > cap),
                "the whole text is offered first: {offered:?}"
            );
            let accepted = offered.iter().filter(|chars| **chars <= cap).count();
            assert!(
                accepted > 1,
                "the text came back split into provider-acceptable pieces: {offered:?}"
            );
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }

    /// The job path is still the right place for a text the provider
    /// rejects at every length: terminal failures and retries live there.
    /// The provider answers the liveness probe, so this is the input's
    /// fault and the write goes through without a vector.
    #[tokio::test]
    async fn author_derived_defers_text_refused_at_every_length()
    -> Result<(), Box<dyn std::error::Error>> {
        let (pg, db_name) = fresh_pg("proxima_spg_embed").await;
        let result: Result<(), Box<dyn std::error::Error>> = async {
            let owner = owner_fixture();
            // Accepts the probe and nothing the bisection can produce.
            let (engine, authz, origin, _offered) =
                capped_authoring_fixture(&pg, &owner, EMBED_LIVENESS_PROBE.len()).await?;
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
            Ok(())
        }
        .await;
        drop(pg);
        drop_db(&db_name).await?;
        result
    }
}
