//! `PostgreSQL` behavior tests for structured tag enumeration (issue #662).

use proxiima_core::read_models::MemorySchemaSpec;
use proxiima_core::storage_ports::MemoryReadPort;
use proxiima_core::test_fixtures::owner_scope_for_tests;
use proxiima_core::verbs::query::{EntityKind, QueryRequest, SupersessionStatus, TagMatch};
use proxiima_core::{OwnerId, SchemaId, SchemaVersion};
use proxiima_pg_testkit::{db_url, drop_db};
use proxiima_storage_pg::PgStorage;
use proxiima_storage_pg::test_fixtures::create_core_db;
use sqlx::{PgPool, Row};
use uuid::Uuid;

fn note_spec() -> MemorySchemaSpec {
    MemorySchemaSpec::new(
        EntityKind::Fact,
        SchemaId::new("core/agent-note-v1".into()),
        SchemaVersion::new(1),
        Some("proxiima_core.agent_note_v1".into()),
    )
    .with_display_fields(None, Some("tags".into()))
}

async fn seed(pool: &PgPool, owner: OwnerId, other: OwnerId) -> Result<(), sqlx::Error> {
    let owner_id = owner.into_inner();
    let other_id = other.into_inner();
    sqlx::query(
        "INSERT INTO proxiima_core.owners(owner_id, kind)
         VALUES ($1, 'personal'), ($2, 'personal')",
    )
    .bind(owner_id)
    .bind(other_id)
    .execute(pool)
    .await?;
    let mut stamped = pool.begin().await?;
    // The first rows in descending t order are untagged; matches are older.
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head(handle, t, kind, owner_id, schema_id)
         SELECT uuidv7(), uuidv7(), 'fact', $1, 'core/agent-note-v1'
           FROM generate_series(1, 12)",
    )
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.memory_head(handle, t, kind, owner_id, schema_id)
         VALUES (uuidv7(), uuidv7(), 'fact', $1, 'core/agent-note-v1')",
    )
    .bind(other_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.memory
             (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
         SELECT handle, t, 'fact', owner_id, schema_id,
                ARRAY['proxiima_core.agent_note_v1'], uuid_extract_timestamp(t),
                '00000000-0000-0000-0000-000000000509'::uuid, NULL
           FROM proxiima_core.memory_head",
    )
    .execute(&mut *stamped)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.agent_note_v1(t, note_id, title, body, tags)
         SELECT m.t, uuidv7(), 'tag test', 'tag test',
                CASE WHEN m.owner_id <> $1
                          OR row_number() OVER (PARTITION BY m.owner_id ORDER BY m.t) IN (1, 2)
                     THEN ARRAY['rust']::text[]
                     WHEN row_number() OVER (PARTITION BY m.owner_id ORDER BY m.t) = 3
                     THEN ARRAY['rust', 'storage']::text[]
                     ELSE '{}'::text[] END
           FROM proxiima_core.memory m
          WHERE m.schema_id = 'core/agent-note-v1'",
    )
    .bind(owner_id)
    .execute(&mut *stamped)
    .await?;
    let current = sqlx::query(
        "SELECT handle FROM proxiima_core.memory_head
          WHERE owner_id = $1 ORDER BY t DESC LIMIT 1",
    )
    .bind(owner_id)
    .fetch_one(&mut *stamped)
    .await?;
    let handle: Uuid = current.try_get("handle")?;
    let old = sqlx::query(
        "INSERT INTO proxiima_core.memory
             (handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via)
         VALUES ($1, uuidv7(), 'fact', $2, 'core/agent-note-v1',
                 ARRAY['proxiima_core.agent_note_v1'], now(), '00000000-0000-0000-0000-000000000509'::uuid, NULL)
         RETURNING t",
    )
    .bind(handle)
    .bind(owner_id)
    .fetch_one(&mut *stamped)
    .await?;
    let old_t: Uuid = old.try_get("t")?;
    sqlx::query(
        "INSERT INTO proxiima_core.agent_note_v1(t, note_id, title, body, tags)
         VALUES ($1, uuidv7(), 'old', 'old', ARRAY['rust']::text[])",
    )
    .bind(old_t)
    .execute(&mut *stamped)
    .await?;
    stamped.commit().await?;
    sqlx::raw_sql(sqlx::AssertSqlSafe(
        "ANALYZE proxiima_core.memory_head; ANALYZE proxiima_core.memory; ANALYZE proxiima_core.agent_note_v1;"
            .to_owned(),
    ))
    .execute(pool)
    .await?;
    Ok(())
}

async fn all_ids(
    pg: &PgStorage,
    owner: OwnerId,
    mut req: QueryRequest,
    include_payloads: bool,
) -> Result<(Vec<Uuid>, usize), proxiima_core::StorageError> {
    req.include_payloads = include_payloads;
    req.limit = 1;
    let scope = owner_scope_for_tests(&[owner]);
    let specs = [note_spec()];
    let mut ids = Vec::new();
    let mut pages = 0;
    loop {
        let page = pg
            .query_memories(Some(&scope), &[owner], &req, &specs)
            .await?;
        pages += 1;
        ids.extend(page.memories.iter().map(|row| row.id.into_inner()));
        let Some(cursor) = page.next_cursor else {
            break;
        };
        req.page.after = Some(cursor);
    }
    Ok((ids, pages))
}

async fn expected_ids(
    pool: &PgPool,
    owner: OwnerId,
    tags: &[&str],
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT m.t
           FROM proxiima_core.memory m
           JOIN proxiima_core.memory_head h ON h.t = m.t AND h.handle = m.handle
           JOIN proxiima_core.agent_note_v1 n ON n.t = m.t
          WHERE m.owner_id = $1 AND m.schema_id = 'core/agent-note-v1'
            AND n.tags && $2::text[]
          ORDER BY m.t DESC",
    )
    .bind(owner.into_inner())
    .bind(tags)
    .fetch_all(pool)
    .await
}

fn request(tags: &[&str], match_mode: TagMatch) -> QueryRequest {
    let mut req = QueryRequest::readable();
    req.entity_kind = Some(EntityKind::Fact);
    req.schema_id = Some(SchemaId::new("core/agent-note-v1".into()));
    req.tags = tags.iter().map(|tag| (*tag).to_owned()).collect();
    req.tag_match = match_mode;
    req
}

#[tokio::test]
async fn tags_filter_before_limit_and_cursor_with_any_all_and_owner_scope() {
    let db_name = format!("proxiima_query_tags_{}", Uuid::now_v7().simple());
    create_core_db(&db_name).await.expect("owned PG required");
    let url = db_url(&db_name);
    let result = async {
        let owner = OwnerId::new(Uuid::now_v7());
        let other = OwnerId::new(Uuid::now_v7());
        let admin = PgPool::connect(&url).await?;
        seed(&admin, owner, other).await?;
        let pg = PgStorage::connect_for_tests(url.clone()).await?;

        let (any_ids, pages) =
            all_ids(&pg, owner, request(&["rust"], TagMatch::Any), false).await?;
        assert_eq!(any_ids.len(), 3, "all three rust rows survive paging");
        assert_eq!(
            pages, 3,
            "three one-row pages continue without a skipped match"
        );
        assert_eq!(any_ids, expected_ids(&admin, owner, &["rust"]).await?);
        let (all_current, _) = all_ids(&pg, owner, request(&[], TagMatch::Any), false).await?;
        assert_eq!(
            all_current.len(),
            12,
            "empty tags retain unfiltered current enumeration"
        );
        assert_eq!(
            any_ids.len(),
            any_ids
                .iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            "cursor does not repeat a match"
        );

        let (both_ids, _) = all_ids(
            &pg,
            owner,
            request(&["rust", "storage"], TagMatch::All),
            false,
        )
        .await?;
        assert_eq!(both_ids.len(), 1, "all requires both tags");
        let (none_ids, pages) =
            all_ids(&pg, owner, request(&["missing"], TagMatch::Any), false).await?;
        assert!(none_ids.is_empty());
        assert_eq!(pages, 1, "no matches need one page");

        let mut hydrated_request = request(&["rust"], TagMatch::Any);
        hydrated_request.limit = 20;
        let hydrated = pg
            .query_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &[owner],
                &hydrated_request,
                &[note_spec()],
            )
            .await?;
        assert!(hydrated.memories.iter().all(|row| row.payload.is_some()));
        let mut payload_free_request = hydrated_request.clone();
        payload_free_request.include_payloads = false;
        let payload_free = pg
            .query_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &[owner],
                &payload_free_request,
                &[note_spec()],
            )
            .await?;
        assert_eq!(
            hydrated
                .memories
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            payload_free
                .memories
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            "filtering does not require payload hydration"
        );
        assert!(
            payload_free
                .memories
                .iter()
                .all(|row| row.payload.is_none())
        );

        let mut history = request(&["rust"], TagMatch::Any);
        history.supersession = SupersessionStatus::IncludeSuperseded;
        let (history_ids, _) = all_ids(&pg, owner, history, false).await?;
        assert_eq!(
            history_ids.len(),
            4,
            "history includes the older tagged revision"
        );
        assert!(any_ids.iter().all(|id| history_ids.contains(id)));
        assert!(history_ids.iter().any(|id| !any_ids.contains(id)));
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("tag query regression test failed");
}

#[tokio::test]
async fn tags_without_a_declared_schema_tag_column_match_nothing() {
    let db_name = format!("proxiima_query_tags_empty_{}", Uuid::now_v7().simple());
    create_core_db(&db_name).await.expect("owned PG required");
    let url = db_url(&db_name);
    let result = async {
        let owner = OwnerId::new(Uuid::now_v7());
        let admin = PgPool::connect(&url).await?;
        seed(&admin, owner, OwnerId::new(Uuid::now_v7())).await?;
        let pg = PgStorage::connect_for_tests(url.clone()).await?;
        let mut req = request(&["rust"], TagMatch::Any);
        let no_tag_spec = MemorySchemaSpec::new(
            EntityKind::Fact,
            SchemaId::new("core/agent-note-v1".into()),
            SchemaVersion::new(1),
            Some("proxiima_core.agent_note_v1".into()),
        );
        let scope = owner_scope_for_tests(&[owner]);
        let tagged = pg
            .query_memories(
                Some(&scope),
                &[owner],
                &req,
                std::slice::from_ref(&no_tag_spec),
            )
            .await?;
        assert!(
            tagged.memories.is_empty(),
            "undeclared tag fields cannot match"
        );
        req.tags.clear();
        let unfiltered = pg
            .query_memories(Some(&scope), &[owner], &req, &[no_tag_spec])
            .await?;
        assert!(
            !unfiltered.memories.is_empty(),
            "the fixture contains real rows"
        );
        Ok::<(), Box<dyn std::error::Error>>(())
    }
    .await;
    let _ = drop_db(&db_name).await;
    result.expect("no-tag schema test failed");
}
