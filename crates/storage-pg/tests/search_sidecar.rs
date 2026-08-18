//! Sidecar-first core search: GIN on the sidecar, admit via memory_head.
#![allow(clippy::doc_markdown, clippy::too_many_lines)]

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

fn note_projection() -> MemorySearchProjection {
    MemorySearchProjection {
        schema_id: SchemaId::new("core/agent-note-v1".to_string()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Fact,
        sidecar_table: "proxima_core.agent_note_v1".into(),
        fields: vec![
            MemorySearchProjectionField {
                column: "title".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "body".into(),
                kind: SearchProjectionColumnKind::Text,
            },
        ],
        tag_column: Some("tags".into()),
        tsv_column: Some("search_tsv".into()),
        embed_text_column: Some("embed_text".into()),
        language_column: Some("lexical_language".into()),
    }
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

async fn seed_note(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    title: &str,
    body: &str,
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
    Ok(t)
}

async fn seed_note_lang(
    pool: &sqlx::PgPool,
    owner: OwnerRef,
    title: &str,
    body: &str,
    language: &str,
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
        "INSERT INTO proxima_core.agent_note_v1
            (t, note_id, title, body, tags, lexical_language)
         VALUES ($1, $2, $3, $4, '{}', $5::regconfig)",
    )
    .bind(t)
    .bind(Uuid::now_v7())
    .bind(title)
    .bind(body)
    .bind(language)
    .execute(pool)
    .await?;
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

        let like_only = pg
            .search_memories(&search_req(owner, "eedle"), &[note_projection()])
            .await?;
        assert_eq!(
            like_only.results.len(),
            1,
            "substring with no tsvector lexeme must still hit via LIKE fallback"
        );
        assert_eq!(like_only.results[0].memory_id.into_inner(), ours);
        assert!(
            (like_only.results[0].lexical_score - 0.25).abs() < f32::EPSILON,
            "LIKE fallback score is the 0.25 substring band, got {}",
            like_only.results[0].lexical_score
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

        let projection = MemorySearchProjection {
            schema_id: SchemaId::new("proxima-docs/section-text-v1".to_string()),
            schema_version: SchemaVersion::new(1),
            kind: PayloadKind::Abstraction,
            sidecar_table: "proxima_docs.section_text_v1".into(),
            fields: vec![MemorySearchProjectionField {
                column: "text".into(),
                kind: SearchProjectionColumnKind::Text,
            }],
            tag_column: Some("tags".into()),
            tsv_column: None,
            embed_text_column: Some("text".into()),
            language_column: None,
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
            "german",
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
