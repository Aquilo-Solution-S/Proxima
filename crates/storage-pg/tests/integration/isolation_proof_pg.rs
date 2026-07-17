use crate::common::{drop_db, fresh_pg, owner_fixture, owner_write_permit};
use proxima_core::storage_ports::*;
use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::query::{
    EdgeFilter, EdgeReadRequest, MemoryLineageDirection, MemoryLineageRequest, MemorySearchRequest,
    QueryRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::{
    AccessKind, Cursor, FactPayload, MemoryId, Owner, OwnerRef, PayloadKeyBuilder, RelationClass,
    SchemaId, SchemaVersion, SourceBatchId, SourceId, StorageError, UserId,
};
use proxima_storage_pg::verbs::fact_ingest::ingest_fact;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct IsolationPayload {
    label: String,
}

impl FactPayload for IsolationPayload {
    const SCHEMA_ID: &'static str = "test/isolation-proof-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("label", &self.label);
        key.finish()
    }

    fn render(&self) -> String {
        self.label.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("public.isolation_fact_sidecar")
    }
}

#[tokio::test]
async fn isolation_proof_covers_reads_replay_and_receipt_scope()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    let result = async {
        sqlx::query(
            "CREATE TABLE public.isolation_fact_sidecar (
                memory_id uuid PRIMARY KEY,
                label text NOT NULL
            )",
        )
        .execute(pg.pool_for_tests())
        .await?;

        let owner_a = owner_fixture();
        let owner_b = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let permit_a = owner_write_permit(&owner_a, AccessKind::Fact).await?;
        let permit_b = owner_write_permit(&owner_b, AccessKind::Fact).await?;
        let payload = IsolationPayload {
            label: format!("isolation-alpha-{}", Uuid::now_v7()),
        };

        let first = ingest_fact(
            pg.pool_for_tests(),
            &permit_a,
            &payload,
            None,
            sidecar_insert(payload.label.clone()),
        )
        .await?;
        let cited_object_id = ingest_cited_fact(&pg, &permit_a, &owner_a).await?;
        let source = "isolation/projection";
        pg.store_source_cursor(
            &permit_a,
            source,
            &Cursor::from_bytes(vec![0x01, 0x02, 0x03]),
        )
        .await?;

        let source_memory = insert_memory(&pg, &owner_a, "isolation source").await?;
        let target_memory = insert_memory(&pg, &owner_a, "isolation target").await?;
        let edge_id = insert_edge(
            &pg,
            &owner_a,
            source_memory,
            target_memory,
            "test/isolation-edge",
            RelationClass::Provenance,
        )
        .await?;

        assert_owner_b_cannot_see_owner_a(
            &pg,
            owner_b,
            &payload.label,
            cited_object_id,
            source,
            source_memory,
            edge_id,
        )
        .await?;

        let replay = ingest_fact(
            pg.pool_for_tests(),
            &permit_a,
            &payload,
            None,
            |_tx, _outcome| {
                Box::pin(async {
                    Err(StorageError::Internal(
                        "sidecar closure must not run for idempotent replay".into(),
                    ))
                })
            },
        )
        .await?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.memory_id, first.memory_id);
        assert_eq!(replay.change_event_seq, first.change_event_seq);
        assert_eq!(memory_count_for_schema(&pg, &owner_a).await?, 1);
        assert_eq!(sidecar_count(&pg, first.memory_id.into_inner()).await?, 1);

        let cross_owner = ingest_fact(
            pg.pool_for_tests(),
            &permit_b,
            &payload,
            None,
            sidecar_insert(payload.label.clone()),
        )
        .await?;
        assert!(!cross_owner.idempotent_replay);
        assert_ne!(cross_owner.memory_id, first.memory_id);
        assert_eq!(memory_count_for_schema(&pg, &owner_b).await?, 1);

        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result
}

async fn assert_owner_b_cannot_see_owner_a(
    pg: &proxima_storage_pg::PgStorage,
    owner_b: Owner,
    query_text: &str,
    cited_object_id: Uuid,
    cursor_source: &str,
    lineage_start: Uuid,
    edge_id: Uuid,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut query = QueryRequest::for_owner(owner_b);
    query.include_payloads = false;
    query.schema_id = Some(IsolationPayload::schema_id());
    assert!(
        pg.query_memories(&query, &[]).await?.memories.is_empty(),
        "owner B query must not see owner A memories"
    );

    let search = MemorySearchRequest {
        owner: owner_b,
        read_owners: vec![owner_b],
        query: query_text.to_string(),
        mode: SearchMode::Lexical,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 10,
        kind: None,
        schema_id: Some(IsolationPayload::schema_id()),
        tags: Vec::new(),
        tag_match: TagMatch::Any,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        min_score: None,
        semantic_weight: None,
        after: None,
        query_embedding: None,
        embedding_model_id: None,
    };
    assert!(
        pg.search_memories(&search, &[]).await?.results.is_empty(),
        "owner B search must not see owner A text"
    );

    let lineage = pg
        .walk_memory_lineage(
            &[owner_b],
            &MemoryLineageRequest {
                owner: owner_b,
                start_memory_id: MemoryId::new(lineage_start),
                direction: MemoryLineageDirection::Ancestors,
                depth: 2,
                limit: 10,
                after: None,
            },
        )
        .await?;
    assert!(
        lineage.nodes.is_empty() && lineage.edges.is_empty(),
        "owner B lineage must not reveal owner A start node"
    );

    assert!(
        pg.facts_citing_object(&[owner_b], cited_object_id, &[], None, 50)
            .await?
            .facts
            .is_empty(),
        "owner B citation lookup must not reveal owner A cited facts"
    );

    let edges = pg
        .read_edges(
            &[owner_b],
            &EdgeReadRequest {
                owner: owner_b,
                edge_ids: vec![proxima_core::EdgeId::new(edge_id)],
                filter: EdgeFilter::default(),
                limit: 10,
                cursor: None,
                include_payloads: false,
            },
            &[],
        )
        .await?;
    assert!(edges.edges.is_empty(), "owner B edge read must be empty");

    assert_eq!(
        pg.load_source_cursor(&owner_b, cursor_source).await?,
        None,
        "owner B cursor read must not see owner A cursor"
    );
    Ok(())
}

fn sidecar_insert(
    label: String,
) -> impl for<'t> FnOnce(
    &'t mut sqlx::Transaction<'_, sqlx::Postgres>,
    &'t proxima_core::verbs::fact_ingest::FactIngestOutcome,
) -> proxima_storage_pg::verbs::fact_ingest::FactIngestSidecarFuture<'t> {
    move |tx, outcome| {
        Box::pin(async move {
            sqlx::query(
                "INSERT INTO public.isolation_fact_sidecar (memory_id, label)
                 VALUES ($1, $2)
                 ON CONFLICT (memory_id) DO NOTHING",
            )
            .bind(outcome.memory_id.into_inner())
            .bind(label)
            .execute(&mut **tx)
            .await
            .map_err(|err| StorageError::Internal(err.to_string()))?;
            Ok(())
        })
    }
}

async fn ingest_cited_fact(
    pg: &proxima_storage_pg::PgStorage,
    permit: &proxima_core::storage_ports::OwnerWritePermit,
    owner: &Owner,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let now = time::OffsetDateTime::now_utc();
    let draft = FactWriteCommand {
        schema_id: SchemaId::new("test/isolation-cited-v1".into()),
        schema_version: SchemaVersion::new(1),
        payload: b"isolation cited payload".to_vec(),
        rendered_text: Some("isolation cited payload".to_string()),
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/isolation-citation"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("test/isolation-object-v1".into()),
                schema_version: SchemaVersion::new(1),
                content_hash: *blake3::hash(b"isolation object").as_bytes(),
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("test/isolation-mapping-v1".into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    };
    let outcome = pg.ingest_fact_atomic(permit, &draft, None).await?;
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    let cited_object_id = sqlx::query_scalar(
        "SELECT cm.cited_object_id
           FROM proxima_core.citation_mappings cm
          WHERE cm.memory_id = $1
            AND cm.owner_kind = $2
            AND cm.owner_id IS NOT DISTINCT FROM $3",
    )
    .bind(outcome.memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_id)
    .fetch_one(pg.pool_for_tests())
    .await?;
    Ok(cited_object_id)
}

async fn insert_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/isolation-lineage-v1', 1, 'Abstraction',
                 $4, 'AtoA', '00000000-0000-0000-0000-000000000511'::uuid,
                 '00000000-0000-0000-0000-000000000512'::uuid, NULL,
                 'test-model', 'test-v1')",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

async fn insert_edge(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    source: Uuid,
    target: Uuid,
    relation: &str,
    relation_class: RelationClass,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let edge_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.edges
            (edge_id, owner_kind, owner_id, relation, relation_class,
             source_kind, source_memory_id, source_goal_id,
             target_kind, target_memory_id, target_goal_id,
             authorship_kind, authorship_owner_memory_id)
         VALUES ($1, $2, $3, $4, $5,
                 'Abstraction', $6, NULL,
                 'Abstraction', $7, NULL,
                 'Engine', NULL)",
    )
    .bind(edge_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(relation)
    .bind(relation_class)
    .bind(source)
    .bind(target)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(edge_id)
}

async fn memory_count_for_schema(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
) -> Result<i64, sqlx::Error> {
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM proxima_core.memories
          WHERE owner_kind = $1
            AND owner_id IS NOT DISTINCT FROM $2
            AND schema_id = $3",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(IsolationPayload::SCHEMA_ID)
    .fetch_one(pg.pool_for_tests())
    .await
}

async fn sidecar_count(
    pg: &proxima_storage_pg::PgStorage,
    memory_id: Uuid,
) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*)::bigint
           FROM public.isolation_fact_sidecar
          WHERE memory_id = $1",
    )
    .bind(memory_id)
    .fetch_one(pg.pool_for_tests())
    .await
}
