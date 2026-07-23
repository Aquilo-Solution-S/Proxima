use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::storage_ports::*;

use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, SearchCursor, SearchMode, SearchOrder, SupersessionStatus,
    TagMatch,
};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
};
use proxima_core::{
    FactReceiptDraft, MemoryId, Owner, OwnerRef, SchemaId, SchemaVersion,
    SearchProjectionColumnKind, SourceBatchId, SourceId, UserId,
};
use uuid::Uuid;

#[tokio::test]
async fn semantic_search_ranks_nearest_vector_and_isolates_owner()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let other_owner = OwnerRef::Personal(UserId::new(Uuid::from_u128(7)));

    let near = insert_embedded_memory(&pg, &owner, "nearest", [0.99, 0.01, 0.0]).await?;
    let far = insert_embedded_memory(&pg, &owner, "orthogonal", [0.0, 1.0, 0.0]).await?;
    let other = insert_embedded_memory(&pg, &other_owner, "other owner", [1.0, 0.0, 0.0]).await?;

    let rows = pg
        .search_memories(
            &MemorySearchRequest {
                owner,
                read_owners: vec![owner],
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
                min_score: None,
                semantic_weight: None,
                after: None,
                query_embedding: Some(padded_embedding([1.0, 0.0, 0.0])),
                embedding_model_id: Some("test-embed".into()),
            },
            &[],
        )
        .await?
        .results;

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
async fn search_heads_only_ignores_cross_owner_supersedes_successor()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let victim = owner_fixture();
    let attacker = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let foreign_shadowed = insert_search_abstraction(
        &pg,
        &victim,
        "headscope victim with foreign successor",
        None,
    )
    .await?;
    let foreign_successor = insert_search_abstraction(
        &pg,
        &attacker,
        "headscope attacker corrupt successor",
        Some(foreign_shadowed),
    )
    .await?;
    let same_owner_shadowed =
        insert_search_abstraction(&pg, &victim, "headscope victim superseded", None).await?;
    let same_owner_successor = insert_search_abstraction(
        &pg,
        &victim,
        "headscope victim same-owner successor",
        Some(same_owner_shadowed),
    )
    .await?;

    let rows = pg
        .search_memories(
            &MemorySearchRequest {
                owner: victim,
                read_owners: vec![victim],
                query: "headscope".into(),
                mode: SearchMode::Lexical,
                supersession: SupersessionStatus::HeadsOnly,
                limit: 10,
                kind: Some(EntityKind::Abstraction),
                schema_id: Some(SchemaId::new("test/search-abstraction-v1".into())),
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
            },
            &[],
        )
        .await?
        .results;
    let ids = rows
        .iter()
        .map(|row| row.memory_id.into_inner())
        .collect::<Vec<_>>();

    assert!(
        ids.contains(&foreign_shadowed),
        "foreign successor must not suppress victim search head: {ids:#?}"
    );
    assert!(
        !ids.contains(&foreign_successor),
        "attacker successor must remain unreadable in search: {ids:#?}"
    );
    assert!(
        !ids.contains(&same_owner_shadowed),
        "same-owner successor must suppress prior search head: {ids:#?}"
    );
    assert!(
        ids.contains(&same_owner_successor),
        "same-owner successor remains searchable head: {ids:#?}"
    );

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
    .fetch_optional(pg.pool_for_tests())
    .await?;
    let indexdef = indexdef.expect("HNSW index exists");
    assert!(indexdef.contains("USING hnsw"), "{indexdef}");

    let rows = pg
        .search_memories(&semantic_request(&owner, query_vec.clone()), &[])
        .await?
        .results;

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
        .await?
        .results;
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
async fn semantic_search_uses_current_embedding_head() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let stale_far = padded_embedding([0.0, 1.0, 0.0]);
    let current_near = padded_embedding([1.0, 0.0, 0.0]);
    let memory_id =
        insert_embedded_memory_with_vec(&pg, &owner, "current head", &stale_far).await?;
    let (owner_kind, owner_id) = owner.columns();
    insert_embedding_with_head(
        pg.pool_for_tests(),
        EntityKind::Abstraction,
        memory_id,
        "test-embed",
        2,
        &current_near,
        owner_kind,
        owner_id,
    )
    .await?;

    let rows = pg
        .search_memories(&semantic_request(&owner, current_near), &[])
        .await?
        .results;

    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(memory_id)
    );
    assert!(
        rows.first().is_some_and(|row| row.similarity_score > 0.99),
        "{rows:#?}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn semantic_search_filters_current_and_authorized_before_candidate_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let other_owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let query = padded_embedding([1.0, 0.0, 0.0]);
    let stale_current_far = padded_embedding([0.0, 1.0, 0.0]);
    let target_vec = padded_embedding([0.8, 0.2, 0.0]);
    let target =
        insert_embedded_memory_with_vec(&pg, &owner, "authorized current target", &target_vec)
            .await?;
    let (owner_kind, owner_id) = owner.columns();

    for idx in 0..300 {
        let inaccessible = insert_embedded_memory_with_vec(
            &pg,
            &other_owner,
            &format!("inaccessible near {idx}"),
            &query,
        )
        .await?;
        assert_ne!(inaccessible, target);

        let stale =
            insert_embedded_memory_with_vec(&pg, &owner, &format!("stale near {idx}"), &query)
                .await?;
        insert_embedding_with_head(
            pg.pool_for_tests(),
            EntityKind::Abstraction,
            stale,
            "test-embed",
            2,
            &stale_current_far,
            owner_kind,
            owner_id,
        )
        .await?;
    }

    let mut req = semantic_request(&owner, query);
    req.limit = 1;
    let rows = pg.search_memories(&req, &[]).await?.results;

    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(target),
        "{rows:#?}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn semantic_search_filters_query_predicates_before_candidate_limit()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let query = padded_embedding([1.0, 0.0, 0.0]);
    let target_vec = padded_embedding([0.8, 0.2, 0.0]);
    let target =
        insert_embedded_memory_with_vec(&pg, &owner, "matching schema target", &target_vec).await?;
    for idx in 0..540 {
        let bad = insert_embedded_memory_with_schema(
            &pg,
            &owner,
            "test/search-other-abstraction-v1",
            &format!("schema filtered near {idx}"),
            &query,
        )
        .await?;
        assert_ne!(bad, target);
    }

    let mut req = semantic_request(&owner, query);
    req.limit = 1;
    let rows = pg.search_memories(&req, &[]).await?.results;

    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(target),
        "{rows:#?}"
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
        .execute(pg.pool_for_tests())
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
    .execute(pg.pool_for_tests())
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
    .execute(pg.pool_for_tests())
    .await?;

    let projections = vec![code_chunk_projection()];
    let rows = pg
        .search_memories(&lexical_request(&owner, "rawonlyneedle"), &projections)
        .await?
        .results;
    assert!(rows.is_empty(), "{rows:#?}");

    let rows = pg
        .search_memories(&lexical_request(&owner, "src search"), &projections)
        .await?
        .results;
    assert_eq!(rows.first().map(|row| row.memory_id), Some(chunk_id));
    assert!(!rows[0].snippet.contains("rawonlyneedle"));

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
        .execute(pg.pool_for_tests())
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.unprojected_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             secret text NOT NULL
         )",
    )
    .execute(pg.pool_for_tests())
    .await?;

    let owner = owner_fixture();
    let memory_id =
        ingest_fact_memory(&pg, &owner, "proxima-test/unprojected-v1", b"unprojected").await?;
    sqlx::query("INSERT INTO proxima_test.unprojected_v1 (memory_id, secret) VALUES ($1, $2)")
        .bind(memory_id.into_inner())
        .bind("hidden payload phrase")
        .execute(pg.pool_for_tests())
        .await?;

    let rows = pg
        .search_memories(&lexical_request(&owner, "hidden payload"), &[])
        .await?
        .results;
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
    create_tagged_search_sidecars(pg.pool_for_tests()).await?;

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
    let unprojected = insert_text_memory(&pg, &owner, "tagged filter needle unprojected").await?;
    let projections = vec![tagged_abstraction_projection()];

    let mut any_req = tagged_search_request(&owner, "tagged filter", SearchMode::Lexical);
    any_req.schema_id = None;
    any_req.tags = vec!["blue".into(), "focus".into()];
    any_req.tag_match = TagMatch::Any;
    let rows = pg.search_memories(&any_req, &projections).await?.results;
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
    let rows = pg.search_memories(&all_req, &projections).await?.results;
    assert_eq!(
        rows.iter().map(|row| row.memory_id).collect::<Vec<_>>(),
        vec![target]
    );

    let mut semantic_req = tagged_search_request(&owner, "semantic query", SearchMode::Semantic);
    semantic_req.schema_id = None;
    semantic_req.tags = vec!["focus".into()];
    semantic_req.query_embedding = Some(padded_embedding([1.0, 0.0, 0.0]));
    semantic_req.embedding_model_id = Some("test-embed".into());
    let rows = pg
        .search_memories(&semantic_req, &projections)
        .await?
        .results;
    assert_eq!(rows.first().map(|row| row.memory_id), Some(target));

    let mut hybrid_req = tagged_search_request(&owner, "semantic query", SearchMode::Hybrid);
    hybrid_req.schema_id = None;
    hybrid_req.tags = vec!["focus".into()];
    hybrid_req.query_embedding = Some(padded_embedding([1.0, 0.0, 0.0]));
    hybrid_req.embedding_model_id = Some("test-embed".into());
    let rows = pg.search_memories(&hybrid_req, &projections).await?.results;
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
    create_tagged_search_sidecars(pg.pool_for_tests()).await?;

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
        .await?
        .results;

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
    create_tagged_search_sidecars(pg.pool_for_tests()).await?;

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
        .await?
        .results;
    assert_eq!(
        rows.iter().map(|row| row.memory_id).collect::<Vec<_>>(),
        vec![older, newer],
        "relevance keeps score then memory_id ordering"
    );

    let mut recency_req = tagged_search_request(&owner, "recency ordering", SearchMode::Lexical);
    recency_req.order = SearchOrder::Recency;
    let rows = pg
        .search_memories(&recency_req, &[projection])
        .await?
        .results;
    assert_eq!(
        rows.iter().map(|row| row.memory_id).collect::<Vec<_>>(),
        vec![newer, older]
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn hybrid_fusion_weight_defaults_and_overrides_rerank()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    // Strong lexical match, orthogonal embedding — and vice versa.
    let lexical_hit =
        insert_embedded_memory(&pg, &owner, "fusiongrain exact needle", [0.0, 1.0, 0.0]).await?;
    let semantic_hit =
        insert_embedded_memory(&pg, &owner, "entirely unrelated prose", [1.0, 0.0, 0.0]).await?;

    let base = hybrid_request(&owner, "fusiongrain", padded_embedding([1.0, 0.0, 0.0]));
    let rows = pg.search_memories(&base, &[]).await?.results;
    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(semantic_hit),
        "default 0.6 semantic weight must outrank the lexical-only hit"
    );
    assert!(
        rows.iter()
            .any(|row| row.memory_id.into_inner() == lexical_hit)
    );
    for row in &rows {
        let expected = (0.6 * row.similarity_score) + (0.4 * row.lexical_score);
        assert!(
            (row.score - expected).abs() <= 1.0e-6,
            "default fused score must be 0.6*semantic + 0.4*lexical; got {} want {expected}",
            row.score
        );
    }

    let mut lexical_only = base.clone();
    lexical_only.semantic_weight = Some(0.0);
    let rows = pg.search_memories(&lexical_only, &[]).await?.results;
    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(lexical_hit),
        "semantic_weight=0.0 must rank purely lexically"
    );

    let mut semantic_only = base;
    semantic_only.semantic_weight = Some(1.0);
    let rows = pg.search_memories(&semantic_only, &[]).await?.results;
    assert_eq!(
        rows.first().map(|row| row.memory_id.into_inner()),
        Some(semantic_hit),
        "semantic_weight=1.0 must rank purely semantically"
    );
    for row in &rows {
        assert!(
            (row.score - row.similarity_score).abs() <= 1.0e-6,
            "weight 1.0 score must equal the similarity component"
        );
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn min_score_floor_drops_weak_matches() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    let strong = insert_search_abstraction(&pg, &owner, "floorgrain strong hit", None).await?;
    // Substring-only match: no tsquery token, so the LIKE arm scores it 0.25.
    let weak = insert_search_abstraction(&pg, &owner, "prefix xfloorgrainx suffix", None).await?;

    let mut unfloored = lexical_request(&owner, "floorgrain");
    unfloored.kind = Some(EntityKind::Abstraction);
    let page = pg.search_memories(&unfloored, &[]).await?;
    assert!(!page.has_more);
    let ids: Vec<_> = page
        .results
        .iter()
        .map(|row| row.memory_id.into_inner())
        .collect();
    assert!(ids.contains(&strong) && ids.contains(&weak));
    let strong_score = page
        .results
        .iter()
        .find(|row| row.memory_id.into_inner() == strong)
        .expect("strong row")
        .score;
    let weak_score = page
        .results
        .iter()
        .find(|row| row.memory_id.into_inner() == weak)
        .expect("weak row")
        .score;
    assert!(
        strong_score > 0.5,
        "tsquery match scores high: {strong_score}"
    );
    assert!(
        (weak_score - 0.25).abs() <= 1.0e-6,
        "LIKE-only match scores 0.25: {weak_score}"
    );

    let mut floored = unfloored;
    floored.min_score = Some(0.5);
    let page = pg.search_memories(&floored, &[]).await?;
    assert!(
        !page.has_more,
        "sub-floor rows must not count toward has_more"
    );
    assert_eq!(
        page.results
            .iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        vec![strong],
        "the floor must drop the 0.25 substring match"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn lexical_search_stems_and_ignores_stopwords() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    let adopted =
        insert_search_abstraction(&pg, &owner, "we adopted two kittens yesterday", None).await?;

    // Stemming ('english' config): the query's inflection must not matter —
    // "adopting a kitten" reaches "adopted two kittens".
    let mut inflected = lexical_request(&owner, "adopting a kitten");
    inflected.kind = Some(EntityKind::Abstraction);
    let page = pg.search_memories(&inflected, &[]).await?;
    assert!(
        page.results
            .iter()
            .any(|row| row.memory_id.into_inner() == adopted),
        "stemmed query must match the inflected document"
    );

    // Stopword removal: a natural-language question matches on its content
    // words; its function words ("when", "were", "the") must not be
    // required to appear literally under websearch AND semantics. Under
    // the previous 'simple' config this exact shape returned nothing.
    let mut question = lexical_request(&owner, "when were the kittens adopted?");
    question.kind = Some(EntityKind::Abstraction);
    let page = pg.search_memories(&question, &[]).await?;
    assert!(
        page.results
            .iter()
            .any(|row| row.memory_id.into_inner() == adopted),
        "natural-language question must match via its content words"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn relevance_pagination_pages_are_disjoint_and_exhaustive()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    for _ in 0..12 {
        insert_search_abstraction(&pg, &owner, "pagegrain needle", None).await?;
    }

    let mut req = lexical_request(&owner, "pagegrain");
    req.kind = Some(EntityKind::Abstraction);
    req.limit = 5;
    let full = {
        let mut one_shot = req.clone();
        one_shot.limit = 12;
        pg.search_memories(&one_shot, &[]).await?.results
    };
    assert_eq!(full.len(), 12);

    // Every score is identical here, so this exercises the pure
    // memory_id tiebreak of the relevance keyset.
    let mut paged = Vec::new();
    let mut after = None;
    let mut hops = 0;
    loop {
        let mut page_req = req.clone();
        page_req.after = after;
        let page = pg.search_memories(&page_req, &[]).await?;
        assert!(page.results.len() <= 5);
        paged.extend(page.results);
        if !page.has_more {
            break;
        }
        let last = paged.last().expect("page emitted rows");
        after = Some(SearchCursor::Relevance {
            score_bits: last.score.to_bits(),
            memory_id: last.memory_id,
            seen: u32::try_from(paged.len())?,
        });
        hops += 1;
        assert!(hops <= 4, "pagination must terminate");
    }
    assert_eq!(paged.len(), 12, "pages must be exhaustive");
    assert_eq!(
        paged
            .iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        full.iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        "concatenated pages must equal the one-shot listing: no dupes, no gaps"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn recency_pagination_pushes_keyset_into_sql_and_rejects_mismatched_cursor()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    for _ in 0..8 {
        insert_search_abstraction(&pg, &owner, "recencygrain needle", None).await?;
    }

    let mut req = lexical_request(&owner, "recencygrain");
    req.kind = Some(EntityKind::Abstraction);
    req.order = SearchOrder::Recency;
    req.limit = 3;
    let full = {
        let mut one_shot = req.clone();
        one_shot.limit = 8;
        pg.search_memories(&one_shot, &[]).await?.results
    };
    assert_eq!(full.len(), 8);

    let mut paged = Vec::new();
    let mut after = None;
    loop {
        let mut page_req = req.clone();
        page_req.after = after;
        let page = pg.search_memories(&page_req, &[]).await?;
        paged.extend(page.results);
        if !page.has_more {
            break;
        }
        let last = paged.last().expect("page emitted rows");
        after = Some(SearchCursor::Recency {
            created_at: last.created_at,
            memory_id: last.memory_id,
            seen: u32::try_from(paged.len())?,
        });
        assert!(paged.len() <= 8, "pagination must terminate");
    }
    assert_eq!(
        paged
            .iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        full.iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        "recency pages must equal the one-shot listing"
    );

    // A cursor of the wrong class for the requested order fails closed.
    let mut mismatched = req;
    mismatched.after = Some(SearchCursor::Relevance {
        score_bits: 1.0_f32.to_bits(),
        memory_id: full[0].memory_id,
        seen: 3,
    });
    let err = pg
        .search_memories(&mismatched, &[])
        .await
        .expect_err("relevance cursor with recency order must fail");
    assert!(
        err.to_string().contains("cursor order"),
        "unexpected error: {err}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn search_pages_past_the_fifty_result_cap() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    for _ in 0..55 {
        insert_search_abstraction(&pg, &owner, "capgrain needle", None).await?;
    }

    let mut req = lexical_request(&owner, "capgrain");
    req.kind = Some(EntityKind::Abstraction);
    req.limit = 60;
    let first = pg.search_memories(&req, &[]).await?;
    assert_eq!(first.results.len(), 50, "the per-page cap holds");
    assert!(first.has_more, "matches past the cap must be reported");

    let last = first.results.last().expect("full first page");
    let mut rest_req = req.clone();
    rest_req.after = Some(SearchCursor::Relevance {
        score_bits: last.score.to_bits(),
        memory_id: last.memory_id,
        seen: 50,
    });
    let rest = pg.search_memories(&rest_req, &[]).await?;
    assert_eq!(rest.results.len(), 5, "the cursor reaches past the cap");
    assert!(!rest.has_more);
    let mut all: Vec<_> = first
        .results
        .iter()
        .chain(rest.results.iter())
        .map(|row| row.memory_id.into_inner())
        .collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(
        all.len(),
        55,
        "the two pages cover every match exactly once"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

#[tokio::test]
async fn semantic_search_plan_uses_hnsw_index() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();
    insert_embedded_memory(&pg, &owner, "plan probe", [1.0, 0.0, 0.0]).await?;

    let mut req = semantic_request(&owner, padded_embedding([1.0, 0.0, 0.0]));
    // No schema filter: keeps the bind list to owner arrays + vector + model.
    req.schema_id = None;
    let sql = proxima_storage_pg::verbs::query::semantic_search_sql_for_tests(&req, &[], 40, 512)?;
    let explain_sql = format!("EXPLAIN (FORMAT JSON, COSTS OFF) {sql}");

    let (owner_kind, owner_id) = owner.columns();
    let mut tx = pg.pool_for_tests().begin().await?;
    // The production session settings, plus seqscan/sort penalized so the
    // assertion is about capability, not tiny-table costing: the only way
    // to satisfy `ORDER BY emb.vec <=> $query` without an explicit sort is
    // the HNSW scan, so if the shipped query shape can no longer be served
    // by the index (e.g. the ORDER BY expression stops matching the
    // operator class), no planner setting can save it and this fails.
    for setting in [
        "SET LOCAL hnsw.ef_search = 100",
        "SET LOCAL hnsw.iterative_scan = relaxed_order",
        "SET LOCAL enable_seqscan = off",
        "SET LOCAL enable_sort = off",
    ] {
        // SQL-POLICY: fixed-fragment
        sqlx::query(setting).execute(&mut *tx).await?;
    }
    // SQL-POLICY: fixed-fragment — EXPLAIN prefix over the audited
    // production builder's parameterized SQL; only bound values vary.
    let plan: serde_json::Value = sqlx::query_scalar(sqlx::AssertSqlSafe(explain_sql.as_str()))
        .bind(vec![owner_kind])
        .bind(vec![owner_id])
        .bind(vector_literal(&padded_embedding([1.0, 0.0, 0.0])))
        .bind("test-embed")
        .fetch_one(&mut *tx)
        .await?;
    tx.rollback().await?;

    let rendered = plan.to_string();
    assert!(
        rendered.contains("idx_embeddings_vec_hnsw"),
        "the semantic branch must scan the HNSW index; plan:\n{rendered}"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

fn hybrid_request(owner: &Owner, query: &str, query_embedding: Vec<f32>) -> MemorySearchRequest {
    MemorySearchRequest {
        owner: *owner,
        read_owners: vec![*owner],
        query: query.into(),
        mode: SearchMode::Hybrid,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 10,
        kind: Some(EntityKind::Abstraction),
        schema_id: Some(SchemaId::new("test/search-abstraction-v1".into())),
        tags: Vec::new(),
        tag_match: TagMatch::Any,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        min_score: None,
        semantic_weight: None,
        after: None,
        query_embedding: Some(query_embedding),
        embedding_model_id: Some("test-embed".into()),
    }
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
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, created_at,
             kind, text, operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'proxima-test/tagged-abstraction-v1', 1,
                 $4, 'Abstraction', $5, 'AtoA',
                 '00000000-0000-0000-0000-000000000321'::uuid,
                 '00000000-0000-0000-0000-000000000322'::uuid, NULL,
                 'test-model', 'test-v1')"
    )
    .bind(input.memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(input.created_at)
    .bind(input.body)
    .execute(pg.pool_for_tests())
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
    .execute(pg.pool_for_tests())
    .await?;
    if let Some(embedding) = input.embedding {
        insert_embedding_with_head(
            pg.pool_for_tests(),
            EntityKind::Abstraction,
            input.memory_id,
            "test-embed",
            1,
            &padded_embedding(embedding),
            owner_kind,
            owner_id,
        )
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
    insert_embedded_memory_with_schema(pg, owner, "test/search-abstraction-v1", text, embedding)
        .await
}

async fn insert_embedded_memory_with_schema(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    schema_id: &str,
    text: &str,
    embedding: &[f32],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, $4, 1,
                 'Abstraction', $5, 'AtoA',
                 '00000000-0000-0000-0000-000000000323'::uuid,
                 '00000000-0000-0000-0000-000000000324'::uuid, NULL,
                 'test-model', 'test-v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    insert_embedding_with_head(
        pg.pool_for_tests(),
        EntityKind::Abstraction,
        memory_id,
        "test-embed",
        1,
        embedding,
        owner_kind,
        owner_id,
    )
    .await?;
    Ok(memory_id)
}

async fn insert_text_memory(
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
         VALUES ($1, $2, $3, 'test/search-attribution-v1', 1,
                 'Abstraction', $4, 'AtoA',
                 '00000000-0000-0000-0000-000000000325'::uuid,
                 '00000000-0000-0000-0000-000000000326'::uuid, NULL,
                 'test-model', 'test-v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

async fn insert_search_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    supersedes: Option<Uuid>,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version, supersedes)
         VALUES ($1, $2, $3, 'test/search-abstraction-v1', 1,
                 'Abstraction', $4, 'AtoA',
                 '00000000-0000-0000-0000-000000000327'::uuid,
                 '00000000-0000-0000-0000-000000000328'::uuid, NULL,
                 'test-model', 'test-v1', $5)"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .bind(supersedes)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

async fn ingest_fact_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    schema_id: &str,
    payload: &[u8],
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    use proxima_core::verbs::fact_ingest::{
        Citation, CitationMappingHint, CitedObjectHint, FactWriteCommand,
    };

    let permit = crate::common::owner_write_permit(owner, proxima_core::AccessKind::Fact).await?;
    let now = time::OffsetDateTime::now_utc();
    let outcome = pg
        .ingest_fact_atomic(
            &permit,
            &FactWriteCommand {
                schema_id: SchemaId::new(schema_id.to_string()),
                schema_version: SchemaVersion::new(1),
                payload: payload.to_vec(),
                rendered_text: None,
                receipt: Some(FactReceiptDraft {
                    source_id: SourceId::new("test/search"),
                    source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                    observed_at: now,
                    occurred_at: now,
                }),
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
        owner: *owner,
        read_owners: vec![*owner],
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
        min_score: None,
        semantic_weight: None,
        after: None,
        query_embedding: None,
        embedding_model_id: None,
    }
}

fn semantic_request(owner: &Owner, query_embedding: Vec<f32>) -> MemorySearchRequest {
    MemorySearchRequest {
        owner: *owner,
        read_owners: vec![*owner],
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
        min_score: None,
        semantic_weight: None,
        after: None,
        query_embedding: Some(query_embedding),
        embedding_model_id: Some("test-embed".into()),
    }
}

fn tagged_search_request(owner: &Owner, query: &str, mode: SearchMode) -> MemorySearchRequest {
    MemorySearchRequest {
        owner: *owner,
        read_owners: vec![*owner],
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
        min_score: None,
        semantic_weight: None,
        after: None,
        query_embedding: None,
        embedding_model_id: None,
    }
}

fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
    let mut embedding = vec![0.0; EMBEDDING_DIM];
    embedding[..prefix.len()].copy_from_slice(&prefix);
    embedding
}

#[allow(clippy::too_many_arguments)]
async fn insert_embedding_with_head(
    pool: &sqlx::PgPool,
    entity_kind: EntityKind,
    entity_id: Uuid,
    model_id: &str,
    embedding_version: i32,
    embedding: &[f32],
    owner_kind: proxima_core::OwnerRefKind,
    owner_id: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_kind, owner_id)
         VALUES ($1, $2, $3, $4, $5::vector, $6, $7)",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(embedding_version)
    .bind(model_id)
    .bind(vector_literal(embedding))
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_kind, entity_id, model_id, embedding_version, owner_kind, owner_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (entity_kind, entity_id, model_id)
         DO UPDATE SET
             embedding_version = EXCLUDED.embedding_version,
             owner_kind = EXCLUDED.owner_kind,
             owner_id = EXCLUDED.owner_id,
             updated_at = now()",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .bind(embedding_version)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await?;
    Ok(())
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
