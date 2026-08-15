//! Semantic-branch search behaviour: vector ranking, owner isolation, chunk scoring, and pre-limit filtering.

use super::{
    SHIPPED_ANN_WINDOW, TaggedAbstractionInsert, WIDE_ANN_WINDOW, brute_cosine,
    create_tagged_search_sidecars, drop_db, fresh_pg, insert_embedded_memory,
    insert_embedded_memory_with_schema, insert_embedded_memory_with_vec,
    insert_embedding_with_head, insert_search_abstraction, insert_tagged_abstraction,
    owner_fixture, padded_embedding, pg_with_ann_window, semantic_request,
    tagged_abstraction_projection, tagged_search_request, vector_literal,
};

use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::{OwnerRef, SchemaId, UserId};
use proxima_storage_pg::SemanticIndexFirst;
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
async fn semantic_search_scores_chunked_memory_by_best_chunk_without_duplicates()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let (owner_kind, owner_id) = owner.columns();
    let far_chunk = padded_embedding([0.0, 1.0, 0.0]);
    let near_chunk = padded_embedding([1.0, 0.0, 0.0]);
    let middling = padded_embedding([0.5, 0.5, 0.0]);
    let query_vec = padded_embedding([1.0, 0.0, 0.0]);

    // One over-limit memory embedded as two chunks under one version: the
    // first chunk is orthogonal to the query, the second matches exactly.
    let chunked =
        insert_embedded_memory_with_vec(&pg, &owner, "chunked oversized memory", &far_chunk)
            .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_kind, owner_id, chunk_index)
         VALUES ($1, $2, 1, 'test-embed', $3::vector, $4, $5, 1)",
    )
    .bind(EntityKind::Abstraction)
    .bind(chunked)
    .bind(vector_literal(&near_chunk))
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pg.pool_for_tests())
    .await?;
    let plain = insert_embedded_memory_with_vec(&pg, &owner, "plain memory", &middling).await?;

    let rows = pg
        .search_memories(&semantic_request(&owner, query_vec), &[])
        .await?
        .results;

    let ids: Vec<Uuid> = rows.iter().map(|row| row.memory_id.into_inner()).collect();
    assert_eq!(
        ids,
        vec![chunked, plain],
        "chunked memory must rank by its best chunk and appear exactly once"
    );
    assert!(
        (rows[0].similarity_score - 1.0).abs() <= 1.0e-4,
        "best-chunk similarity must win, got {}",
        rows[0].similarity_score
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

/// Owner scope and the embedding head decide which rows the candidate limit
/// may cut — and which arm of `semantic_index_first` still guarantees it.
///
/// The fixture puts 600 rows at cosine similarity 1.0 in front of the one
/// authorized, current target: 300 owned by somebody else, and 300 whose
/// near vector is a superseded embedding version. Only 301 of them are
/// owned by the searcher.
///
/// - `off` keeps both joins UNDER the nearest-neighbour window, so the
///   window is a budget of eligible rows and the target always survives.
/// - `pushdown` puts the owner arms on the scan itself, so the window is
///   spent on the searcher's own rows and the target survives a window the
///   full fixture would overflow. This is the one thing pushdown buys over
///   overfetch, and it is exactly the owner/privacy predicate.
/// - `overfetch` spends the window before either join is known, so the
///   foreign rows can displace the target. That is the declared ANN-pool
///   approximation, not a lost row: widen the window past the fixture and
///   the target comes back.
///
/// No arm may return a row the joins exclude — the approximation costs
/// recall, never scope.
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

    for (arm, exact_at_shipped_window) in [
        (SemanticIndexFirst::Off, true),
        (SemanticIndexFirst::Overfetch, false),
        (SemanticIndexFirst::Pushdown, true),
    ] {
        let shipped = pg_with_ann_window(&db_name, arm, SHIPPED_ANN_WINDOW).await?;
        let rows = shipped.search_memories(&req, &[]).await?.results;
        assert!(
            rows.iter().all(|row| row.memory_id.into_inner() == target),
            "{arm:?} returned a row outside the owner scope or off the head: {rows:#?}"
        );
        let found = rows.first().map(|row| row.memory_id.into_inner());
        if exact_at_shipped_window {
            assert_eq!(found, Some(target), "{arm:?} lost the target: {rows:#?}");
        } else {
            assert_eq!(
                found, None,
                "{arm:?} is declared to spend its window before the joins: {rows:#?}"
            );
        }

        let wide = pg_with_ann_window(&db_name, arm, WIDE_ANN_WINDOW).await?;
        let rows = wide.search_memories(&req, &[]).await?.results;
        assert_eq!(
            rows.first().map(|row| row.memory_id.into_inner()),
            Some(target),
            "{arm:?} lost the target under a window wider than the fixture: {rows:#?}"
        );
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// The query predicates decide which rows the candidate limit may cut — on
/// the `off` arm exactly, and on the index-first arms only as far as the
/// nearest-neighbour window reaches.
///
/// 540 decoys sit at cosine similarity 1.0 under a DIFFERENT `schema_id`,
/// in front of the one memory the request asks for. `off` joins the
/// eligibility set under the window, so the window is a budget of rows that
/// already passed `schema_id` and the target always survives. Neither
/// index-first arm pushes `schema_id` (or `kind`, `tags`, `since`,
/// `until`) onto the scan — only owner and model ride it, and only under
/// `pushdown` — so both spend the window on rows the schema filter will
/// discard. Over a window the fixture overflows, the target is displaced.
///
/// That is the declared ANN-pool approximation, and the widened-window pass
/// is what tells it apart from a broken join: nothing is lost structurally,
/// the rows are simply past the cut. What no arm may do is answer with a
/// row of the wrong schema — the filter is inexact in recall, never in
/// what it admits.
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

    for arm in [
        SemanticIndexFirst::Off,
        SemanticIndexFirst::Overfetch,
        SemanticIndexFirst::Pushdown,
    ] {
        let shipped = pg_with_ann_window(&db_name, arm, SHIPPED_ANN_WINDOW).await?;
        let rows = shipped.search_memories(&req, &[]).await?.results;
        assert!(
            rows.iter().all(|row| row.memory_id.into_inner() == target),
            "{arm:?} answered with a row of the filtered-out schema: {rows:#?}"
        );
        let found = rows.first().map(|row| row.memory_id.into_inner());
        if matches!(arm, SemanticIndexFirst::Off) {
            assert_eq!(found, Some(target), "{arm:?} lost the target: {rows:#?}");
        } else {
            assert_eq!(
                found, None,
                "{arm:?} is declared to spend its window before the query predicates: {rows:#?}"
            );
        }

        let wide = pg_with_ann_window(&db_name, arm, WIDE_ANN_WINDOW).await?;
        let rows = wide.search_memories(&req, &[]).await?.results;
        assert_eq!(
            rows.first().map(|row| row.memory_id.into_inner()),
            Some(target),
            "{arm:?} lost the target under a window wider than the fixture: {rows:#?}"
        );
    }

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// A page is a budget of memories, not embedding rows. Chunked embeddings
/// put several rows in the ANN scan for one memory; collapse after LIMIT
/// would starve the page and lie on `has_more`.
#[tokio::test]
async fn chunked_memories_do_not_starve_the_semantic_page() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let owner = owner_fixture();
    let (owner_kind, owner_id) = owner.columns();
    let query_vec = padded_embedding([1.0, 0.0, 0.0]);

    // Three memories that each hold many chunks, all near the query, so the
    // nearest-neighbour scan is dominated by their rows.
    for memory in 0..3 {
        let id = insert_embedded_memory_with_vec(
            &pg,
            &owner,
            &format!("chunked memory {memory}"),
            &padded_embedding([0.99, 0.01, 0.0]),
        )
        .await?;
        for chunk_index in 1..12 {
            sqlx::query(
                "INSERT INTO proxima_core.embeddings
                    (entity_kind, entity_id, embedding_version, model_id, vec,
                     owner_kind, owner_id, chunk_index)
                 VALUES ($1, $2, 1, 'test-embed', $3::vector, $4, $5, $6)",
            )
            .bind(EntityKind::Abstraction)
            .bind(id)
            .bind(vector_literal(&padded_embedding([0.99, 0.01, 0.0])))
            .bind(owner_kind)
            .bind(owner_id)
            .bind(chunk_index)
            .execute(pg.pool_for_tests())
            .await?;
        }
    }
    // Plus enough single-chunk memories to fill a page on their own.
    for memory in 0..12 {
        insert_embedded_memory_with_vec(
            &pg,
            &owner,
            &format!("plain memory {memory}"),
            &padded_embedding([0.95, 0.05, 0.0]),
        )
        .await?;
    }

    let mut req = semantic_request(&owner, query_vec);
    req.limit = 5;
    let page = pg.search_memories(&req, &[]).await?;

    assert_eq!(
        page.results.len(),
        5,
        "page must fill with distinct memories, not be starved by one memory's chunks"
    );
    let unique: std::collections::HashSet<Uuid> = page
        .results
        .iter()
        .map(|row| row.memory_id.into_inner())
        .collect();
    assert_eq!(unique.len(), 5, "every result must be a distinct memory");
    assert!(
        page.has_more,
        "15 matching memories against a limit of 5 must report has_more"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// Collapse of a memory reachable through more than one candidate branch
/// picks one `search_text`: the schema's own projection wins (branches
/// share `created_at`, so `ORDER BY created_at DESC` does not discriminate).
/// A NULL projection never displaces text the base branch could supply.
/// This memory is admitted by both the base branch (`memories.text`) and
/// its schema sidecar (`concat_ws`), so the snippet names which one won.
#[tokio::test]
async fn semantic_snippet_prefers_the_schema_projection_over_memory_text()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    create_tagged_search_sidecars(pg.pool_for_tests()).await?;
    let owner = owner_fixture();

    insert_tagged_abstraction(
        &pg,
        &owner,
        TaggedAbstractionInsert {
            memory_id: Uuid::from_u128(0x5171),
            title: "Projected title",
            body: "shared body prose",
            tags: &["gamma"],
            created_at: time::OffsetDateTime::now_utc(),
            embedding: Some([1.0, 0.0, 0.0]),
        },
    )
    .await?;

    let mut req = tagged_search_request(&owner, "shared body prose", SearchMode::Semantic);
    req.query_embedding = Some(padded_embedding([1.0, 0.0, 0.0]));
    req.embedding_model_id = Some("test-embed".into());

    let rows = pg
        .search_memories(&req, &[tagged_abstraction_projection()])
        .await?
        .results;

    assert_eq!(rows.len(), 1, "the seeded memory is the only candidate");
    assert_eq!(
        rows[0].snippet, "Projected title shared body prose gamma",
        "the snippet must come from the schema's projection, not from \
         memories.text ({:?})",
        rows[0].snippet
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
