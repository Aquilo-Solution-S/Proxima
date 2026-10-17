//! Projection-first core search: one ranked statement per flavor over
//! `<flavor>.projection`, a DECLARED substring arm over the schemas it
//! returned nothing for, then admit via `memory_head`.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

#[path = "search_sidecar/embedding_chunks.rs"]
mod embedding_chunks;

#[path = "search_sidecar/query_stopwords.rs"]
mod query_stopwords;

#[path = "search_sidecar/hybrid_rrf.rs"]
mod hybrid_rrf;

use proxima_core::flavor::{
    Band, BandComparability, LanguagePolicy, RankSource, SubstringArm, WEIGHT_UNIFORM,
};
use proxima_core::flavor0::{BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING};
use proxima_core::storage_ports::MemoryReadPort;
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind, RenderBands,
};
use proxima_core::{OwnerId, SchemaId, SchemaVersion, SearchProjectionColumnKind, UserId};
use proxima_pg_testkit::{db_url, drop_db, split_role_urls_for};
use proxima_storage_pg::test_fixtures::create_core_db;
use proxima_storage_pg::{PgMigrationStorage, PgPlatformScope, PgPoolConfig, PgStorage, PgTuning};
use uuid::Uuid;

/// The out-of-tree fixture flavor's bands: core's, referenced. Referencing
/// them IS the band-comparability claim, which is what makes
/// `BandComparability::CoreBands` below an assertion rather than a label.
const DOCS_BANDS: &[Band] = &[BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING];

/// Superseded substring matches in the starvation corpus. Above the
/// `overfetch` a `limit = 1` request gets (`1 * 20 = 20`), so the window is
/// the scarce thing rather than the corpus.
const SUBSTRING_BACKLOG: usize = 25;

fn note_projection() -> MemorySearchProjection {
    // The shipped declaration, not a second copy of it. This fixture used
    // to restate `core/agent-note-v1`'s columns by hand, which meant the
    // test agreed with the contract only by coincidence.
    proxima_core::FlavorRegistry::new()
        .freeze_or_panic_for_tests()
        .search_projections()
        .iter()
        .find(|projection| projection.schema_id.as_str() == "core/agent-note-v1")
        .expect("core/agent-note-v1 is a search surface")
        .clone()
}

/// A query vector in the fixtures' 1024-wide `test-embed` space.
fn test_semantic(vector: Vec<f32>) -> proxima_core::SpaceVector {
    proxima_core::SpaceVector::new(
        proxima_core::EmbeddingSpace::new("test-embed", proxima_core::EmbeddingDim::D1024),
        vector,
    )
    .expect("1024-wide test vector")
}

fn search_req(owner: OwnerId, query: &str) -> MemorySearchRequest {
    MemorySearchRequest {
        owner,
        read_owners: vec![owner],
        query: query.into(),
        mode: SearchMode::Lexical,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 8,
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
        semantic: None,
    }
}

/// The production maintenance statement, run against a hand-seeded row.
///
/// The tests below write `memory` and sidecar rows directly (they are
/// testing the READ path), so they have to keep the projection themselves.
/// They do it with the generator's own statement rather than a restated
/// INSERT: a test that hand-wrote the vector expression would pass while
/// production wrote a different one.
async fn project(
    pool: &sqlx::PgPool,
    t: Uuid,
    schema_id: &str,
    language: Option<&str>,
) -> Result<(), sqlx::Error> {
    let schema = proxima_core::FLAVOR_0
        .schemas
        .iter()
        .find(|schema| schema.schema_id().as_str() == schema_id)
        .expect("declared schema");
    let sql =
        proxima_storage_pg::projection::projection_insert_sql(&proxima_core::FLAVOR_0, schema)
            .expect("the generator emits a valid statement");
    // SQL-POLICY: generated
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(t)
        .bind(language)
        .bind(schema_id)
        .execute(pool)
        .await?;
    Ok(())
}

async fn seed_note(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    title: &str,
    body: &str,
) -> Result<Uuid, sqlx::Error> {
    seed_note_lang(pool, owner, title, body, None, &[]).await
}

async fn seed_note_lang(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    title: &str,
    body: &str,
    language: Option<&str>,
    tags: &[&str],
) -> Result<Uuid, sqlx::Error> {
    let owner_id = owner.into_inner();
    sqlx::query(
        "INSERT INTO proxima_core.owners (owner_id, kind)
         VALUES ($1, 'personal')
         ON CONFLICT DO NOTHING",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'fact', 'core/agent-note-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner_id)
    .bind(t)
    .execute(pool)
    .await?;
    // The stamp and the row it promises land in one transaction: a memory row
    // that names a sidecar table it has no row in is refused at COMMIT.
    let mut stamped = pool.begin().await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
             (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
         VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1',
                 ARRAY['proxima_core.agent_note_v1'], uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .execute(&mut *stamped)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(t)
    .bind(Uuid::now_v7())
    .bind(title)
    .bind(body)
    .bind(tags)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    project(pool, t, "core/agent-note-v1", language).await?;
    Ok(t)
}

fn embed_vector() -> pgvector::Vector {
    embed_vector_xy(1.0, 0.0)
}

fn embed_vector_xy(x: f32, y: f32) -> pgvector::Vector {
    let mut values = vec![0.0; 1024];
    values[0] = x;
    values[1] = y;
    pgvector::Vector::from(values)
}

async fn create_docs_search_surface(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE SCHEMA proxima_docs")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_docs.section_text_v1 (
            t uuid PRIMARY KEY,
            text text NOT NULL,
            tags text[] NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TABLE proxima_docs.projection (
            memory_id uuid NOT NULL
                REFERENCES proxima_core.memory (t) ON DELETE CASCADE,
            schema_id text NOT NULL,
            owner_id uuid NOT NULL REFERENCES proxima_core.owners (owner_id),
            search_tsv tsvector NOT NULL,
            tag text[] NOT NULL DEFAULT '{}',
            lexical_language regconfig NOT NULL
                DEFAULT proxima_core.lexical_config()
                REFERENCES proxima_core.lexical_languages (config),
            PRIMARY KEY (memory_id, schema_id)
         )",
    )
    .execute(pool)
    .await?;
    sqlx::query("GRANT USAGE ON SCHEMA proxima_docs TO proxima_test_runtime")
        .execute(pool)
        .await?;
    sqlx::query("GRANT SELECT ON ALL TABLES IN SCHEMA proxima_docs TO proxima_test_runtime")
        .execute(pool)
        .await?;
    Ok(())
}

fn docs_projection() -> MemorySearchProjection {
    MemorySearchProjection {
        schema_id: SchemaId::new("proxima-docs/section-text-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Abstraction,
        sidecar_table: "proxima_docs.section_text_v1".into(),
        sidecar_key_column: "t".into(),
        fields: vec![MemorySearchProjectionField {
            column: "text".into(),
            kind: SearchProjectionColumnKind::Text,
            weight: WEIGHT_UNIFORM,
        }],
        projection_table: "proxima_docs.projection".into(),
        tag_column: Some("tags".into()),
        language: LanguagePolicy::PerRow {
            column: "lexical_language",
        },
        rank_weights: None,
        render_bands: RenderBands::resolve(DOCS_BANDS).ok(),
        substring: SubstringArm::MemoryFirstNestedLoop,
        overfetch_k: 1_000,
        band_comparability: BandComparability::CoreBands,
        rank_source: RankSource::Projection,
    }
}

async fn seed_docs_note(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    origin_t: Uuid,
    text: &str,
    tags: &[&str],
) -> Result<Uuid, sqlx::Error> {
    let owner_id = owner.into_inner();
    let handle = Uuid::now_v7();
    let t = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
         VALUES ($1, 'abstraction', 'proxima-docs/section-text-v1', $2, $3)",
    )
    .bind(handle)
    .bind(owner_id)
    .bind(t)
    .execute(pool)
    .await?;
    let mut hash = [0_u8; 32];
    hash[..16].copy_from_slice(t.as_bytes());
    hash[16..].copy_from_slice(t.as_bytes());
    let content_id: Uuid = sqlx::query_scalar(
        "INSERT INTO proxima_core.content (owner_id, schema_id, content_hash)
         VALUES ($1, 'proxima-docs/section-text-v1', $2)
         RETURNING content_id",
    )
    .bind(owner_id)
    .bind(hash.as_slice())
    .fetch_one(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.memory
            (handle, t, kind, owner_id, schema_id, origins, content_id, \"by\", via)
         VALUES ($1, $2, 'abstraction', $3, 'proxima-docs/section-text-v1', ARRAY[$4]::uuid[], $5, '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .bind(origin_t)
    .bind(content_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_docs.section_text_v1 (t, text, tags)
         VALUES ($1, $2, $3)",
    )
    .bind(t)
    .bind(text)
    .bind(tags)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_docs.projection
            (memory_id, schema_id, owner_id, search_tsv, tag)
         SELECT c.t,
                'proxima-docs/section-text-v1',
                m.owner_id,
                COALESCE(
                    proxima_core.lexical_tsv(
                        proxima_core.lexical_config(),
                        proxima_core.lexical_join(VARIADIC ARRAY[NULLIF(c.text, '')])
                    ),
                    ''::tsvector
                ),
                c.tags
           FROM proxima_docs.section_text_v1 c
           JOIN proxima_core.memory m ON m.t = c.t
          WHERE c.t = $1",
    )
    .bind(t)
    .execute(pool)
    .await?;
    Ok(t)
}

async fn seed_embedding(
    pool: &sqlx::PgPool,
    owner: OwnerId,
    t: Uuid,
    vector: &pgvector::Vector,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_id, model_id, dim, embedding_version, vec, owner_id)
         VALUES ($1, 'test-embed', 1024, 1, $2, $3)",
    )
    .bind(t)
    .bind(vector)
    .bind(owner.into_inner())
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_id, model_id, dim, embedding_version, owner_id)
         VALUES ($1, 'test-embed', 1024, 1, $2)",
    )
    .bind(t)
    .bind(owner.into_inner())
    .execute(pool)
    .await?;
    Ok(())
}

#[tokio::test]
async fn lexical_search_is_sidecar_first_then_owner_admit() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();

        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other = OwnerId::from(UserId::new(Uuid::now_v7()));
        let ours = seed_note(
            pool,
            owner,
            "Retrieval surface",
            "hybrid substrate keyword needle",
        )
        .await?;
        let _theirs = seed_note(
            pool,
            other,
            "Retrieval surface",
            "hybrid substrate keyword needle",
        )
        .await?;

        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "keyword needle"),
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            page.results.len(),
            1,
            "other-owner sidecar hit must not admit"
        );
        assert_eq!(page.results[0].memory_id.into_inner(), ours);
        assert!(
            page.results[0].lexical_score > 0.0,
            "banded lexical score, not the ILIKE constant 1.0"
        );
        assert!(
            page.results[0].snippet.contains("keyword needle"),
            "snippet comes from the sidecar scan"
        );

        // The substring arm is DECLARED, not blanket. `core/agent-note-v1`
        // declares `MemoryFirstNestedLoop`, so a partial word with no
        // tsvector lexeme still lands — and it lands at the declared
        // substring band, from the one statement that runs over exactly
        // the schemas the ranked arm returned nothing for.
        let substring = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "eedle"),
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            substring.results.len(),
            1,
            "a declared substring arm must still hit where no lexeme does"
        );
        assert_eq!(substring.results[0].memory_id.into_inner(), ours);
        assert!(
            (substring.results[0].lexical_score - 0.25).abs() < f32::EPSILON,
            "substring hits score at the declared band floor, got {}",
            substring.results[0].lexical_score
        );
        assert!(
            substring.results[0].snippet.contains("keyword needle"),
            "the snippet is hydrated for the page whichever arm found the row"
        );

        // Mutation target: turn the arm off and the row disappears. With no
        // blanket retry behind it, the arm is the only thing that finds this
        // row.
        let arm_off = MemorySearchProjection {
            substring: proxima_core::flavor::SubstringArm::Off,
            ..note_projection()
        };
        let refused = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "eedle"),
                &[arm_off],
            )
            .await?;
        assert!(
            refused.results.is_empty(),
            "a schema declaring SubstringArm::Off contributes no statement and no rows"
        );

        let miss = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "no-such-lexeme-xyzzy"),
                &[note_projection()],
            )
            .await?;
        assert!(miss.results.is_empty());

        let flavor = MemorySearchProjection {
            schema_id: SchemaId::new("proxima-code/code-chunk-v1".into()),
            sidecar_table: "proxima_code.code_chunk_v1".into(),
            projection_table: "proxima_code.projection".into(),
            band_comparability: BandComparability::Divergent {
                why: "the flavor has its own score bands",
            },
            rank_source: RankSource::SidecarWithProjectionOwner {
                why: "the flavor supplies its own renderer",
            },
            render_bands: None,
            ..note_projection()
        };
        let skipped = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "keyword needle"),
                &[flavor],
            )
            .await?;
        assert!(
            skipped.results.is_empty(),
            "a divergent sidecar-ranked flavor stays outside the core renderer"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("sidecar-first search test failed");
}

#[tokio::test]
async fn lexical_search_does_not_let_other_owner_fill_overfetch() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other = OwnerId::from(UserId::new(Uuid::now_v7()));
        let ours = seed_note(pool, owner, "zebra", "owner-local zebra needle").await?;
        for i in 0..25 {
            seed_note(
                pool,
                other,
                &format!("zebra {i}"),
                "other-owner zebra flood",
            )
            .await?;
        }
        let mut req = search_req(owner, "zebra");
        req.limit = 1;
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &req,
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            page.results.len(),
            1,
            "other-owner GIN hits must not occupy the overfetch window"
        );
        assert_eq!(page.results[0].memory_id.into_inner(), ours);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("owner-at-scan search test failed");
}

#[tokio::test]
async fn tagged_search_scans_flavor_sidecars() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    split_role_urls_for(&db_name, &["proxima_docs"])
        .await
        .expect("split docs roles");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let other_owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             SELECT unnest($1::uuid[]), 'personal' ON CONFLICT DO NOTHING",
        )
        .bind(vec![owner.into_inner(), other_owner.into_inner()])
        .execute(pool)
        .await?;
        create_docs_search_surface(pool).await?;
        let origin = seed_note_lang(pool, owner, "Origin", "origin body", None, &[]).await?;
        let t = seed_docs_note(
            pool,
            owner,
            origin,
            "Antriebswelle im Getriebe",
            &["proxima-docs", "shared"],
        )
        .await?;
        let other_t = seed_docs_note(
            pool,
            owner,
            origin,
            "Antriebswelle im Motor",
            &["other", "shared"],
        )
        .await?;
        let other_origin =
            seed_note_lang(pool, other_owner, "Origin", "origin body", None, &[]).await?;
        let hidden_t = seed_docs_note(
            pool,
            other_owner,
            other_origin,
            "Antriebswelle im Getriebe",
            &["proxima-docs", "shared"],
        )
        .await?;

        let projection = docs_projection();
        let projections = std::slice::from_ref(&projection);
        let scope = owner_scope_for_tests(&[owner]);
        let ids = |page: &proxima_core::verbs::query::MemorySearchPage| {
            let mut ids = page
                .results
                .iter()
                .map(|result| result.memory_id.into_inner())
                .collect::<Vec<_>>();
            ids.sort_unstable();
            ids
        };
        let wrong_kind = pg
            .search_memories(
                Some(&scope),
                &search_req(owner, "Antriebswelle"),
                projections,
            )
            .await?;
        assert!(
            wrong_kind.results.is_empty(),
            "a Fact query must exclude Abstraction rows"
        );

        let mut unscoped = search_req(owner, "Antriebswelle");
        unscoped.kind = Some(EntityKind::Abstraction);
        let mut expected = vec![t, other_t];
        expected.sort_unstable();
        let page = pg
            .search_memories(Some(&scope), &unscoped, projections)
            .await?;
        assert_eq!(
            ids(&page),
            expected,
            "unscoped search must reach matching flavor text"
        );
        assert!(!page.has_more, "hidden owners cannot add continuation");

        let mut schema_scoped = unscoped.clone();
        schema_scoped.schema_id = Some(projection.schema_id.clone());
        let page = pg
            .search_memories(Some(&scope), &schema_scoped, projections)
            .await?;
        assert_eq!(
            ids(&page),
            expected,
            "a schema id reaches the flavor without tags"
        );

        let mut no_tags = projection.clone();
        no_tags.tag_column = None;
        let page = pg
            .search_memories(Some(&scope), &unscoped, std::slice::from_ref(&no_tags))
            .await?;
        assert_eq!(
            ids(&page),
            expected,
            "a projection need not declare tags to be searched"
        );

        for (tag_match, tags, mut expected_ids) in [
            (TagMatch::Any, vec!["proxima-docs"], vec![t]),
            (
                TagMatch::Any,
                vec!["proxima-docs", "other"],
                vec![t, other_t],
            ),
            (TagMatch::All, vec!["proxima-docs", "shared"], vec![t]),
            (TagMatch::All, vec!["proxima-docs", "other"], Vec::new()),
            (TagMatch::Any, vec!["absent"], Vec::new()),
        ] {
            let mut tagged = unscoped.clone();
            tagged.tags = tags.iter().map(|tag| (*tag).to_string()).collect();
            tagged.tag_match = tag_match;
            let page = pg
                .search_memories(Some(&scope), &tagged, projections)
                .await?;
            expected_ids.sort_unstable();
            assert_eq!(
                ids(&page),
                expected_ids,
                "{tag_match:?} tags {tags:?} narrow flavor rows"
            );
        }

        let mut tagged = unscoped.clone();
        tagged.tags = vec!["proxima-docs".into()];
        let page = pg
            .search_memories(Some(&scope), &tagged, projections)
            .await?;
        assert_eq!(page.results.len(), 1, "tagged search must hit flavor text");
        assert_eq!(page.results[0].memory_id.into_inner(), t);
        let page = pg
            .search_memories(Some(&scope), &tagged, std::slice::from_ref(&no_tags))
            .await?;
        assert!(
            page.results.is_empty(),
            "tag filtering requires a declared tag column"
        );

        let mut overbroad = unscoped;
        overbroad.read_owners.push(other_owner);
        overbroad.limit = 2;
        let page = pg
            .search_memories(Some(&scope), &overbroad, projections)
            .await?;
        assert_eq!(
            ids(&page),
            expected,
            "the owner witness still bounds admission"
        );
        assert!(!ids(&page).contains(&hidden_t));
        assert!(!page.has_more);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("flavor sidecar search failed");
}

#[tokio::test]
async fn lexical_search_matches_german_via_lexical_languages() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let t = seed_note_lang(
            pool,
            owner,
            "Tiere",
            "die Katzen schlafen auf dem Sofa",
            Some("german"),
            &[],
        )
        .await?;
        let registered: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM proxima_core.lexical_languages
                  WHERE config = 'german'::regconfig
             )",
        )
        .fetch_one(pool)
        .await?;
        assert!(registered, "insert must register the row language");

        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "Katze"),
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            page.results.len(),
            1,
            "german stem must match via language OR"
        );
        assert_eq!(page.results[0].memory_id.into_inner(), t);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("german lexical search failed");
}

#[tokio::test]
async fn simple_rows_retain_vectors_after_default_switch_and_query_analysis() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));

        sqlx::query("SELECT proxima_core.set_lexical_config('simple')")
            .execute(pool)
            .await?;
        let simple = seed_note(pool, owner, "Stopword", "the checkpoint").await?;
        sqlx::query("SELECT proxima_core.set_lexical_config('english')")
            .execute(pool)
            .await?;

        let stamped_simple: bool = sqlx::query_scalar(
            "SELECT lexical_language = 'simple'::regconfig
               FROM proxima_core.projection
              WHERE memory_id = $1",
        )
        .bind(simple)
        .fetch_one(pool)
        .await?;
        assert!(
            stamped_simple,
            "the existing row must retain its simple config"
        );
        let stored_the: bool = sqlx::query_scalar(
            "SELECT search_tsv @@ to_tsquery('simple', 'the')
               FROM proxima_core.projection WHERE memory_id = $1",
        )
        .bind(simple)
        .fetch_one(pool)
        .await?;
        assert!(
            stored_the,
            "query analysis must not rewrite stored simple lexemes"
        );

        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "checkpoint"),
                &[note_projection()],
            )
            .await?;
        assert!(
            page.results
                .iter()
                .any(|row| row.memory_id.into_inner() == simple),
            "a simple-config row must remain searchable after the default becomes english"
        );
        let stopword_page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "the"),
                &[note_projection()],
            )
            .await?;
        let no_ranked_terms: bool = sqlx::query_scalar(
            "SELECT numnode(websearch_to_tsquery('simple',
                        proxima_core.lexical_query_text('simple', 'the'))) = 0",
        )
        .fetch_one(pool)
        .await?;
        assert!(
            no_ranked_terms,
            "registered English removes its stopword-only full-text query even for simple rows"
        );
        let stopword_hit = stopword_page
            .results
            .iter()
            .find(|hit| hit.memory_id.into_inner() == simple)
            .expect("the declared substring arm preserves literal stopword matches");
        assert!(
            (stopword_hit.lexical_score - BAND_SUBSTRING.floor).abs() < f32::EPSILON,
            "a literal stopword match has only the declared substring band"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("simple lexical stability check failed");
}

#[tokio::test]
async fn lexical_default_switch_stamps_only_subsequent_core_rows() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));

        let initial_default_is_english: bool =
            sqlx::query_scalar("SELECT proxima_core.lexical_config() = 'english'::regconfig")
                .fetch_one(pool)
                .await?;
        assert!(
            initial_default_is_english,
            "initial default must be english"
        );

        let english = seed_note(pool, owner, "Cats", "the cats sleep on the sofa").await?;
        sqlx::query("SELECT proxima_core.set_lexical_config('german')")
            .execute(pool)
            .await?;
        let german = seed_note(pool, owner, "Tiere", "die Katzen schlafen auf dem Sofa").await?;

        let stamped: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT memory_id, lexical_language::text
               FROM proxima_core.projection
              WHERE memory_id = ANY($1::uuid[])
              ORDER BY memory_id",
        )
        .bind(vec![english, german])
        .fetch_all(pool)
        .await?;
        assert_eq!(
            stamped,
            vec![(english, "english".into()), (german, "german".into())],
            "the switch must stamp only subsequently inserted omitted-language rows"
        );

        let default_is_german: bool =
            sqlx::query_scalar("SELECT proxima_core.lexical_config() = 'german'::regconfig")
                .fetch_one(pool)
                .await?;
        assert!(
            default_is_german,
            "set_lexical_config must change the default"
        );

        let german_is_active: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM proxima_core.lexical_languages
                  WHERE config = 'german'::regconfig
             )",
        )
        .fetch_one(pool)
        .await?;
        assert!(
            german_is_active,
            "the new default must be an active language"
        );
        let english_is_still_active: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM proxima_core.lexical_languages
                  WHERE config = 'english'::regconfig
             )",
        )
        .fetch_one(pool)
        .await?;
        assert!(
            english_is_still_active,
            "switching the prose default must keep pinned-English rows searchable"
        );

        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "Katze"),
                &[note_projection()],
            )
            .await?;
        assert!(
            page.results
                .iter()
                .any(|row| row.memory_id.into_inner() == german),
            "the German-default row must match through the active-language OR"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("lexical default switch checks failed");
}

#[tokio::test]
async fn semantic_search_respects_until() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let t = seed_note(pool, owner, "Embedded", "semantic neighbour body").await?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, dim, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2, $3)",
        )
        .bind(t)
        .bind(embed_vector())
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, dim, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1024, 1, $2)",
        )
        .bind(t)
        .bind(owner.into_inner())
        .execute(pool)
        .await?;

        let mut inside = search_req(owner, "unused");
        inside.mode = SearchMode::Semantic;
        let mut query_vec = vec![0.0; 1024];
        query_vec[0] = 1.0;
        inside.semantic = Some(test_semantic(query_vec));
        let hit = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &inside,
                &[note_projection()],
            )
            .await?;
        assert_eq!(hit.results.len(), 1);
        // Snippets are hydrated from the PAGE now, not carried by the
        // ranked statement, so a row that reached the page on similarity
        // alone gets one too. The sidecar-join shape could not do this:
        // the join lived in the lexical statement, so a semantic-only hit
        // came back with an empty snippet.
        assert!(
            !hit.results[0].snippet.is_empty(),
            "a semantic-only hit is hydrated by the same per-schema lookup"
        );

        let mut too_old = inside.clone();
        too_old.until = Some(time::OffsetDateTime::UNIX_EPOCH);
        let missed = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &too_old,
                &[note_projection()],
            )
            .await?;
        assert!(
            missed.results.is_empty(),
            "ANN hits older than until must not admit"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("semantic until filter failed");
}

/// The semantic scan binds its query vector typed, not as text (#379).
///
/// A prepared statement switches to a generic plan after five executions.
/// There a text bind cast in SQL is not a constant, and `vector_in`
/// re-parses it for every row the scan scores. So the scan's `$3` must
/// reach Postgres as a `vector`, and a generic plan must rank what the
/// custom plan ranked.
#[tokio::test]
async fn semantic_scan_binds_a_typed_query_vector_under_a_generic_plan() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let result: Result<(), Box<dyn std::error::Error>> = async {
        // One connection: the statements the searches prepare are the ones
        // `pg_prepared_statements` lists below.
        let pool_config = PgPoolConfig {
            max_connections: 1,
            ..PgPoolConfig::default()
        };
        let (runtime_url, platform_url) = proxima_pg_testkit::split_role_urls(&db_name).await?;
        let migration = PgMigrationStorage::connect_with_config(
            &platform_url,
            PgPoolConfig::default(),
            PgTuning::default(),
        )
        .await?;
        migration.run_migrations().await?;
        migration.initialize_store_number(1).await?;
        let scope = PgPlatformScope::new(
            sqlx::PgPool::connect(&platform_url).await?,
            &["proxima_core"],
        )
        .await?;
        let pg = PgStorage::connect_with_config(
            &runtime_url,
            pool_config,
            PgTuning::default(),
            scope,
            1,
        )
        .await?
        .with_admin_pool_for_tests(sqlx::PgPool::connect(&db_url(&db_name)).await?);
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let near = seed_note(pool, owner, "Near", "semantic neighbour body").await?;
        seed_embedding(pool, owner, near, &embed_vector()).await?;
        let far = seed_note(pool, owner, "Far", "another neighbour body").await?;
        seed_embedding(pool, owner, far, &embed_vector_xy(0.6, 0.8)).await?;

        let mut req = search_req(owner, "unused");
        req.mode = SearchMode::Semantic;
        req.semantic = Some(test_semantic(embed_vector().to_vec()));
        let ranked = |page: proxima_core::verbs::query::MemorySearchPage| {
            page.results
                .iter()
                .map(|hit| hit.memory_id.into_inner())
                .collect::<Vec<_>>()
        };
        let custom = ranked(
            pg.search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &req,
                &[note_projection()],
            )
            .await?,
        );
        assert_eq!(custom, vec![near, far]);

        let runtime_pool = pg.clone_pool_for_backend();
        sqlx::query("SET plan_cache_mode = force_generic_plan")
            .execute(&runtime_pool)
            .await?;
        let generic = ranked(
            pg.search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &req,
                &[note_projection()],
            )
            .await?,
        );
        assert_eq!(
            generic, custom,
            "a generic plan ranks what the custom plan ranked"
        );

        let runtime_pool = pg.clone_pool_for_backend();
        let (parameter_types, generic_plans): (Vec<String>, i64) = sqlx::query_as(
            "SELECT parameter_types::text[], generic_plans
               FROM pg_prepared_statements
              WHERE statement LIKE '%<=> $3::vector(1024)%'
                AND statement NOT LIKE '%pg_prepared_statements%'",
        )
        .fetch_one(&runtime_pool)
        .await?;
        assert_eq!(
            parameter_types.get(2).map(String::as_str),
            Some("vector"),
            "the scan's query vector is bound typed: {parameter_types:?}"
        );
        assert!(generic_plans >= 1, "the second search ran a generic plan");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("typed semantic bind checks failed");
}

/// The tag filter reaches the SEMANTIC arm's candidate scan.
///
/// Two embedded notes under one owner, one tagged. A tagged `Semantic`
/// search must return the tagged one alone, and so must a tagged `Hybrid`
/// search — the default mode, whose semantic arm used to admit memories
/// carrying none of the requested tags because only the lexical arm applied
/// the predicate. And a tagged request no flavor participates in returns
/// nothing, not the unfiltered scan.
#[tokio::test]
async fn tagged_semantic_search_returns_only_tagged_rows() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let tagged_t = seed_note_lang(
            pool,
            owner,
            "Tagged",
            "semantic neighbour body",
            None,
            &["bucket-0"],
        )
        .await?;
        let plain_t = seed_note(pool, owner, "Plain", "semantic neighbour body").await?;
        for t in [tagged_t, plain_t] {
            sqlx::query(
                "INSERT INTO proxima_core.embeddings
                    (entity_id, model_id, dim, embedding_version, vec, owner_id)
                 VALUES ($1, 'test-embed', 1024, 1, $2, $3)",
            )
            .bind(t)
            .bind(embed_vector())
            .bind(owner.into_inner())
            .execute(pool)
            .await?;
            sqlx::query(
                "INSERT INTO proxima_core.embedding_heads
                    (entity_id, model_id, dim, embedding_version, owner_id)
                 VALUES ($1, 'test-embed', 1024, 1, $2)",
            )
            .bind(t)
            .bind(owner.into_inner())
            .execute(pool)
            .await?;
        }
        let ids = |page: &proxima_core::verbs::query::MemorySearchPage| {
            let mut ids: Vec<Uuid> = page
                .results
                .iter()
                .map(|result| result.memory_id.into_inner())
                .collect();
            ids.sort_unstable();
            ids
        };
        let mut both = vec![tagged_t, plain_t];
        both.sort_unstable();

        // The word is in BOTH bodies, so the hybrid merge below has a
        // lexical arm to merge with; the untagged row must still not
        // reach the page through the semantic arm.
        let mut semantic = search_req(owner, "neighbour");
        semantic.mode = SearchMode::Semantic;
        let mut query_vec = vec![0.0; 1024];
        query_vec[0] = 1.0;
        semantic.semantic = Some(test_semantic(query_vec));
        let untagged = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &semantic,
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            ids(&untagged),
            both,
            "untagged, both embedded rows are semantic candidates"
        );

        semantic.tags = vec!["bucket-0".into()];
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &semantic,
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            ids(&page),
            vec![tagged_t],
            "a tagged semantic search returns only rows carrying the tag"
        );

        let mut hybrid = semantic.clone();
        hybrid.mode = SearchMode::Hybrid;
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &hybrid,
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            ids(&page),
            vec![tagged_t],
            "…and so does hybrid, whose semantic arm used to leak untagged rows"
        );

        let page = pg
            .search_memories(Some(&owner_scope_for_tests(&[owner])), &semantic, &[])
            .await?;
        assert!(
            page.results.is_empty(),
            "a tagged request no flavor participates in returns nothing, \
             not the unfiltered scan"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("tagged semantic search failed");
}

/// Semantic candidates come from one shared embedding table, while the
/// participating flavor set is the projection contract. A foreign flavor
/// must not consume an explicitly core-scoped Semantic/Hybrid window, even
/// when more than that window of foreign rows is closer than the core row.
/// Without that schema restriction, eligible foreign projections participate;
/// tags narrow them. Any/All and an empty participating set remain real
/// candidate-side cases.
#[tokio::test]
async fn semantic_and_hybrid_respect_untagged_flavor_scope() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    split_role_urls_for(&db_name, &["proxima_docs"])
        .await
        .expect("split docs roles");
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        create_docs_search_surface(pool).await?;

        // The core row is deliberately farther from the query vector. The
        // 22 foreign rows therefore fill the old global semantic LIMIT 20;
        // a post-admit schema check cannot recover this core candidate.
        let core_t = seed_note_lang(
            pool,
            owner,
            "Core needle",
            "core needle body",
            None,
            &["core"],
        )
        .await?;
        seed_embedding(pool, owner, core_t, &embed_vector_xy(0.8, 0.6)).await?;

        let mut foreign = Vec::new();
        for index in 0..22 {
            let tags = if index == 0 {
                vec!["foreign", "shared"]
            } else {
                vec!["foreign"]
            };
            let t = seed_docs_note(pool, owner, core_t, "foreign needle body", &tags).await?;
            seed_embedding(pool, owner, t, &embed_vector()).await?;
            foreign.push(t);
        }
        let docs = docs_projection();
        let projections = [note_projection(), docs.clone()];
        let ids = |page: &proxima_core::verbs::query::MemorySearchPage| {
            page.results
                .iter()
                .map(|result| result.memory_id.into_inner())
                .collect::<Vec<_>>()
        };
        let mut query_vec = vec![0.0; 1024];
        query_vec[0] = 1.0;

        let mut semantic = search_req(owner, "needle");
        semantic.kind = None;
        semantic.limit = 1;
        semantic.mode = SearchMode::Semantic;
        semantic.schema_id = Some(note_projection().schema_id);
        semantic.semantic = Some(test_semantic(query_vec.clone()));
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &semantic,
                &projections,
            )
            .await?;
        assert_eq!(
            ids(&page),
            vec![core_t],
            "core-scoped Semantic must filter before its limit, despite 22 closer foreign rows"
        );

        let mut hybrid = semantic.clone();
        hybrid.mode = SearchMode::Hybrid;
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &hybrid,
                &projections,
            )
            .await?;
        assert_eq!(
            ids(&page),
            vec![core_t],
            "core-scoped Hybrid must keep the semantic arm on core projections"
        );

        for mode in [SearchMode::Semantic, SearchMode::Hybrid] {
            let mut unscoped = semantic.clone();
            unscoped.schema_id = None;
            unscoped.limit = 8;
            unscoped.mode = mode;
            let page = pg
                .search_memories(
                    Some(&owner_scope_for_tests(&[owner])),
                    &unscoped,
                    &projections,
                )
                .await?;
            assert!(
                ids(&page).iter().any(|id| foreign.contains(id)),
                "unscoped {mode:?} must reach eligible foreign projections"
            );
            unscoped.schema_id = Some(docs.schema_id.clone());
            let page = pg
                .search_memories(
                    Some(&owner_scope_for_tests(&[owner])),
                    &unscoped,
                    &projections,
                )
                .await?;
            assert!(
                !page.results.is_empty() && ids(&page).iter().all(|id| foreign.contains(id)),
                "schema-scoped {mode:?} reaches only its foreign projection without tags"
            );
        }

        let mut tagged = semantic.clone();
        tagged.schema_id = None;
        tagged.limit = 8;
        tagged.tags = vec!["foreign".into()];
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &tagged,
                &projections,
            )
            .await?;
        assert!(
            !page.results.is_empty()
                && page
                    .results
                    .iter()
                    .all(|result| foreign.contains(&result.memory_id.into_inner())),
            "tagged Semantic must reach matching eligible foreign rows"
        );

        tagged.tag_match = TagMatch::All;
        tagged.tags = vec!["foreign".into(), "shared".into()];
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &tagged,
                &projections,
            )
            .await?;
        assert_eq!(
            ids(&page),
            vec![foreign[0]],
            "tagged All must require every requested foreign tag"
        );

        tagged.tag_match = TagMatch::Any;
        tagged.tags = vec!["foreign".into(), "absent".into()];
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &tagged,
                &projections,
            )
            .await?;
        assert!(
            !page.results.is_empty()
                && page
                    .results
                    .iter()
                    .all(|result| foreign.contains(&result.memory_id.into_inner())),
            "tagged Any must accept a matching foreign tag"
        );

        tagged.tags = vec!["absent".into()];
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &tagged,
                &projections,
            )
            .await?;
        assert!(
            page.results.is_empty(),
            "a tag with no participating projection rows must return empty"
        );

        let mut hybrid = tagged;
        hybrid.tags = vec!["foreign".into()];
        hybrid.mode = SearchMode::Hybrid;
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &hybrid,
                &projections,
            )
            .await?;
        assert!(
            !page.results.is_empty()
                && page
                    .results
                    .iter()
                    .all(|result| foreign.contains(&result.memory_id.into_inner())),
            "tagged Hybrid must reach matching eligible foreign rows"
        );

        let mut no_projection = semantic;
        no_projection.limit = 8;
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &no_projection,
                &[docs],
            )
            .await?;
        assert!(
            page.results.is_empty(),
            "a core schema restriction with only foreign projections must be empty"
        );
        no_projection.schema_id = None;
        let page = pg
            .search_memories(Some(&owner_scope_for_tests(&[owner])), &no_projection, &[])
            .await?;
        assert!(
            page.results.is_empty(),
            "an empty projection set must remain empty"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("semantic and hybrid flavor scope failed");
}

/// `since` is a LOWER bound and `until` is an UPPER one — proved by rows,
/// not by the shape of the string.
///
/// The two binds have the same type and the same cast, so exchanging their
/// comparisons (`>= $5` / `<= $4`) is a mutation only a test that binds a
/// non-null window on the LEXICAL arm can catch — the semantic arm does not
/// render these predicates.
///
/// Two things make this test kill that mutant:
///
/// - The window must be ASYMMETRIC. A symmetric `since == until` asks for
///   `t >= x AND t <= x` either way round and survives the exchange.
/// - **The assertion has to be about SCORES, not ids.** `search_admit_sql`
///   re-applies `since`/`until` on the hit set (`$5`/`$6` there), so a
///   candidate window that admitted the wrong rows still returns the right
///   IDS — admission trims them. What it cannot repair is the arm they came
///   from: an exchanged window empties the RANKED statement, the schema is
///   then reported missing, the substring arm re-finds the same rows, and
///   they come back stamped with the flat `BAND_SUBSTRING` floor instead of
///   their `ts_rank` score. That is the same signature as the head-starvation
///   defect, and it is what the band assertion below reads.
#[tokio::test]
async fn lexical_search_reads_since_as_a_floor_and_until_as_a_ceiling() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));

        // Three notes, strictly increasing in `t` — uuidv7 is millisecond
        // precision, so the sleeps are what make "strictly".
        let mut ts = Vec::new();
        for title in ["Atlas alpha", "Atlas beta", "Atlas gamma"] {
            let t = seed_note(pool, owner, title, "cartography of the archive").await?;
            ts.push(t);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        let stamps: Vec<time::OffsetDateTime> =
            sqlx::query_scalar("SELECT uuid_extract_timestamp(t) FROM unnest($1::uuid[]) AS t")
                .bind(&ts)
                .fetch_all(pool)
                .await?;
        assert!(
            stamps[0] < stamps[1] && stamps[1] < stamps[2],
            "the fixture needs three distinct instants, got {stamps:?}"
        );

        let unbounded = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &search_req(owner, "atlas"),
                &[note_projection()],
            )
            .await?;
        assert_eq!(unbounded.results.len(), 3, "all three match the query");

        let mut windowed = search_req(owner, "atlas");
        windowed.since = Some(stamps[1]);
        windowed.until = Some(stamps[2]);
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &windowed,
                &[note_projection()],
            )
            .await?;
        let found: std::collections::BTreeSet<Uuid> = page
            .results
            .iter()
            .map(|row| row.memory_id.into_inner())
            .collect();
        assert_eq!(
            found,
            [ts[1], ts[2]].into_iter().collect(),
            "[second, third] is what `since = second, until = third` selects"
        );
        for row in &page.results {
            assert!(
                row.score >= BAND_EXACT.floor,
                "the window must not empty the ranked arm and hand these rows \
                 to the substring fallback at its flat floor; got {}",
                row.score
            );
        }

        // …and the other half of the window, so a mutation that drops one
        // predicate entirely cannot pass by returning everything.
        let mut early = search_req(owner, "atlas");
        early.until = Some(stamps[0]);
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &early,
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            page.results
                .iter()
                .map(|row| row.memory_id.into_inner())
                .collect::<Vec<_>>(),
            vec![ts[0]],
            "`until = first` admits the first row and nothing after it"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("lexical time-window direction failed");
}

#[tokio::test]
async fn lexical_language_forget_refuses_while_rows_reference_it() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let t = seed_note_lang(
            pool,
            owner,
            "Tiere",
            "die Katzen schlafen",
            Some("german"),
            &[],
        )
        .await?;

        let err = sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
            .execute(pool)
            .await
            .expect_err("forget must refuse while a row is stamped german");
        assert!(
            err.to_string().contains("still reference"),
            "refusal must say rows still reference it: {err}"
        );
        let registered: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM proxima_core.lexical_languages
                  WHERE config = 'german'::regconfig
             )",
        )
        .fetch_one(pool)
        .await?;
        assert!(registered, "a refused forget must not deregister");

        sqlx::query("DELETE FROM proxima_core.projection WHERE memory_id = $1")
            .bind(t)
            .execute(pool)
            .await?;
        sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
            .execute(pool)
            .await?;
        let gone: bool = sqlx::query_scalar(
            "SELECT NOT EXISTS (
                 SELECT 1 FROM proxima_core.lexical_languages
                  WHERE config = 'german'::regconfig
             )",
        )
        .fetch_one(pool)
        .await?;
        assert!(gone, "forget removes the registration once unreferenced");

        // Not registered any more: forgetting again is a clean no-op.
        sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
            .execute(pool)
            .await?;
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("lexical forget lifecycle failed");
}

#[tokio::test]
async fn lexical_language_forget_refuses_null_and_the_default() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();

        let err = sqlx::query("SELECT proxima_core.lexical_language_forget(NULL)")
            .execute(pool)
            .await
            .expect_err("NULL is refused");
        assert!(err.to_string().contains("must not be null"), "{err}");

        let err = sqlx::query("SELECT proxima_core.lexical_language_forget('english')")
            .execute(pool)
            .await
            .expect_err("the default configuration is refused");
        assert!(
            err.to_string().contains("default lexical configuration"),
            "{err}"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("lexical forget default refusal failed");
}

/// The remember trigger fires BEFORE INSERT, so a single statement stamping a
/// not-yet-registered configuration self-registers it before the FK check.
#[tokio::test]
async fn lexical_remember_trigger_registers_before_the_fk_check() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        seed_note_lang(
            pool,
            owner,
            "Salutation",
            "bonjour le monde",
            Some("french"),
            &[],
        )
        .await?;
        let registered: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                 SELECT 1 FROM proxima_core.lexical_languages
                  WHERE config = 'french'::regconfig
             )",
        )
        .fetch_one(pool)
        .await?;
        assert!(registered, "self-registration must satisfy the FK");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("lexical self-registration failed");
}

/// The FK is the concurrency story: a writer's RI check holds KEY SHARE on
/// the registration row for its whole transaction, so forget blocks while a
/// write in that language is in flight and refuses once it commits — no
/// committed row can reference a forgotten language.
#[tokio::test]
async fn lexical_language_forget_blocks_on_an_in_flight_writer() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner.into_inner())
        .execute(pool)
        .await?;
        sqlx::query("INSERT INTO proxima_core.lexical_languages (config) VALUES ('german')")
            .execute(pool)
            .await?;

        // Writer: uncommitted projection row stamped german — its RI check
        // holds KEY SHARE on the registration row until the transaction ends.
        let memory_id = seed_note(pool, owner, "Tiere", "die Katzen schlafen").await?;
        sqlx::query("DELETE FROM proxima_core.projection WHERE memory_id = $1")
            .bind(memory_id)
            .execute(pool)
            .await?;
        let mut writer = pool.begin().await?;
        sqlx::query(
            "INSERT INTO proxima_core.projection
                (memory_id, schema_id, owner_id, search_tsv, lexical_language)
             VALUES ($1, 'core/agent-note-v1', $2, ''::tsvector, 'german'::regconfig)",
        )
        .bind(memory_id)
        .bind(owner.into_inner())
        .execute(&mut *writer)
        .await?;

        let mut forgetter = pool.acquire().await?;
        sqlx::query("SET lock_timeout = '500ms'")
            .execute(&mut *forgetter)
            .await?;
        let blocked = sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
            .execute(&mut *forgetter)
            .await
            .expect_err("forget must block on the in-flight writer, not slip past it");
        assert!(
            blocked.to_string().contains("lock timeout"),
            "expected a lock wait, got: {blocked}"
        );

        writer.commit().await?;
        sqlx::query("SET lock_timeout = 0")
            .execute(&mut *forgetter)
            .await?;
        let refused = sqlx::query("SELECT proxima_core.lexical_language_forget('german')")
            .execute(&mut *forgetter)
            .await
            .expect_err("after the writer commits, forget refuses on the FK");
        assert!(refused.to_string().contains("still reference"), "{refused}");
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("lexical forget concurrency failed");
}

/// A superseded backlog must not starve the SUBSTRING leg either.
///
/// The substring leg is the arm the collapse gave a head restriction to, and
/// its restriction has a different shape from the ranked arm's — the leg
/// already drives `proxima_core.memory m`, so it probes `memory_head`
/// directly instead of through a second `memory` lookup. That shape was
/// pinned by a string assertion only, and a string assertion cannot see a join
/// that is present but wrong.
///
/// The corpus has to make the overfetch window scarce. With one superseded
/// substring match the test passes with the restriction REMOVED:
/// `search_admit_sql` drops the row either way, so the candidate-side predicate
/// changes nothing until the window is the scarce thing. This corpus makes it
/// scarce — [`SUBSTRING_BACKLOG`] superseded matches against
/// a `limit = 1` window of twenty — and puts the live row FIRST, because the
/// substring arm scores everything at one flat floor and breaks the tie on
/// `t DESC`, so the oldest row is the one a spent window loses.
///
/// `artograph` is inside `cartography` and is not a lexeme of it, so
/// `websearch_to_tsquery` matches nothing, the schema is reported missing,
/// and the substring leg is the only thing that can answer.
#[tokio::test]
async fn a_superseded_backlog_does_not_starve_the_substring_leg() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_core_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let owner_id = owner.into_inner();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner_id)
        .execute(pool)
        .await?;

        // The live row, seeded FIRST so every decoy is newer than it.
        let live = seed_note(pool, owner, "Atlas", "the cartography of the archive").await?;

        for index in 0..SUBSTRING_BACKLOG {
            let handle = Uuid::now_v7();
            let superseded = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
                 VALUES ($1, 'fact', 'core/agent-note-v1', $2, $3)",
            )
            .bind(handle)
            .bind(owner_id)
            .bind(superseded)
            .execute(pool)
            .await?;
            let head = Uuid::now_v7();
            for (t, body) in [
                (superseded, "the cartography of the archive"),
                (head, "this revision says nothing"),
            ] {
                // The stamp and the row it promises land in one transaction.
                let mut stamped = pool.begin().await?;
                sqlx::query(
                    "INSERT INTO proxima_core.memory
                         (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
                     VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1',
                             ARRAY['proxima_core.agent_note_v1'], uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)",
                )
                .bind(handle)
                .bind(t)
                .bind(owner_id)
                .execute(&mut *stamped)
                .await?;
                sqlx::query(
                    "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
                     VALUES ($1, $2, $3, $4, '{}')",
                )
                .bind(t)
                .bind(t)
                .bind(format!("Backlog {index}"))
                .bind(body)
                .execute(&mut *stamped)
                .await?;
                stamped.commit().await?;
                project(pool, t, "core/agent-note-v1", None).await?;
            }
            sqlx::query("UPDATE proxima_core.memory_head SET t = $2 WHERE handle = $1")
                .bind(handle)
                .bind(head)
                .execute(pool)
                .await?;
        }

        let mut req = search_req(owner, "artograph");
        req.limit = 1;
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &req,
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            page.results
                .iter()
                .map(|row| row.memory_id.into_inner())
                .collect::<Vec<_>>(),
            vec![live],
            "the substring window must not be spent on revisions admission \
             will drop"
        );
        assert!(
            (page.results[0].score - BAND_SUBSTRING.floor).abs() < f32::EPSILON,
            "…and the row came through the substring arm, at its flat floor; \
             got {}",
            page.results[0].score
        );

        // The control. If this returns one row, the assertion above passed
        // because the backlog stopped matching, not because the head
        // restriction worked.
        req.supersession = SupersessionStatus::IncludeSuperseded;
        req.limit = 64;
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &req,
                &[note_projection()],
            )
            .await?;
        assert_eq!(
            page.results.len(),
            SUBSTRING_BACKLOG + 1,
            "IncludeSuperseded wants the revisions, and every one of them \
             carries the substring"
        );
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("substring starvation check failed");
}
