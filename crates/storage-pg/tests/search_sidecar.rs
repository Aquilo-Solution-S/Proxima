//! Projection-first core search: one ranked statement per flavor over
//! `<flavor>.projection`, a DECLARED substring arm over the schemas it
//! returned nothing for, then admit via `memory_head`.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

use proxima_core::flavor::{
    Band, BandComparability, LanguagePolicy, RankSource, SubstringArm, WEIGHT_UNIFORM,
};
use proxima_core::flavor0::{BAND_EXACT, BAND_RESCUE, BAND_SUBSTRING};
use proxima_core::storage_ports::MemoryReadPort;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
};
use proxima_core::{OwnerRef, SchemaId, SchemaVersion, SearchProjectionColumnKind, UserId};
use proxima_pg_testkit::{create_db, db_url, drop_db};
use proxima_storage_pg::PgStorage;
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

fn search_req(owner: OwnerRef, query: &str) -> MemorySearchRequest {
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
        query_embedding: None,
        embedding_model_id: None,
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
    owner: OwnerRef,
    title: &str,
    body: &str,
) -> Result<Uuid, sqlx::Error> {
    seed_note_lang(pool, owner, title, body, None).await
}

async fn seed_note_lang(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    title: &str,
    body: &str,
    language: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let owner_id = owner.stored_owner_id();
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
    sqlx::query(
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
         VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1')",
    )
    .bind(handle)
    .bind(t)
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
         VALUES ($1, $2, $3, $4, '{}')",
    )
    .bind(t)
    .bind(Uuid::now_v7())
    .bind(title)
    .bind(body)
    .execute(pool)
    .await?;
    project(pool, t, "core/agent-note-v1", language).await?;
    Ok(t)
}

fn embed_literal() -> String {
    format!(
        "[{}]",
        std::iter::once("1")
            .chain(std::iter::repeat_n("0", 1023))
            .collect::<Vec<_>>()
            .join(",")
    )
}

#[tokio::test]
async fn lexical_search_is_sidecar_first_then_owner_admit() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();

        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
            .search_memories(&search_req(owner, "keyword needle"), &[note_projection()])
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
            .search_memories(&search_req(owner, "eedle"), &[note_projection()])
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
            .search_memories(&search_req(owner, "eedle"), &[arm_off])
            .await?;
        assert!(
            refused.results.is_empty(),
            "a schema declaring SubstringArm::Off contributes no statement and no rows"
        );

        let miss = pg
            .search_memories(
                &search_req(owner, "no-such-lexeme-xyzzy"),
                &[note_projection()],
            )
            .await?;
        assert!(miss.results.is_empty());

        let flavor = MemorySearchProjection {
            sidecar_table: "proxima_code.code_chunk_v1".into(),
            ..note_projection()
        };
        let skipped = pg
            .search_memories(&search_req(owner, "keyword needle"), &[flavor])
            .await?;
        assert!(
            skipped.results.is_empty(),
            "unscoped core_search_memories does not scan flavor sidecars"
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let other = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
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
        let page = pg.search_memories(&req, &[note_projection()]).await?;
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let owner_id = owner.stored_owner_id();
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal')
             ON CONFLICT DO NOTHING",
        )
        .bind(owner_id)
        .execute(pool)
        .await?;
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
        // A flavor's projection lives in the flavor's own schema. This is
        // the generator's shape, hand-built here because the synthetic
        // `proxima-docs` flavor has no contract to run the generator over.
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
        let fact_handle = Uuid::now_v7();
        let fact_t = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO proxima_core.memory_head (handle, kind, schema_id, owner_id, t)
             VALUES ($1, 'fact', 'core/test-fact-v1', $2, $3)",
        )
        .bind(fact_handle)
        .bind(owner_id)
        .bind(fact_t)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
             VALUES ($1, $2, 'fact', $3, 'core/test-fact-v1')",
        )
        .bind(fact_handle)
        .bind(fact_t)
        .bind(owner_id)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.memory
                (handle, t, kind, owner_id, schema_id, origins, content_id)
             VALUES ($1, $2, 'abstraction', $3, 'proxima-docs/section-text-v1', ARRAY[$4]::uuid[], $5)",
        )
        .bind(handle)
        .bind(t)
        .bind(owner_id)
        .bind(fact_t)
        .bind(content_id)
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_docs.section_text_v1 (t, text, tags)
             VALUES ($1, 'Antriebswelle im Getriebe', ARRAY['proxima-docs'])",
        )
        .bind(t)
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

        let projection = MemorySearchProjection {
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
            // A hand-built out-of-tree flavor: it has to declare what the
            // renderer resolves, which is the point of the freeze rule that
            // holds a real one to the same three names.
            bands: DOCS_BANDS,
            substring: SubstringArm::MemoryFirstNestedLoop,
            overfetch_k: 1_000,
            // Both gates, satisfied. Flip either and the tagged search below
            // returns nothing.
            band_comparability: BandComparability::CoreBands,
            rank_source: RankSource::Projection,
        };

        let unscoped = pg
            .search_memories(
                &search_req(owner, "Antriebswelle"),
                std::slice::from_ref(&projection),
            )
            .await?;
        assert!(
            unscoped.results.is_empty(),
            "unscoped search must not open a flavor table"
        );

        let mut tagged = search_req(owner, "Antriebswelle");
        tagged.kind = Some(EntityKind::Abstraction);
        tagged.tags = vec!["proxima-docs".into()];
        let page = pg.search_memories(&tagged, &[projection]).await?;
        assert_eq!(page.results.len(), 1, "tagged search must hit flavor text");
        assert_eq!(page.results[0].memory_id.into_inner(), t);
        Ok(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("tagged flavor sidecar search failed");
}

#[tokio::test]
async fn lexical_search_matches_german_via_lexical_languages() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let t = seed_note_lang(
            pool,
            owner,
            "Tiere",
            "die Katzen schlafen auf dem Sofa",
            Some("german"),
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
            .search_memories(&search_req(owner, "Katze"), &[note_projection()])
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
async fn simple_rows_retain_stopwords_after_default_switch() {
    let db_name = format!("proxima_test_{}", Uuid::now_v7().simple());
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));

        sqlx::query("SELECT proxima_core.set_lexical_config('simple')")
            .execute(pool)
            .await?;
        let simple = seed_note(pool, owner, "Stopword", "the").await?;
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

        let page = pg
            .search_memories(&search_req(owner, "the"), &[note_projection()])
            .await?;
        assert!(
            page.results
                .iter()
                .any(|row| row.memory_id.into_inner() == simple),
            "a simple-config row must retain stopword matches after the default becomes english"
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));

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
            .search_memories(&search_req(owner, "Katze"), &[note_projection()])
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let t = seed_note(pool, owner, "Embedded", "semantic neighbour body").await?;
        sqlx::query(
            "INSERT INTO proxima_core.embeddings
                (entity_id, model_id, embedding_version, vec, owner_id)
             VALUES ($1, 'test-embed', 1, $2::vector, $3)",
        )
        .bind(t)
        .bind(embed_literal())
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        sqlx::query(
            "INSERT INTO proxima_core.embedding_heads
                (entity_id, model_id, embedding_version, owner_id)
             VALUES ($1, 'test-embed', 1, $2)",
        )
        .bind(t)
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;

        let mut inside = search_req(owner, "unused");
        inside.mode = SearchMode::Semantic;
        let mut query_vec = vec![0.0; 1024];
        query_vec[0] = 1.0;
        inside.query_embedding = Some(query_vec);
        inside.embedding_model_id = Some("test-embed".into());
        let hit = pg.search_memories(&inside, &[note_projection()]).await?;
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
        let missed = pg.search_memories(&too_old, &[note_projection()]).await?;
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));

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
            .search_memories(&search_req(owner, "atlas"), &[note_projection()])
            .await?;
        assert_eq!(unbounded.results.len(), 3, "all three match the query");

        let mut windowed = search_req(owner, "atlas");
        windowed.since = Some(stamps[1]);
        windowed.until = Some(stamps[2]);
        let page = pg.search_memories(&windowed, &[note_projection()]).await?;
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
        let page = pg.search_memories(&early, &[note_projection()]).await?;
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let t = seed_note_lang(pool, owner, "Tiere", "die Katzen schlafen", Some("german")).await?;

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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner.stored_owner_id())
        .execute(pool)
        .await?;
        seed_note_lang(
            pool,
            owner,
            "Salutation",
            "bonjour le monde",
            Some("french"),
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        sqlx::query(
            "INSERT INTO proxima_core.owners (owner_id, kind)
             VALUES ($1, 'personal') ON CONFLICT DO NOTHING",
        )
        .bind(owner.stored_owner_id())
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
        .bind(owner.stored_owner_id())
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
    if let Err(e) = create_db(&db_name).await {
        panic!("PG required for tests but admin connect failed: {e}");
    }
    let url = db_url(&db_name);
    let result: Result<(), Box<dyn std::error::Error>> = async {
        let pg = PgStorage::connect(&url).await?;
        pg.run_migrations().await?;
        let pool = pg.pool_for_tests();
        let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
        let owner_id = owner.stored_owner_id();
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
                sqlx::query(
                    "INSERT INTO proxima_core.memory (handle, t, kind, owner_id, schema_id)
                     VALUES ($1, $2, 'fact', $3, 'core/agent-note-v1')",
                )
                .bind(handle)
                .bind(t)
                .bind(owner_id)
                .execute(pool)
                .await?;
                sqlx::query(
                    "INSERT INTO proxima_core.agent_note_v1 (t, note_id, title, body, tags)
                     VALUES ($1, $2, $3, $4, '{}')",
                )
                .bind(t)
                .bind(t)
                .bind(format!("Backlog {index}"))
                .bind(body)
                .execute(pool)
                .await?;
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
        let page = pg.search_memories(&req, &[note_projection()]).await?;
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
        let page = pg.search_memories(&req, &[note_projection()]).await?;
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
