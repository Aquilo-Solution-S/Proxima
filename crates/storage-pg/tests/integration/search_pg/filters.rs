//! Request-level filters: tags, created-at ranges, and owner/World read-set scoping.

use super::{
    TaggedAbstractionInsert, any_kind_lexical_request, create_memory_text_sidecar,
    create_tagged_search_sidecars, drop_db, fresh_pg, insert_memory_text_abstraction,
    insert_search_abstraction, insert_tagged_abstraction, insert_text_memory,
    memory_text_projection, owner_fixture, padded_embedding, tagged_abstraction_projection,
    tagged_search_request,
};

use proxima_core::storage_ports::*;
use proxima_core::verbs::query::{SearchMode, TagMatch};
use proxima_core::{MemoryId, OwnerRef, UserId};
use uuid::Uuid;

/// A tag filter is the only predicate that scopes a search to a subset of
/// a corpus, and the base `memories` branch cannot carry one — so a flavor
/// that wants scoped search must project. Before
/// `SearchProjectionColumnKind::MemoryText` the only way to project the
/// text was to keep a byte-identical copy of `memories.text` in the
/// sidecar. Here the sidecar has no text column at all: a lexical match on
/// words that exist *only* in `memories.text` proves the branch read the
/// memory row, and the snippet equality proves a scoped query and an
/// unscoped one cannot disagree about a memory's text.
#[tokio::test]
async fn tag_scoped_search_projects_memory_text_without_a_sidecar_copy()
-> Result<(), Box<dyn std::error::Error>> {
    const HYDRAULIC_TEXT: &str = "hydraulic accumulator bladder precharge procedure";

    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    create_memory_text_sidecar(pg.pool_for_tests()).await?;

    let owner = owner_fixture();
    let now = time::OffsetDateTime::from_unix_timestamp(1_700_000_000)?;
    let hydraulic = insert_memory_text_abstraction(
        &pg,
        &owner,
        Uuid::from_u128(41),
        HYDRAULIC_TEXT,
        &["manual", "hydraulics"],
        now,
    )
    .await?;
    let electrical = insert_memory_text_abstraction(
        &pg,
        &owner,
        Uuid::from_u128(42),
        "electrical accumulator bonding strap precharge interval",
        &["manual", "electrical"],
        now + time::Duration::seconds(1),
    )
    .await?;
    let projections = vec![memory_text_projection()];

    let mut scoped = any_kind_lexical_request(&owner, "accumulator precharge");
    scoped.tags = vec!["hydraulics".into()];
    let rows = pg.search_memories(&scoped, &projections).await?.results;
    let ids: Vec<_> = rows.iter().map(|row| row.memory_id).collect();
    assert!(
        ids.contains(&hydraulic),
        "a sidecar with no text column must still match on the memory's text: {rows:#?}"
    );
    assert!(
        !ids.contains(&electrical),
        "the tag filter must still exclude the other tag: {rows:#?}"
    );

    let scoped_snippet = rows
        .iter()
        .find(|row| row.memory_id == hydraulic)
        .map(|row| row.snippet.clone())
        .expect("the hydraulic row is present");
    assert_eq!(
        scoped_snippet, HYDRAULIC_TEXT,
        "the projected search text is the memory's own text, not a sidecar copy"
    );

    // The same memory, reached through the base branch instead: a tag
    // filter excludes that branch entirely, so the two spellings of the
    // snippet have to agree or a scoped and an unscoped search return
    // different text for one memory.
    let unscoped = any_kind_lexical_request(&owner, "accumulator precharge");
    let rows = pg.search_memories(&unscoped, &projections).await?.results;
    let unscoped_snippet = rows
        .iter()
        .find(|row| row.memory_id == hydraulic)
        .map(|row| row.snippet.clone())
        .expect("an unscoped search still finds the memory");
    assert_eq!(unscoped_snippet, scoped_snippet);

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

/// World rows carry `owner_kind = 'world', owner_id = NULL`
/// (`Engine::publish_to_world` transfers ownership in place). The
/// owner-scope gate splits the read set into an equality join plus a
/// dedicated World arm at SQL-build time; this pins the semantics of
/// both arms: World-published memories surface exactly when World is in
/// the read set, alongside (not instead of) the reader's own rows.
#[tokio::test]
async fn world_published_memory_needs_world_in_read_set() -> Result<(), Box<dyn std::error::Error>>
{
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;

    let author = owner_fixture();
    let reader = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let published = insert_search_abstraction(&pg, &author, "world beacon xylophone", None).await?;
    let private = insert_search_abstraction(&pg, &reader, "reader private xylophone", None).await?;
    // Mirror the publish-to-World owner TRANSFER (an UPDATE, not an
    // insert) without dragging in the engine's permit plumbing.
    sqlx::query(
        "UPDATE proxima_core.memories SET owner_kind = 'world', owner_id = NULL
          WHERE memory_id = $1",
    )
    .bind(published)
    .execute(pg.pool_for_tests())
    .await?;

    let mut req = any_kind_lexical_request(&reader, "xylophone");
    let own_only = pg.search_memories(&req, &[]).await?.results;
    assert_eq!(
        own_only
            .iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        vec![private],
        "without World in the read set only the reader's own row matches"
    );

    req.read_owners = vec![reader, OwnerRef::World];
    let with_world = pg.search_memories(&req, &[]).await?.results;
    let mut ids: Vec<_> = with_world
        .iter()
        .map(|row| row.memory_id.into_inner())
        .collect();
    ids.sort_unstable();
    let mut expected = vec![published, private];
    expected.sort_unstable();
    assert_eq!(
        ids, expected,
        "World in the read set surfaces the published row alongside the reader's own"
    );

    req.read_owners = vec![OwnerRef::World];
    let world_only = pg.search_memories(&req, &[]).await?.results;
    assert_eq!(
        world_only
            .iter()
            .map(|row| row.memory_id.into_inner())
            .collect::<Vec<_>>(),
        vec![published],
        "a World-only read set surfaces exactly the published row"
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}

/// `%` and `_` in a query are characters the user typed, not wildcards.
///
/// The decoy is reachable *only* through the substring arm: its text
/// shares no lexeme with the query, so a hit proves wildcard expansion.
#[tokio::test]
async fn like_metacharacters_in_a_query_are_literal() -> Result<(), Box<dyn std::error::Error>> {
    let (pg, db_name) = fresh_pg().await;
    pg.run_migrations().await?;
    let owner = owner_fixture();

    let literal = insert_search_abstraction(&pg, &owner, "alpha_beta configuration", None).await?;
    let decoy = insert_search_abstraction(&pg, &owner, "alphaXbeta configuration", None).await?;

    let req = any_kind_lexical_request(&owner, "alpha_beta");
    let ids: Vec<_> = pg
        .search_memories(&req, &[])
        .await?
        .results
        .iter()
        .map(|row| row.memory_id.into_inner())
        .collect();
    assert!(
        ids.contains(&literal),
        "the memory containing the literal text must match"
    );
    assert!(
        !ids.contains(&decoy),
        "`_` must not match an arbitrary character: {ids:?}"
    );

    // A query that is nothing but wildcards must not sweep the corpus.
    let req = any_kind_lexical_request(&owner, "%");
    let swept = pg.search_memories(&req, &[]).await?.results;
    assert!(
        swept.is_empty(),
        "a bare `%` matched {} memories",
        swept.len()
    );

    drop(pg);
    drop_db(&db_name).await?;
    Ok(())
}
