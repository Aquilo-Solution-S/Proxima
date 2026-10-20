//! Semantic retrieval with an independent in-memory index and real owner RLS.

use std::sync::{Arc, Mutex};

use proxima_core::storage_ports::{
    InMemoryVectorIndex, MemoryReadPort, VectorCandidate, VectorEntry, VectorIndex, VectorKey,
};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::{
    EmbeddableEntityRef, EmbeddingDim, EmbeddingSpace, MemoryId, OwnerId, SpaceVector,
    StorageError, UserId,
};
use proxima_storage_pg::test_fixtures::fresh_pg;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

/// The reference index, recording each window the store asks for.
#[derive(Default)]
struct RecordingIndex {
    inner: InMemoryVectorIndex,
    windows: Mutex<Vec<u32>>,
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
        owners: &[OwnerId],
        query: &SpaceVector,
        window: u32,
    ) -> Result<Vec<VectorCandidate>, StorageError> {
        self.windows.lock().unwrap().push(window);
        self.inner.nearest(owners, query, window).await
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
        owner,
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

async fn seed(pool: &PgPool, owner: OwnerId, schema: &str, tag: &str) -> Result<Uuid, sqlx::Error> {
    sqlx::query("INSERT INTO proxima_core.owners(owner_id, kind) VALUES ($1, 'personal') ON CONFLICT DO NOTHING")
        .bind(owner.into_inner()).execute(pool).await?;
    let t = Uuid::now_v7();
    let handle = Uuid::now_v7();
    sqlx::query("INSERT INTO proxima_core.memory_head(handle, t, kind, owner_id, schema_id) VALUES ($1, $2, 'fact', $3, $4)")
        .bind(handle).bind(t).bind(owner.into_inner()).bind(schema).execute(pool).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("INSERT INTO proxima_core.memory(handle, t, kind, owner_id, schema_id, sidecar_tables, occurred_at, \"by\", via) VALUES ($1, $2, 'fact', $3, $4, $5, uuid_extract_timestamp($2), '00000000-0000-0000-0000-000000000509'::uuid, NULL)")
        .bind(handle).bind(t).bind(owner.into_inner()).bind(schema)
        .bind(if schema == "core/agent-note-v1" { vec!["proxima_core.agent_note_v1"] } else { Vec::new() })
        .execute(&mut *tx).await?;
    if schema == "core/agent-note-v1" {
        sqlx::query("INSERT INTO proxima_core.agent_note_v1(t, note_id, title, body, tags) VALUES ($1, $2, 'Index result', 'Hydrated from memory', ARRAY[$3])")
            .bind(t).bind(Uuid::now_v7()).bind(tag).execute(&mut *tx).await?;
    }
    sqlx::query("INSERT INTO proxima_core.projection(memory_id, schema_id, owner_id, search_tsv, tag) VALUES ($1, $2, $3, ''::tsvector, ARRAY[$4])")
        .bind(t).bind(schema).bind(owner.into_inner()).bind(tag).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(t)
}

#[tokio::test]
async fn semantic_port_expands_past_stale_foreign_and_filtered_candidates() -> TestResult {
    let (pg, _database) = fresh_pg("semantic_memory_index").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let other = OwnerId::from(UserId::new(Uuid::now_v7()));
    let visible = seed(pg.pool_for_tests(), owner, "core/agent-note-v1", "wanted").await?;
    let wrong_tag = seed(pg.pool_for_tests(), owner, "core/agent-note-v1", "other").await?;
    let wrong_schema = seed(pg.pool_for_tests(), owner, "semantic/other-v1", "wanted").await?;
    let foreign = seed(pg.pool_for_tests(), other, "core/agent-note-v1", "wanted").await?;
    let index = Arc::new(RecordingIndex::default());
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
    let pg = pg.with_vector_index(index.clone());
    let registry = proxima_core::FlavorRegistry::new().freeze_or_panic_for_tests();
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

async fn head(
    pool: &PgPool,
    entity: Uuid,
    owner: OwnerId,
    version: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads (entity_id, model_id, dim, embedding_version, owner_id)
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
    sqlx::query_scalar("SELECT count(*) FROM proxima_core.vector_index_debt")
        .fetch_one(pool)
        .await
}

/// A head deleted or moved leaves durable debt, and the next drain pass pays
/// it to the index: the deleted key's chunks go, the moved key's owner
/// follows.
#[tokio::test]
async fn the_drain_pays_head_deletes_and_owner_moves_to_the_index() -> TestResult {
    use proxima_core::storage_ports::EmbeddingJobPort;
    let (pg, _database) = fresh_pg("vector_index_debt_drain").await;
    let pool = pg.pool_for_tests().clone();
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let other = OwnerId::from(UserId::new(Uuid::now_v7()));
    let gone = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let moved = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    seed(&pool, other, "core/agent-note-v1", "wanted").await?;
    let index = Arc::new(InMemoryVectorIndex::default());
    for entity in [gone, moved] {
        head(&pool, entity, owner, 1).await?;
        index.put(fact(entity), owner, 1, &[vector(1.0)]).await?;
    }
    let pg = pg.with_vector_index(index.clone());
    sqlx::query("DELETE FROM proxima_core.embedding_heads WHERE entity_id = $1")
        .bind(gone)
        .execute(&pool)
        .await?;
    sqlx::query("UPDATE proxima_core.embedding_heads SET owner_id = $2 WHERE entity_id = $1")
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
    let (pg, _database) = fresh_pg("vector_index_sweep").await;
    let pool = pg.pool_for_tests().clone();
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let other = OwnerId::from(UserId::new(Uuid::now_v7()));
    seed(&pool, other, "core/agent-note-v1", "wanted").await?;
    let kept = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let misowned = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let lost = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let stale_version = seed(&pool, owner, "core/agent-note-v1", "wanted").await?;
    let index = Arc::new(InMemoryVectorIndex::default());
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
    let pg = pg.with_vector_index(index.clone());

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
    let heads: Vec<Uuid> =
        sqlx::query_scalar("SELECT entity_id FROM proxima_core.embedding_heads ORDER BY entity_id")
            .fetch_all(&pool)
            .await?;
    let mut expected = vec![kept, misowned];
    expected.sort();
    assert_eq!(heads, expected);
    Ok(())
}
