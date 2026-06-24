use crate::common::{drop_db, fresh_pg, owner_fixture};

use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
};
use proxima_core::{
    MemoryId, Owner, PersonalityInstanceId, Principal, SchemaId, SchemaVersion,
    SearchProjectionColumnKind, SourceBatchId, SourceId, Storage, UserId,
};
use uuid::Uuid;

#[tokio::test]
async fn semantic_search_ranks_nearest_vector_and_isolates_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let other_owner = Principal::User(UserId::new(Uuid::from_u128(7)));

    let near = insert_embedded_memory(&pg, &owner, "nearest", [0.99, 0.01, 0.0]).await?;
    let far = insert_embedded_memory(&pg, &owner, "orthogonal", [0.0, 1.0, 0.0]).await?;
    let other = insert_embedded_memory(&pg, &other_owner, "other owner", [1.0, 0.0, 0.0]).await?;

    let rows = pg
        .search_memories(
            &MemorySearchRequest {
                principal: owner.clone(),
                query: "semantic query".into(),
                mode: SearchMode::Semantic,
                supersession: SupersessionStatus::HeadsOnly,
                limit: 10,
                kind: Some(EntityKind::Abstraction),
                schema_id: Some(SchemaId::new("test/search-abstraction-v1".into())),
                tags: Vec::new(),
                tag_match: TagMatch::Any,
                since: None,
                until: None,
                order: SearchOrder::Relevance,
                query_embedding: Some(padded_embedding([1.0, 0.0, 0.0])),
                embedding_model_id: Some("test-embed".into()),
                reader_personality_instance_id: None,
            },
            &[],
        )
        .await?;

    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(near)
    );
    assert!(rows.iter().any(|row| row.memory_id.into_inner() == far));
    assert!(!rows.iter().any(|row| row.memory_id.into_inner() == other));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn semantic_search_matches_pgvector_cosine_and_clamps_zero_query()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let east_vec = padded_embedding([1.0, 0.0, 0.0]);
    let north_vec = padded_embedding([0.0, 1.0, 0.0]);
    let diagonal_vec = padded_embedding([0.5, 0.5, 0.0]);
    let query_vec = padded_embedding([1.0, 0.2, 0.0]);

    let east = insert_embedded_memory_with_vec(&pg, &owner, "east", &east_vec).await?;
    let north = insert_embedded_memory_with_vec(&pg, &owner, "north", &north_vec).await?;
    let diagonal = insert_embedded_memory_with_vec(&pg, &owner, "diagonal", &diagonal_vec).await?;

    let indexdef: Option<String> = sqlx::query_scalar(
        "SELECT indexdef
           FROM pg_indexes
          WHERE schemaname = 'proxima_core'
            AND tablename = 'embeddings'
            AND indexname = 'idx_embeddings_vec_hnsw'",
    )
    .fetch_optional(pg.pool())
    .await?;
    let indexdef = indexdef.expect("HNSW index exists");
    assert!(indexdef.contains("USING hnsw"), "{indexdef}");

    let rows = pg
        .search_memories(&semantic_request(&owner, query_vec.clone()), &[])
        .await?;

    let mut expected = [
        (east, brute_cosine(&east_vec, &query_vec)),
        (north, brute_cosine(&north_vec, &query_vec)),
        (diagonal, brute_cosine(&diagonal_vec, &query_vec)),
    ];
    expected.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| b.0.cmp(&a.0)));

    let actual: Vec<_> = rows
        .iter()
        .map(|row| (row.memory_id.into_inner(), row.similarity_score))
        .collect();
    assert_eq!(
        actual.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        expected.iter().map(|(id, _)| *id).collect::<Vec<_>>()
    );
    for ((_, actual_score), (_, expected_score)) in actual.iter().zip(expected.iter()) {
        assert!(
            (actual_score - expected_score).abs() <= 1.0e-4,
            "actual {actual_score} expected {expected_score}"
        );
    }

    let zero_rows = pg
        .search_memories(&semantic_request(&owner, vec![0.0; EMBEDDING_DIM]), &[])
        .await?;
    assert!(
        zero_rows
            .iter()
            .all(|row| row.similarity_score.abs() <= f32::EPSILON),
        "{zero_rows:#?}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn lexical_search_ignores_unprojected_code_chunk_text()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_code")
        .execute(pg.pool())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_code.code_chunk_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             file_path text NOT NULL,
             language text,
             chunk_type text NOT NULL,
             text text NOT NULL
         )",
    )
    .execute(pg.pool())
    .await?;

    let owner = owner_fixture();
    let chunk_id =
        ingest_fact_memory(&pg, &owner, "proxima-code/code-chunk-v1", b"code-chunk").await?;
    let long_token = format!("{}rawonlyneedle", "a".repeat(300));
    sqlx::query(
        "INSERT INTO proxima_code.code_chunk_v1
            (memory_id, file_path, language, chunk_type, text)
         VALUES ($1, 'src/search.rs', 'rust', 'function', $2)",
    )
    .bind(chunk_id.into_inner())
    .bind(long_token)
    .execute(pg.pool())
    .await?;

    let projections = vec![code_chunk_projection()];
    let rows = pg
        .search_memories(&lexical_request(&owner, "rawonlyneedle"), &projections)
        .await?;
    assert!(rows.is_empty(), "{rows:#?}");

    let rows = pg
        .search_memories(&lexical_request(&owner, "src search"), &projections)
        .await?;
    assert_eq!(rows.first().map(|row| row.memory_id), Some(chunk_id));
    assert!(!rows[0].snippet.contains("rawonlyneedle"));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_projects_authoring_personality_and_nil_as_none()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let author = PersonalityInstanceId::new(Uuid::now_v7());
    let authored = insert_text_memory(
        &pg,
        &owner,
        "authored attribution needle",
        Some(author.into_inner()),
    )
    .await?;
    let nil_authored = insert_text_memory(&pg, &owner, "nil attribution needle", None).await?;

    let authored_rows = pg
        .search_memories(
            &MemorySearchRequest {
                principal: owner.clone(),
                query: "authored attribution".into(),
                mode: SearchMode::Lexical,
                supersession: SupersessionStatus::HeadsOnly,
                limit: 10,
                kind: Some(EntityKind::Abstraction),
                schema_id: Some(SchemaId::new("test/search-attribution-v1".into())),
                tags: Vec::new(),
                tag_match: TagMatch::Any,
                since: None,
                until: None,
                order: SearchOrder::Relevance,
                query_embedding: None,
                embedding_model_id: None,
                reader_personality_instance_id: None,
            },
            &[],
        )
        .await?;
    let authored_row = authored_rows
        .iter()
        .find(|row| row.memory_id.into_inner() == authored)
        .expect("authored row");
    assert_eq!(authored_row.authoring_personality_instance_id, Some(author));

    let nil_rows = pg
        .search_memories(
            &MemorySearchRequest {
                principal: owner.clone(),
                query: "nil attribution".into(),
                mode: SearchMode::Lexical,
                supersession: SupersessionStatus::HeadsOnly,
                limit: 10,
                kind: Some(EntityKind::Abstraction),
                schema_id: Some(SchemaId::new("test/search-attribution-v1".into())),
                tags: Vec::new(),
                tag_match: TagMatch::Any,
                since: None,
                until: None,
                order: SearchOrder::Relevance,
                query_embedding: None,
                embedding_model_id: None,
                reader_personality_instance_id: None,
            },
            &[],
        )
        .await?;
    let nil_row = nil_rows
        .iter()
        .find(|row| row.memory_id.into_inner() == nil_authored)
        .expect("nil-author row");
    assert_eq!(nil_row.authoring_personality_instance_id, None);

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn lexical_search_ignores_sidecar_without_projection()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_test")
        .execute(pg.pool())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.unprojected_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             secret text NOT NULL
         )",
    )
    .execute(pg.pool())
    .await?;

    let owner = owner_fixture();
    let memory_id =
        ingest_fact_memory(&pg, &owner, "proxima-test/unprojected-v1", b"unprojected").await?;
    sqlx::query("INSERT INTO proxima_test.unprojected_v1 (memory_id, secret) VALUES ($1, $2)")
        .bind(memory_id.into_inner())
        .bind("hidden payload phrase")
        .execute(pg.pool())
        .await?;

    let rows = pg
        .search_memories(&lexical_request(&owner, "hidden payload"), &[])
        .await?;
    assert!(rows.is_empty(), "{rows:#?}");

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_filters_tags_across_modes_and_excludes_untagged()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    create_tagged_search_sidecars(pg.pool()).await?;

    let owner = owner_fixture();
    let now = time::OffsetDateTime::from_unix_timestamp(1_700_000_000)?;
    let target = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(11),
            title: "Tagged focus",
            body: "tagged filter needle target",
            tags: &["blue", "focus"],
            created_at: now,
            embedding: Some([1.0, 0.0, 0.0]),
        },
    )
    .await?;
    let blue_only = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(12),
            title: "Tagged blue",
            body: "tagged filter needle blue",
            tags: &["blue"],
            created_at: now + time::Duration::seconds(1),
            embedding: Some([0.0, 1.0, 0.0]),
        },
    )
    .await?;
    let empty_tags = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(13),
            title: "Tagged empty",
            body: "tagged filter needle empty",
            tags: &[],
            created_at: now + time::Duration::seconds(2),
            embedding: Some([1.0, 0.0, 0.0]),
        },
    )
    .await?;
    let unprojected = insert_text_memory(
        &pg,
        &owner,
        "tagged filter needle unprojected",
        Some(Uuid::nil()),
    )
    .await?;
    let projections = vec![tagged_abstraction_projection()];

    let mut any_req = tagged_search_request(&owner, "tagged filter", SearchMode::Lexical);
    any_req.schema_id = None;
    any_req.tags = vec!["blue".into(), "focus".into()];
    any_req.tag_match = TagMatch::Any;
    let rows = pg.search_memories(&any_req, &projections).await?;
    let ids: Vec<_> = rows.iter().map(|row| row.memory_id).collect();
    assert!(ids.contains(&target), "{rows:#?}");
    assert!(ids.contains(&blue_only), "{rows:#?}");
    assert!(!ids.contains(&empty_tags), "{rows:#?}");
    assert!(
        !ids.contains(&MemoryId::new(unprojected)),
        "untagged base memory matched tag filter: {rows:#?}"
    );

    let mut all_req = tagged_search_request(&owner, "tagged filter", SearchMode::Lexical);
    all_req.schema_id = None;
    all_req.tags = vec!["blue".into(), "focus".into()];
    all_req.tag_match = TagMatch::All;
    let rows = pg.search_memories(&all_req, &projections).await?;
    assert_eq!(
        rows.iter().map(|row| row.memory_id).collect::<Vec<_>>(),
        vec![target]
    );

    let mut semantic_req = tagged_search_request(&owner, "semantic query", SearchMode::Semantic);
    semantic_req.schema_id = None;
    semantic_req.tags = vec!["focus".into()];
    semantic_req.query_embedding = Some(padded_embedding([1.0, 0.0, 0.0]));
    semantic_req.embedding_model_id = Some("test-embed".into());
    let rows = pg.search_memories(&semantic_req, &projections).await?;
    assert_eq!(rows.first().map(|row| row.memory_id), Some(target));

    let mut hybrid_req = tagged_search_request(&owner, "semantic query", SearchMode::Hybrid);
    hybrid_req.schema_id = None;
    hybrid_req.tags = vec!["focus".into()];
    hybrid_req.query_embedding = Some(padded_embedding([1.0, 0.0, 0.0]));
    hybrid_req.embedding_model_id = Some("test-embed".into());
    let rows = pg.search_memories(&hybrid_req, &projections).await?;
    assert_eq!(rows.first().map(|row| row.memory_id), Some(target));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_filters_created_at_range_and_populates_created_at()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    create_tagged_search_sidecars(pg.pool()).await?;

    let owner = owner_fixture();
    let base = time::OffsetDateTime::from_unix_timestamp(1_700_010_000)?;
    let old = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(21),
            title: "Time old",
            body: "time range needle",
            tags: &["time"],
            created_at: base - time::Duration::days(2),
            embedding: None,
        },
    )
    .await?;
    let middle = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(22),
            title: "Time middle",
            body: "time range needle",
            tags: &["time"],
            created_at: base,
            embedding: None,
        },
    )
    .await?;
    let new = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(23),
            title: "Time new",
            body: "time range needle",
            tags: &["time"],
            created_at: base + time::Duration::days(2),
            embedding: None,
        },
    )
    .await?;

    let mut req = tagged_search_request(&owner, "time range", SearchMode::Lexical);
    req.since = Some(base - time::Duration::hours(1));
    req.until = Some(base + time::Duration::hours(1));
    let rows = pg
        .search_memories(&req, &[tagged_abstraction_projection()])
        .await?;

    assert_eq!(
        rows.iter().map(|row| row.memory_id).collect::<Vec<_>>(),
        vec![middle]
    );
    assert_eq!(rows[0].created_at.unix_timestamp(), base.unix_timestamp());
    assert!(!rows.iter().any(|row| row.memory_id == old));
    assert!(!rows.iter().any(|row| row.memory_id == new));

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_order_recency_sorts_matching_candidates_newest_first()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    create_tagged_search_sidecars(pg.pool()).await?;

    let owner = owner_fixture();
    let base = time::OffsetDateTime::from_unix_timestamp(1_700_020_000)?;
    let newer = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(31),
            title: "Recency same",
            body: "recency ordering needle",
            tags: &["recency"],
            created_at: base + time::Duration::days(1),
            embedding: None,
        },
    )
    .await?;
    let older = insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(32),
            title: "Recency same",
            body: "recency ordering needle",
            tags: &["recency"],
            created_at: base - time::Duration::days(1),
            embedding: None,
        },
    )
    .await?;
    let projection = tagged_abstraction_projection();

    let relevance_req = tagged_search_request(&owner, "recency ordering", SearchMode::Lexical);
    let rows = pg
        .search_memories(&relevance_req, std::slice::from_ref(&projection))
        .await?;
    assert_eq!(
        rows.iter().map(|row| row.memory_id).collect::<Vec<_>>(),
        vec![older, newer],
        "relevance keeps score then memory_id ordering"
    );

    let mut recency_req = tagged_search_request(&owner, "recency ordering", SearchMode::Lexical);
    recency_req.order = SearchOrder::Recency;
    let rows = pg.search_memories(&recency_req, &[projection]).await?;
    assert_eq!(
        rows.iter().map(|row| row.memory_id).collect::<Vec<_>>(),
        vec![newer, older]
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[derive(Debug)]
struct TaggedAbstractionInsert<'a> {
    memory_id: Uuid,
    title: &'a str,
    body: &'a str,
    tags: &'a [&'a str],
    created_at: time::OffsetDateTime,
    embedding: Option<[f32; 3]>,
}

async fn create_tagged_search_sidecars(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_test")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.tagged_abstraction_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             title text NOT NULL,
             body text NOT NULL,
             tags text[] NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_tagged_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    input: TaggedAbstractionInsert<'_>,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             schema_id, schema_version, created_at, kind, text, operator_kind,
             model_id, prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, 'proxima-test/tagged-abstraction-v1', 1,
                 $4, 'Abstraction', $5, 'Wake', 'test-model', 'test-v1',
                 '00000000-0000-0000-0000-000000000000'::uuid, 2)",
    )
    .bind(input.memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(input.created_at)
    .bind(input.body)
    .execute(pg.pool())
    .await?;
    let tags: Vec<String> = input.tags.iter().map(|tag| (*tag).to_string()).collect();
    sqlx::query(
        "INSERT INTO proxima_test.tagged_abstraction_v1 (memory_id, title, body, tags)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(input.memory_id)
    .bind(input.title)
    .bind(input.body)
    .bind(tags)
    .execute(pg.pool())
    .await?;
    if let Some(embedding) = input.embedding {
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_kind, entity_id, embedding_version, model_id, vec,
                 owner_principal_kind, owner_principal_id)
             VALUES ('Abstraction', $1, 1, 'test-embed', $2::vector, $3, $4)",
        )
        .bind(input.memory_id)
        .bind(vector_literal(&padded_embedding(embedding)))
        .bind(owner_kind)
        .bind(owner_principal_id)
        .execute(pg.pool())
        .await?;
    }
    Ok(MemoryId::new(input.memory_id))
}

async fn insert_embedded_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    embedding: [f32; 3],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let embedding = padded_embedding(embedding);
    insert_embedded_memory_with_vec(pg, owner, text, &embedding).await
}

async fn insert_embedded_memory_with_vec(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    embedding: &[f32],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, 'test/search-abstraction-v1', 1,
                 'Abstraction', $4, 'Wake', 'test-model', 'test-v1',
                 '00000000-0000-0000-0000-000000000000'::uuid, 2)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(text)
    .execute(pg.pool())
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_principal_kind, owner_principal_id)
         VALUES ('Abstraction', $1, 1, 'test-embed', $2::vector, $3, $4)",
    )
    .bind(memory_id)
    .bind(vector_literal(embedding))
    .bind(owner_kind)
    .bind(owner_principal_id)
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

async fn insert_text_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    personality_instance_id: Option<Uuid>,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_principal_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_principal_kind, owner_principal_id,
             schema_id, schema_version, kind, text, operator_kind, model_id,
             prompt_version, personality_instance_id, wake_chain_depth)
         VALUES ($1, $2, $3, 'test/search-attribution-v1', 1,
                 'Abstraction', $4, 'Wake', 'test-model', 'test-v1',
                 COALESCE($5, '00000000-0000-0000-0000-000000000000'::uuid), 2)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(text)
    .bind(personality_instance_id)
    .execute(pg.pool())
    .await?;
    Ok(memory_id)
}

async fn ingest_fact_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    schema_id: &str,
    payload: &[u8],
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    use proxima_core::verbs::event_ingest::{
        Citation, CitationMappingHint, CitedObjectHint, EventDraft,
    };

    let now = time::OffsetDateTime::now_utc();
    let outcome = pg
        .ingest_event_atomic(
            &EventDraft {
                source_id: SourceId::new("test/search"),
                source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                principal: owner.clone(),
                author_personality_instance_id: None,
                schema_id: SchemaId::new(schema_id.to_string()),
                schema_version: SchemaVersion::new(1),
                payload: payload.to_vec(),
                rendered_text: None,
                observed_at: now,
                occurred_at: now,
                citation: Some(Citation {
                    object: CitedObjectHint {
                        schema_id: SchemaId::new("test/search-object-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        content_hash: *blake3::hash(payload).as_bytes(),
                    },
                    mapping: CitationMappingHint {
                        schema_id: SchemaId::new("test/search-whole-v1".into()),
                        schema_version: SchemaVersion::new(1),
                    },
                }),
            },
            None,
        )
        .await?;
    Ok(outcome.memory_id)
}

fn lexical_request(owner: &Owner, query: &str) -> MemorySearchRequest {
    MemorySearchRequest {
        principal: owner.clone(),
        query: query.into(),
        mode: SearchMode::Lexical,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 10,
        kind: Some(EntityKind::Fact),
        schema_id: None,
        tags: Vec::new(),
        tag_match: TagMatch::Any,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        query_embedding: None,
        embedding_model_id: None,
        reader_personality_instance_id: None,
    }
}

fn semantic_request(owner: &Owner, query_embedding: Vec<f32>) -> MemorySearchRequest {
    MemorySearchRequest {
        principal: owner.clone(),
        query: "semantic query".into(),
        mode: SearchMode::Semantic,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 10,
        kind: Some(EntityKind::Abstraction),
        schema_id: Some(SchemaId::new("test/search-abstraction-v1".into())),
        tags: Vec::new(),
        tag_match: TagMatch::Any,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        query_embedding: Some(query_embedding),
        embedding_model_id: Some("test-embed".into()),
        reader_personality_instance_id: None,
    }
}

fn tagged_search_request(owner: &Owner, query: &str, mode: SearchMode) -> MemorySearchRequest {
    MemorySearchRequest {
        principal: owner.clone(),
        query: query.into(),
        mode,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 10,
        kind: Some(EntityKind::Abstraction),
        schema_id: Some(SchemaId::new("proxima-test/tagged-abstraction-v1".into())),
        tags: Vec::new(),
        tag_match: TagMatch::Any,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        query_embedding: None,
        embedding_model_id: None,
        reader_personality_instance_id: None,
    }
}

fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
    let mut embedding = vec![0.0; EMBEDDING_DIM];
    embedding[..prefix.len()].copy_from_slice(&prefix);
    embedding
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

fn brute_cosine(stored: &[f32], query: &[f32]) -> f32 {
    let dot: f32 = stored
        .iter()
        .zip(query.iter())
        .map(|(stored, query)| stored * query)
        .sum();
    let stored_norm = stored.iter().map(|value| value * value).sum::<f32>().sqrt();
    let query_norm = query.iter().map(|value| value * value).sum::<f32>().sqrt();
    if stored_norm <= f32::EPSILON || query_norm <= f32::EPSILON {
        0.0
    } else {
        (dot / (stored_norm * query_norm)).max(0.0)
    }
}

fn code_chunk_projection() -> MemorySearchProjection {
    MemorySearchProjection {
        schema_id: SchemaId::new("proxima-code/code-chunk-v1".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Fact,
        sidecar_table: "proxima_code.code_chunk_v1".into(),
        fields: vec![
            MemorySearchProjectionField {
                column: "file_path".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "language".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "chunk_type".into(),
                kind: SearchProjectionColumnKind::Text,
            },
        ],
        tag_column: None,
    }
}

fn tagged_abstraction_projection() -> MemorySearchProjection {
    MemorySearchProjection {
        schema_id: SchemaId::new("proxima-test/tagged-abstraction-v1".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Abstraction,
        sidecar_table: "proxima_test.tagged_abstraction_v1".into(),
        fields: vec![
            MemorySearchProjectionField {
                column: "title".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "body".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "tags".into(),
                kind: SearchProjectionColumnKind::TextArray,
            },
        ],
        tag_column: Some("tags".into()),
    }
}
