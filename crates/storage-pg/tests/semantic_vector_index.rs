//! Semantic retrieval with an independent in-memory index and real owner RLS.

use std::sync::{Arc, Mutex};

use proxiima_core::storage_ports::{
    CandidateBound, InMemoryVectorIndex, MemoryReadPort, VectorEntry, VectorIndex, VectorKey,
};
use proxiima_core::test_fixtures::owner_scope_for_tests;
use proxiima_core::verbs::query::{
    EntityKind, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxiima_core::{
    EmbeddableEntityRef, EmbeddingDim, EmbeddingSpace, MemoryId, OwnerId, Role, SchemaId,
    SpaceVector, StorageError, UserId,
};
use proxiima_storage_pg::test_fixtures::fresh_pg_host_index;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The reference index, recording each window the store asks for.
#[derive(Default)]
struct RecordingIndex {
    inner: InMemoryVectorIndex,
    windows: Mutex<Vec<u32>>,
    reported_bound: Option<CandidateBound>,
}

#[async_trait::async_trait]
impl VectorIndex for RecordingIndex {
    async fn put(
        &self,
        entity: EmbeddableEntityRef,
        owner: OwnerId,
        version: i32,
        vectors: &[SpaceVector],
    ) -> Result<(), StorageError> {
        self.inner.put(entity, owner, version, vectors).await
    }

    async fn delete(&self, keys: &[VectorKey]) -> Result<(), StorageError> {
        self.inner.delete(keys).await
    }

    async fn set_owner(&self, keys: &[VectorKey], owner: OwnerId) -> Result<(), StorageError> {
        self.inner.set_owner(keys, owner).await
    }

    async fn nearest(
        &self,
        filter: &proxiima_core::storage_ports::VectorCandidateFilter,
        query: &SpaceVector,
        window: u32,
    ) -> Result<proxiima_core::storage_ports::VectorCandidatePage, StorageError> {
        self.windows.lock().unwrap().push(window);
        let mut page = self.inner.nearest(filter, query, window).await?;
        if let Some(bound) = self.reported_bound {
            page.bound = bound;
        }
        Ok(page)
    }

    async fn entries(
        &self,
        after: Option<&VectorKey>,
        limit: u32,
    ) -> Result<Vec<VectorEntry>, StorageError> {
        self.inner.entries(after, limit).await
    }
}

fn vector(score: f32) -> SpaceVector {
    let mut values = vec![0.0; EmbeddingDim::D384.width()];
    values[0] = score;
    values[1] = (1.0 - score * score).sqrt();
    SpaceVector::new(
        EmbeddingSpace::new("memory-index", EmbeddingDim::D384),
        values,
    )
    .unwrap()
}

fn request(owner: OwnerId) -> MemorySearchRequest {
    MemorySearchRequest {
        scope: proxiima_core::verbs::query::MemorySearchScope::Owner(owner),
        read_owners: vec![owner],
        query: "unrelated lexical text".into(),
        mode: SearchMode::Semantic,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 1,
        kind: Some(EntityKind::Fact),
        schema_id: None,
        tags: vec!["wanted".into()],
        tag_match: TagMatch::All,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        min_score: None,
        semantic_weight: None,
        after: None,
        semantic: Some(vector(1.0)),
    }
}

fn readable_request(source_owner: OwnerId) -> MemorySearchRequest {
    MemorySearchRequest {
        scope: proxiima_core::verbs::query::MemorySearchScope::Readable,
        read_owners: vec![source_owner],
        query: "unrelated lexical text".into(),
        mode: SearchMode::Semantic,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 1,
        kind: Some(EntityKind::Fact),
        schema_id: Some(SchemaId::new("core/agent-note-v1".into())),
        tags: vec!["wanted".into()],
        tag_match: TagMatch::All,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        min_score: None,
        semantic_weight: None,
        after: None,
        semantic: Some(vector(1.0)),
    }
}

fn readable_perspective_request(source_owner: OwnerId) -> MemorySearchRequest {
    MemorySearchRequest {
        scope: proxiima_core::verbs::query::MemorySearchScope::Readable,
        read_owners: vec![source_owner],
        query: "unrelated lexical text".into(),
        mode: SearchMode::Semantic,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 1,
        kind: Some(EntityKind::Perspective),
        schema_id: Some(SchemaId::new("core/agent-derivation-v1".into())),
        tags: vec!["wanted".into()],
        tag_match: TagMatch::All,
        since: None,
        until: None,
        order: SearchOrder::Relevance,
        min_score: None,
        semantic_weight: None,
        after: None,
        semantic: Some(vector(1.0)),
    }
}

async fn seed(pool: &PgPool, owner: OwnerId, schema: &str, tag: &str) -> Result<Uuid, sqlx::Error> {
    sqlx::query("INSERT INTO proxiima_core.owners(owner_id, kind) VALUES ($1, 'personal') ON CONFLICT DO NOTHING")
        .bind(owner.into_inner()).execute(pool).await?;
    let t = Uuid::now_v7();
    let handle = Uuid::now_v7();
    sqlx::query("INSERT INTO proxiima_core.memory_head(handle, t, kind, owner_id, schema_id) VALUES ($1, $2, 'fact', $3, $4)")
        .bind(handle).bind(t).bind(owner.into_inner()).bind(schema).execute(pool).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO proxiima_core.memory(handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via) VALUES ($1, $2, 'fact', $3, $4, $5, uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)")
        .bind(handle).bind(t).bind(owner.into_inner()).bind(schema)
        .bind(if schema == "core/agent-note-v1" { vec!["proxiima_core.agent_note_v1"] } else { Vec::new() })
        .execute(&mut *tx).await?;
    if schema == "core/agent-note-v1" {
        sqlx::query("INSERT INTO proxiima_core.agent_note_v1(t, note_id, title, body, tags) VALUES ($1, $2, 'Index result', 'Hydrated from memory', ARRAY[$3])")
            .bind(t).bind(Uuid::now_v7()).bind(tag).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO proxiima_core.projection(memory_id, schema_id, owner_id, search_tsv, tag) VALUES ($1, $2, $3, ''::tsvector, ARRAY[$4])")
        .bind(t).bind(schema).bind(owner.into_inner()).bind(tag).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(t)
}

async fn seed_perspective(
    pool: &PgPool,
    owner: OwnerId,
    tag: &str,
    origin: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let schema = "core/agent-derivation-v1";
    sqlx::query("INSERT INTO proxiima_core.owners(owner_id, kind) VALUES ($1, 'personal') ON CONFLICT DO NOTHING")
        .bind(owner.into_inner()).execute(pool).await?;
    let t = Uuid::now_v7();
    let handle = Uuid::now_v7();
    sqlx::query("INSERT INTO proxiima_core.memory_head(handle, t, kind, owner_id, schema_id) VALUES ($1, $2, 'perspective', $3, $4)")
        .bind(handle).bind(t).bind(owner.into_inner()).bind(schema).execute(pool).await?;
    let mut tx = pool.begin().await?;
    let content_id = Uuid::now_v7();
    let mut content_hash = Vec::with_capacity(32);
    content_hash.extend_from_slice(content_id.as_bytes());
    content_hash.extend_from_slice(content_id.as_bytes());
    sqlx::query("INSERT INTO proxiima_core.content(content_id, owner_id, schema_id, content_hash) VALUES ($1, $2, $3, $4)")
        .bind(content_id).bind(owner.into_inner()).bind(schema).bind(content_hash)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO proxiima_core.memory(handle, t, kind, owner_id, schema_id, content_id, origins, sidecar_tables, occurred_at, \"by\", via) VALUES ($1, $2, 'perspective', $3, $4, $5, ARRAY[$6]::uuid[], $7, NULL, '00000000-0000-0000-0000-000000000509'::uuid, NULL)")
        .bind(handle).bind(t).bind(owner.into_inner()).bind(schema)
        .bind(content_id).bind(origin)
        .bind(vec!["proxiima_core.agent_derivation_v1"])
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO proxiima_core.agent_derivation_v1(t, title, body, tags, source_memory_ids, model_id, client_name, client_version) VALUES ($1, 'Published perspective', 'Hydrated perspective', ARRAY[$2], '{}', 'test', 'test', '1')")
        .bind(t).bind(tag).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO proxiima_core.projection(memory_id, schema_id, owner_id, search_tsv, tag) VALUES ($1, $2, $3, ''::tsvector, ARRAY[$4])")
        .bind(t).bind(schema).bind(owner.into_inner()).bind(tag).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(t)
}

async fn seed_abstraction(
    pool: &PgPool,
    owner: OwnerId,
    tag: &str,
    origin: Uuid,
) -> Result<Uuid, sqlx::Error> {
    let schema = "core/agent-derivation-v1";
    let t = Uuid::now_v7();
    let handle = Uuid::now_v7();
    sqlx::query("INSERT INTO proxiima_core.memory_head(handle, t, kind, owner_id, schema_id) VALUES ($1, $2, 'abstraction', $3, $4)")
        .bind(handle).bind(t).bind(owner.into_inner()).bind(schema).execute(pool).await?;
    let mut tx = pool.begin().await?;
    let content_id = Uuid::now_v7();
    let mut content_hash = Vec::with_capacity(32);
    content_hash.extend_from_slice(content_id.as_bytes());
    content_hash.extend_from_slice(content_id.as_bytes());
    sqlx::query("INSERT INTO proxiima_core.content(content_id, owner_id, schema_id, content_hash) VALUES ($1, $2, $3, $4)")
        .bind(content_id).bind(owner.into_inner()).bind(schema).bind(content_hash)
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO proxiima_core.memory(handle, t, kind, owner_id, schema_id, content_id, origins, sidecar_tables, occurred_at, \"by\", via) VALUES ($1, $2, 'abstraction', $3, $4, $5, ARRAY[$6]::uuid[], $7, NULL, '00000000-0000-0000-0000-000000000509'::uuid, NULL)")
        .bind(handle).bind(t).bind(owner.into_inner()).bind(schema)
        .bind(content_id).bind(origin)
        .bind(vec!["proxiima_core.agent_derivation_v1"])
        .execute(&mut *tx).await?;
    sqlx::query("INSERT INTO proxiima_core.agent_derivation_v1(t, title, body, tags, source_memory_ids, model_id, client_name, client_version) VALUES ($1, 'Grounding abstraction', 'Grounding abstraction', ARRAY[$2], '{}', 'test', 'test', '1')")
        .bind(t).bind(tag).execute(&mut *tx).await?;
    sqlx::query("INSERT INTO proxiima_core.projection(memory_id, schema_id, owner_id, search_tsv, tag) VALUES ($1, $2, $3, ''::tsvector, ARRAY[$4])")
        .bind(t).bind(schema).bind(owner.into_inner()).bind(tag).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(t)
}

#[tokio::test]
async fn semantic_port_expands_past_stale_foreign_and_filtered_candidates() -> TestResult {
    let index = Arc::new(RecordingIndex::default());
    let (pg, _database) = fresh_pg_host_index("semantic_memory_index", index.clone()).await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let other = OwnerId::from(UserId::new(Uuid::now_v7()));
    let visible = seed(pg.pool_for_tests(), owner, "core/agent-note-v1", "wanted").await?;
    let wrong_tag = seed(pg.pool_for_tests(), owner, "core/agent-note-v1", "other").await?;
    let wrong_schema = seed(pg.pool_for_tests(), owner, "semantic/other-v1", "wanted").await?;
    let foreign = seed(pg.pool_for_tests(), other, "core/agent-note-v1", "wanted").await?;
    let entity = |id| EmbeddableEntityRef::Memory {
        kind: EntityKind::Fact,
        memory_id: MemoryId::new(id),
    };
    // The derived index is stale: it attributes a foreign memory to us and
    // retains erased ids. None may survive the store's RLS admission.
    for id in (0..25)
        .map(|_| Uuid::now_v7())
        .chain([foreign, wrong_tag, wrong_schema])
    {
        index.put(entity(id), owner, 1, &[vector(1.0)]).await?;
    }
    index
        .put(entity(visible), owner, 1, &[vector(0.1), vector(0.8)])
        .await?;
    let registry = proxiima_core::SpiiceRegistry::new().freeze_or_panic_for_tests();
    let scope = owner_scope_for_tests(&[owner]);
    let page = pg
        .search_memories(Some(&scope), &request(owner), registry.search_projections())
        .await?;
    assert_eq!(page.results.len(), 1);
    assert_eq!(page.results[0].memory_id.into_inner(), visible);
    assert!((page.results[0].score - 0.8).abs() < 0.000_001);
    assert!(page.results[0].snippet.contains("Hydrated from memory"));
    assert!(!page.has_more);
    assert_eq!(*index.windows.lock().unwrap(), vec![20, 40]);
    Ok(())
}

async fn publish_semantic_fixture(
    pg: &proxiima_storage_pg::PgStorage,
    source_owner: OwnerId,
    guest: UserId,
    published: Uuid,
    read_ceiling: i16,
) -> TestResult {
    let guest_owner = OwnerId::from(guest);
    let goal_handle = Uuid::now_v7();
    let goal_t = Uuid::now_v7();
    let mut platform = pg.platform_scope_for_host().begin().await?;
    sqlx::query(
        "INSERT INTO proxiima_core.owners(owner_id, kind)
         VALUES ($1, 'personal'), ($2, 'personal')
         ON CONFLICT DO NOTHING",
    )
    .bind(source_owner.into_inner())
    .bind(guest_owner.into_inner())
    .execute(&mut *platform)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.goal_head
             (handle, schema_id, owner_id, t, authority_generation)
         VALUES ($1, 'core/simple-text-v1', $2, $3, 0)",
    )
    .bind(goal_handle)
    .bind(source_owner.into_inner())
    .bind(goal_t)
    .execute(&mut *platform)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.goal
             (handle, t, owner_id, title, state, request_id, \"by\", via)
         VALUES ($1, $2, $3, 'semantic publication', 'Active', $4, $3, NULL)",
    )
    .bind(goal_handle)
    .bind(goal_t)
    .bind(source_owner.into_inner())
    .bind(goal_t.to_string())
    .execute(&mut *platform)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.goal_access_grant
             (goal_handle, subject_id, read_ceiling, write_ceiling, manage,
              controller_generation, granted_by)
         SELECT $1, $2, $4, 0, false, authority_generation, $3
           FROM proxiima_core.goal_head
          WHERE handle = $1",
    )
    .bind(goal_handle)
    .bind(guest.into_inner())
    .bind(source_owner.into_inner())
    .bind(read_ceiling)
    .execute(&mut *platform)
    .await?;
    sqlx::query(
        "INSERT INTO proxiima_core.goal_memory_publication
             (goal_handle, memory_t, source_owner, source_generation,
              controller_generation, permitted_use, published_by)
         SELECT $1, $2, mh.owner_id, mh.authority_generation,
                gh.authority_generation, 'read', $3
           FROM proxiima_core.memory_head mh
           JOIN proxiima_core.goal_head gh ON gh.handle = $1
          WHERE mh.t = $2 AND mh.owner_id = $4",
    )
    .bind(goal_handle)
    .bind(published)
    .bind(source_owner.into_inner())
    .bind(source_owner.into_inner())
    .execute(&mut *platform)
    .await?;
    platform.commit().await?;

    Ok(())
}

/// A Goal audience must not lose its exact published Fact when nearer private
/// source-owner vectors consume the remote index's scan cap. The source owner
/// is a selector in `read_owners`, not an authenticated owner Role.
#[tokio::test]
async fn readable_goal_semantic_search_reaches_a_far_published_fact() -> TestResult {
    let index = Arc::new(InMemoryVectorIndex::default());
    let (pg, _database) = fresh_pg_host_index("semantic_goal_publication", index.clone()).await;
    let source_owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let guest = UserId::new(Uuid::now_v7());
    let published = seed(
        pg.pool_for_tests(),
        source_owner,
        "core/agent-note-v1",
        "wanted",
    )
    .await?;
    let mut private = Vec::with_capacity(1_200);
    for _ in 0..1_200 {
        private.push(
            seed(
                pg.pool_for_tests(),
                source_owner,
                "core/agent-note-v1",
                "wanted",
            )
            .await?,
        );
    }

    publish_semantic_fixture(&pg, source_owner, guest, published, 1).await?;

    for memory in &private {
        index
            .put(fact(*memory), source_owner, 1, &[vector(0.999)])
            .await?;
    }
    index
        .put(fact(published), source_owner, 1, &[vector(0.1)])
        .await?;

    let registry = proxiima_core::SpiiceRegistry::new().freeze_or_panic_for_tests();
    let scope = proxiima_core::test_fixtures::authenticated_context(
        proxiima_core::AuthzContext::for_subject(guest, proxiima_core::AuthPath::HostBearer),
    )
    .owner_scope()
    .expect("direct guest scope")
    .clone();
    let page = pg
        .search_memories(
            Some(&scope),
            &readable_request(source_owner),
            registry.search_projections(),
        )
        .await?;
    assert_eq!(
        page.results.len(),
        1,
        "published result must survive admission"
    );
    assert_eq!(page.results[0].memory_id.into_inner(), published);
    assert!(
        !private.contains(&page.results[0].memory_id.into_inner()),
        "private source-owner memories must remain omitted"
    );
    Ok(())
}

/// A Fact-only source-owner role must not let nearer private Perspectives
/// consume the remote candidate cap before a Goal-published Perspective.
#[tokio::test]
async fn fact_role_source_owner_does_not_starve_goal_published_perspective() -> TestResult {
    let index = Arc::new(InMemoryVectorIndex::default());
    let (pg, _database) =
        fresh_pg_host_index("semantic_goal_perspective_role", index.clone()).await;
    let source_owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let guest = UserId::new(Uuid::now_v7());
    let origin = seed(
        pg.pool_for_tests(),
        source_owner,
        "core/agent-note-v1",
        "grounding",
    )
    .await?;
    let abstraction =
        seed_abstraction(pg.pool_for_tests(), source_owner, "grounding", origin).await?;
    let published =
        seed_perspective(pg.pool_for_tests(), source_owner, "wanted", abstraction).await?;
    let mut private = Vec::with_capacity(1_200);
    for _ in 0..1_200 {
        private.push(
            seed_perspective(pg.pool_for_tests(), source_owner, "wanted", abstraction).await?,
        );
    }

    publish_semantic_fixture(&pg, source_owner, guest, published, 3).await?;

    for memory in &private {
        index
            .put(
                EmbeddableEntityRef::Memory {
                    kind: EntityKind::Perspective,
                    memory_id: MemoryId::new(*memory),
                },
                source_owner,
                1,
                &[vector(0.999)],
            )
            .await?;
    }
    index
        .put(
            EmbeddableEntityRef::Memory {
                kind: EntityKind::Perspective,
                memory_id: MemoryId::new(published),
            },
            source_owner,
            1,
            &[vector(0.1)],
        )
        .await?;

    let registry = proxiima_core::SpiiceRegistry::new().freeze_or_panic_for_tests();
    let scope = proxiima_core::test_fixtures::authenticated_context(
        proxiima_core::AuthzContext::for_subject_with_role(
            guest,
            [(source_owner, Role::ingest())],
            proxiima_core::AuthPath::HostBearer,
        ),
    )
    .owner_scope()
    .expect("verified Fact-only source-owner scope")
    .clone();
    let page = pg
        .search_memories(
            Some(&scope),
            &readable_perspective_request(source_owner),
            registry.search_projections(),
        )
        .await?;
    assert_eq!(
        page.results.len(),
        1,
        "published Perspective must survive admission"
    );
    assert_eq!(page.results[0].memory_id.into_inner(), published);
    assert!(!private.contains(&page.results[0].memory_id.into_inner()));
    Ok(())
}

async fn head(
    pool: &PgPool,
    entity: Uuid,
    owner: OwnerId,
    version: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxiima_core.embedding_heads (entity_id, model_id, dim, embedding_version, owner_id)
         VALUES ($1, 'memory-index', 384, $2, $3)",
    )
    .bind(entity)
    .bind(version)
    .bind(owner.into_inner())
    .execute(pool)
    .await
    .map(drop)
}

fn key(entity: Uuid) -> VectorKey {
    VectorKey {
        entity_id: entity,
        space: EmbeddingSpace::new("memory-index", EmbeddingDim::D384),
    }
}

fn fact(id: Uuid) -> EmbeddableEntityRef {
    EmbeddableEntityRef::Memory {
        kind: EntityKind::Fact,
        memory_id: MemoryId::new(id),
    }
}

async fn debt_rows(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar("SELECT count(*) FROM proxiima_core.vector_index_debt")
        .fetch_one(pool)
        .await
}

/// A head deleted or moved leaves durable debt, and the next drain pass pays
/// it to the index: the deleted key's chunks go, the moved key's owner
/// follows.
#[tokio::test]
async fn the_drain_pays_head_deletes_and_owner_moves_to_the_index() -> TestResult {
    use proxiima_core::storage_ports::EmbeddingJobPort;
    let index = Arc::new(InMemoryVectorIndex::default());
    let (pg, _database) = fresh_pg_host_index("vector_index_debt_drain", index.clone()).await;
    let pool = pg.pool_for_tests().clone();
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let other = OwnerId::from(UserId::new(Uuid::now_v7()));
    let gone = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let moved = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    seed(&pool, other, "core/agent-note-v1", "wanted").await?;
    for entity in [gone, moved] {
        head(&pool, entity, owner, 1).await?;
        index.put(fact(entity), owner, 1, &[vector(1.0)]).await?;
    }
    sqlx::query("DELETE FROM proxiima_core.embedding_heads WHERE entity_id = $1")
        .bind(gone)
        .execute(&pool)
        .await?;
    sqlx::query("UPDATE proxiima_core.embedding_heads SET owner_id = $2 WHERE entity_id = $1")
        .bind(moved)
        .bind(other.into_inner())
        .execute(&pool)
        .await?;
    assert_eq!(debt_rows(&pool).await?, 2, "both changes owe the index");
    assert_eq!(
        index.entries(None, 10).await?.len(),
        2,
        "nothing is paid before the drain"
    );

    assert_eq!(pg.drain_vector_index_debt(100).await?, 2);
    let entries = index.entries(None, 10).await?;
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].key, key(moved));
    assert_eq!(entries[0].owner, other);
    assert_eq!(debt_rows(&pool).await?, 0);
    assert_eq!(pg.drain_vector_index_debt(100).await?, 0);
    Ok(())
}

/// The sweep compares the whole index with the heads across pages: an entry
/// with no head goes, an entry under another owner moves, and a head the
/// index lost is deleted so reconciliation embeds it again.
#[tokio::test]
async fn the_sweep_repairs_the_index_against_the_heads() -> TestResult {
    let index = Arc::new(InMemoryVectorIndex::default());
    let (pg, _database) = fresh_pg_host_index("vector_index_sweep", index.clone()).await;
    let pool = pg.pool_for_tests().clone();
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let other = OwnerId::from(UserId::new(Uuid::now_v7()));
    seed(&pool, other, "core/agent-note-v1", "wanted").await?;
    let kept = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let misowned = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let lost = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let stale_version = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    for entity in [kept, misowned, lost, stale_version] {
        head(&pool, entity, owner, 2).await?;
    }
    index.put(fact(kept), owner, 2, &[vector(1.0)]).await?;
    index.put(fact(misowned), other, 2, &[vector(1.0)]).await?;
    index
        .put(fact(stale_version), owner, 1, &[vector(1.0)])
        .await?;
    // More strays than one sweep page holds.
    for _ in 0..700 {
        index
            .put(fact(Uuid::now_v7()), owner, 1, &[vector(1.0)])
            .await?;
    }

    let outcome = pg.sweep_orphan_embedding_rows().await?;
    assert_eq!(outcome.embeddings_deleted, 700);
    assert_eq!(
        outcome.heads_deleted, 2,
        "the lost head and the stale version"
    );
    let entries = index.entries(None, 10).await?;
    let held: Vec<(VectorKey, OwnerId)> = entries.into_iter().map(|e| (e.key, e.owner)).collect();
    let mut expected = vec![
        (key(kept), owner),
        (key(misowned), owner),
        (key(stale_version), owner),
    ];
    expected.sort_by(|a, b| a.0.order().cmp(&b.0.order()));
    assert_eq!(held, expected);
    let heads: Vec<Uuid> = sqlx::query_scalar(
        "SELECT entity_id FROM proxiima_core.embedding_heads ORDER BY entity_id",
    )
    .fetch_all(&pool)
    .await?;
    let mut expected = vec![kept, misowned];
    expected.sort();
    assert_eq!(heads, expected);
    Ok(())
}

/// An owner erase deletes the owner's rows and their index entries before it
/// commits, in a database that has no pgvector to hold them.
#[tokio::test]
async fn an_owner_erase_takes_its_entries_out_of_the_index() -> TestResult {
    use proxiima_core::owner_inverse::{
        EraseAuthorization, OwnerEraseOutcome, OwnerEraseTarget, OwnerSurfaces,
    };
    use proxiima_core::storage_ports::OwnerInversePort;

    let index = Arc::new(InMemoryVectorIndex::default());
    let (pg, _database) = fresh_pg_host_index("vector_index_erase", index.clone()).await;
    let pool = pg.pool_for_tests().clone();
    let user = UserId::new(Uuid::now_v7());
    let owner = OwnerId::from(user);
    let other = OwnerId::from(UserId::new(Uuid::now_v7()));
    let erased = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let kept = seed(&pool, other, "core/agent-note-v1", "wanted").await?;
    for (entity, entity_owner) in [(erased, owner), (kept, other)] {
        head(&pool, entity, entity_owner, 1).await?;
        index
            .put(fact(entity), entity_owner, 1, &[vector(1.0)])
            .await?;
    }

    let auth = EraseAuthorization::new_for_tests(OwnerEraseTarget::PersonalOwner {
        user_id: user,
        drop_event_id: "test-drop".into(),
    });
    let surfaces = OwnerSurfaces::for_registry(
        &proxiima_core::SpiiceRegistry::new().freeze_or_panic_for_tests(),
    );
    let outcome = pg.erase_personal_owner(&auth, user, &surfaces).await?;
    assert!(
        matches!(outcome, OwnerEraseOutcome::Completed { .. }),
        "{outcome:?}"
    );

    let held: Vec<VectorKey> = index
        .entries(None, 10)
        .await?
        .into_iter()
        .map(|entry| entry.key)
        .collect();
    assert_eq!(held, [key(kept)]);
    let heads: Vec<Uuid> =
        sqlx::query_scalar("SELECT entity_id FROM proxiima_core.embedding_heads")
            .fetch_all(&pool)
            .await?;
    assert_eq!(heads, [kept]);
    assert_eq!(debt_rows(&pool).await?, 0);
    Ok(())
}

#[tokio::test]
async fn short_remote_bound_survives_local_tag_admission() -> TestResult {
    for reported in [CandidateBound::Bounded, CandidateBound::Unknown] {
        let index = Arc::new(RecordingIndex {
            reported_bound: Some(reported),
            ..RecordingIndex::default()
        });
        let (pg, _database) = fresh_pg_host_index("semantic_bound_admission", index.clone()).await;
        let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
        let wrong_tag = seed(pg.pool_for_tests(), owner, "core/agent-note-v1", "other").await?;
        index
            .put(
                EmbeddableEntityRef::Memory {
                    kind: EntityKind::Fact,
                    memory_id: MemoryId::new(wrong_tag),
                },
                owner,
                1,
                &[vector(1.0)],
            )
            .await?;
        let registry = proxiima_core::SpiiceRegistry::new().freeze_or_panic_for_tests();
        let page = pg
            .search_memories(
                Some(&owner_scope_for_tests(&[owner])),
                &request(owner),
                registry.search_projections(),
            )
            .await?;
        assert!(page.results.is_empty());
        assert!(!page.has_more);
        assert_eq!(
            page.candidate_bound, reported,
            "admission must not invent source exhaustion"
        );
        assert_eq!(*index.windows.lock().unwrap(), vec![20]);
    }
    Ok(())
}
