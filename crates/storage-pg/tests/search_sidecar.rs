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
        "INSERT INTO proxima_core.memory (handle, t, kind, owner_id)
         VALUES ($1, $2, 'fact', $3)",
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
            "flavor sidecars are not in core_search_memories"
        );

        Ok(())
    }
    .await;

    let _ = drop_db(&db_name).await;
    result.expect("sidecar-first search test failed");
}
