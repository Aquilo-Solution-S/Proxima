//! Semantic retrieval with an independent in-memory index and real owner RLS.

use std::sync::{Arc, Mutex};

use proxima_core::storage_ports::{MemoryReadPort, VectorCandidate, VectorIndex};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::{
    EmbeddableEntityRef, EmbeddingDim, EmbeddingSpace, EmbeddingWriteOutcome, MemoryId, OwnerId,
    SpaceVector, StorageError, UserId,
};
use proxima_storage_pg::test_fixtures::fresh_pg;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct IndexedMemory {
    owner: OwnerId,
    id: Uuid,
    chunks: Vec<SpaceVector>,
}

#[derive(Default)]
struct InMemoryIndex {
    memories: Mutex<Vec<IndexedMemory>>,
    windows: Mutex<Vec<u32>>,
}

#[async_trait::async_trait]
impl VectorIndex for InMemoryIndex {
    type Context = Transaction<'static, Postgres>;

    async fn add(
        &self,
        _context: &mut Self::Context,
        owner: &OwnerId,
        entity: EmbeddableEntityRef,
        vectors: &[SpaceVector],
    ) -> Result<EmbeddingWriteOutcome, StorageError> {
        let mut memories = self.memories.lock().unwrap();
        memories.retain(|memory| {
            memory.owner != *owner
                || memory.id != entity.entity_id()
                || memory.chunks[0].space() != vectors[0].space()
        });
        memories.push(IndexedMemory {
            owner: *owner,
            id: entity.entity_id(),
            chunks: vectors.to_vec(),
        });
        Ok(EmbeddingWriteOutcome {
            embedding_version: 1,
        })
    }

    async fn delete(
        &self,
        _context: &mut Self::Context,
        owner: &OwnerId,
        entity: EmbeddableEntityRef,
    ) -> Result<(), StorageError> {
        self.memories
            .lock()
            .unwrap()
            .retain(|memory| memory.owner != *owner || memory.id != entity.entity_id());
        Ok(())
    }

    async fn nearest(
        &self,
        _context: &mut Self::Context,
        owners: &[OwnerId],
        query: &SpaceVector,
        window: u32,
    ) -> Result<Vec<VectorCandidate>, StorageError> {
        self.windows.lock().unwrap().push(window);
        let mut candidates = Vec::new();
        for memory in self.memories.lock().unwrap().iter() {
            if !owners.contains(&memory.owner) {
                continue;
            }
            let score = memory
                .chunks
                .iter()
                .filter(|chunk| chunk.space() == query.space())
                .map(|chunk| cosine(chunk.values(), query.values()).clamp(0.0, 1.0))
                .max_by(f32::total_cmp);
            if let Some(similarity_score) = score {
                candidates.push(VectorCandidate {
                    memory_id: MemoryId::new(memory.id),
                    similarity_score,
                });
            }
        }
        candidates.sort_by(|left, right| {
            right
                .similarity_score
                .total_cmp(&left.similarity_score)
                .then_with(|| {
                    right
                        .memory_id
                        .into_inner()
                        .cmp(&left.memory_id.into_inner())
                })
        });
        candidates.truncate(usize::try_from(window).unwrap());
        Ok(candidates)
    }
}

fn cosine(left: &[f32], right: &[f32]) -> f32 {
    let dot: f32 = left.iter().zip(right).map(|(x, y)| x * y).sum();
    let left_norm: f32 = left.iter().map(|value| value * value).sum();
    let right_norm: f32 = right.iter().map(|value| value * value).sum();
    dot / (left_norm * right_norm).sqrt()
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
    let index = Arc::new(InMemoryIndex::default());
    let mut tx = pg.pool_for_tests().begin().await?;
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
        index
            .add(&mut tx, &owner, entity(id), &[vector(1.0)])
            .await?;
    }
    index
        .add(
            &mut tx,
            &owner,
            entity(visible),
            &[vector(0.1), vector(0.8)],
        )
        .await?;
    tx.commit().await?;
    let pg = pg.with_vector_index_for_tests(index.clone());
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
