//! Shared fixtures for the `core_search_memories` PG integration suites.
//!
//! The test bodies live in the `search_pg/` submodules, grouped by the part
//! of the search contract they pin: `semantic`, `lexical`, `filters`,
//! `ranking`, `pagination`, and `plans` (EXPLAIN-based plan-shape
//! regressions). Everything below is fixture surface shared across them.

use crate::common::{drop_db, fresh_pg, owner_fixture};
use proxima_core::storage_ports::*;

use proxima_core::llm::EMBEDDING_DIM;
use proxima_core::verbs::query::{
    EntityKind, MemorySearchRequest, SearchMode, SearchOrder, SupersessionStatus, TagMatch,
};
use proxima_core::verbs::schema::{
    MemorySearchProjection, MemorySearchProjectionField, PayloadKind,
};
use proxima_core::{
    FactReceiptDraft, MemoryId, Owner, SchemaId, SchemaVersion, SearchProjectionColumnKind,
    SourceBatchId, SourceId,
};
use uuid::Uuid;

fn hybrid_request(owner: &Owner, query: &str, query_embedding: Vec<f32>) -> MemorySearchRequest {
    MemorySearchRequest {
        owner: *owner,
        read_owners: vec![*owner],
        query: query.into(),
        mode: SearchMode::Hybrid,
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
        query_embedding: Some(query_embedding),
        embedding_model_id: Some("test-embed".into()),
    }
}

#[derive(Debug)]
struct TaggedAbstractionInsert<'a> {
    memory_id: Uuid,
    title: &'a str,
    body: &'a str,
    tags: &'a [&'a str],
    created_at: time::OffsetDateTime,
    embedding: Option<[f32; 3]>,
}

async fn create_tagged_search_sidecars(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_test")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.tagged_abstraction_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             title text NOT NULL,
             body text NOT NULL,
             tags text[] NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_tagged_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    input: TaggedAbstractionInsert<'_>,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, created_at,
             kind, text, operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'proxima-test/tagged-abstraction-v1', 1,
                 $4, 'Abstraction', $5, 'AtoA',
                 '00000000-0000-0000-0000-000000000321'::uuid,
                 '00000000-0000-0000-0000-000000000322'::uuid, NULL,
                 'test-model', 'test-v1')"
    )
    .bind(input.memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(input.created_at)
    .bind(input.body)
    .execute(pg.pool_for_tests())
    .await?;
    let tags: Vec<String> = input.tags.iter().map(|tag| (*tag).to_string()).collect();
    sqlx::query(
        "INSERT INTO proxima_test.tagged_abstraction_v1 (memory_id, title, body, tags)
         VALUES ($1, $2, $3, $4)",
    )
    .bind(input.memory_id)
    .bind(input.title)
    .bind(input.body)
    .bind(tags)
    .execute(pg.pool_for_tests())
    .await?;
    if let Some(embedding) = input.embedding {
        insert_embedding_with_head(
            pg.pool_for_tests(),
            EntityKind::Abstraction,
            input.memory_id,
            "test-embed",
            1,
            &padded_embedding(embedding),
            owner_kind,
            owner_id,
        )
        .await?;
    }
    Ok(MemoryId::new(input.memory_id))
}

async fn insert_embedded_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    embedding: [f32; 3],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let embedding = padded_embedding(embedding);
    insert_embedded_memory_with_vec(pg, owner, text, &embedding).await
}

async fn insert_embedded_memory_with_vec(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    embedding: &[f32],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    insert_embedded_memory_with_schema(pg, owner, "test/search-abstraction-v1", text, embedding)
        .await
}

async fn insert_embedded_memory_with_schema(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    schema_id: &str,
    text: &str,
    embedding: &[f32],
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, $4, 1,
                 'Abstraction', $5, 'AtoA',
                 '00000000-0000-0000-0000-000000000323'::uuid,
                 '00000000-0000-0000-0000-000000000324'::uuid, NULL,
                 'test-model', 'test-v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(schema_id)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    insert_embedding_with_head(
        pg.pool_for_tests(),
        EntityKind::Abstraction,
        memory_id,
        "test-embed",
        1,
        embedding,
        owner_kind,
        owner_id,
    )
    .await?;
    Ok(memory_id)
}

async fn insert_text_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'test/search-attribution-v1', 1,
                 'Abstraction', $4, 'AtoA',
                 '00000000-0000-0000-0000-000000000325'::uuid,
                 '00000000-0000-0000-0000-000000000326'::uuid, NULL,
                 'test-model', 'test-v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

async fn insert_search_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    text: &str,
    supersedes: Option<Uuid>,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let memory_id = Uuid::now_v7();
    let (owner_kind, owner_id) = proxima_storage_pg::access::owner_columns::owner_binds(owner);
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, kind, text,
             operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version, supersedes)
         VALUES ($1, $2, $3, 'test/search-abstraction-v1', 1,
                 'Abstraction', $4, 'AtoA',
                 '00000000-0000-0000-0000-000000000327'::uuid,
                 '00000000-0000-0000-0000-000000000328'::uuid, NULL,
                 'test-model', 'test-v1', $5)"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(text)
    .bind(supersedes)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(memory_id)
}

async fn ingest_fact_memory(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    schema_id: &str,
    payload: &[u8],
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    use proxima_core::verbs::fact_ingest::{
        Citation, CitationMappingHint, CitedObjectHint, FactWriteCommand,
    };

    let permit = crate::common::owner_write_permit(owner, proxima_core::AccessKind::Fact).await?;
    let now = time::OffsetDateTime::now_utc();
    let outcome = pg
        .ingest_fact_atomic(
            &permit,
            &FactWriteCommand {
                schema_id: SchemaId::new(schema_id.to_string()),
                schema_version: SchemaVersion::new(1),
                payload: payload.to_vec(),
                rendered_text: None,
                lexical_language: None,
                receipt: Some(FactReceiptDraft {
                    source_id: SourceId::new("test/search"),
                    source_batch_id: SourceBatchId::new(Uuid::now_v7()),
                    observed_at: now,
                    occurred_at: now,
                }),
                citation: Some(Citation {
                    object: CitedObjectHint {
                        schema_id: SchemaId::new("test/search-object-v1".into()),
                        schema_version: SchemaVersion::new(1),
                        content_hash: *blake3::hash(payload).as_bytes(),
                    },
                    mapping: CitationMappingHint {
                        schema_id: SchemaId::new("test/search-whole-v1".into()),
                        schema_version: SchemaVersion::new(1),
                    },
                }),
                derived_from: None,
            },
            None,
        )
        .await?;
    Ok(outcome.memory_id)
}

fn lexical_request(owner: &Owner, query: &str) -> MemorySearchRequest {
    MemorySearchRequest {
        owner: *owner,
        read_owners: vec![*owner],
        query: query.into(),
        mode: SearchMode::Lexical,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 10,
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

fn semantic_request(owner: &Owner, query_embedding: Vec<f32>) -> MemorySearchRequest {
    MemorySearchRequest {
        owner: *owner,
        read_owners: vec![*owner],
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
        query_embedding: Some(query_embedding),
        embedding_model_id: Some("test-embed".into()),
    }
}

fn tagged_search_request(owner: &Owner, query: &str, mode: SearchMode) -> MemorySearchRequest {
    MemorySearchRequest {
        owner: *owner,
        read_owners: vec![*owner],
        query: query.into(),
        mode,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 10,
        kind: Some(EntityKind::Abstraction),
        schema_id: Some(SchemaId::new("proxima-test/tagged-abstraction-v1".into())),
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

fn padded_embedding(prefix: [f32; 3]) -> Vec<f32> {
    let mut embedding = vec![0.0; EMBEDDING_DIM];
    embedding[..prefix.len()].copy_from_slice(&prefix);
    embedding
}

#[allow(clippy::too_many_arguments)]
async fn insert_embedding_with_head(
    pool: &sqlx::PgPool,
    entity_kind: EntityKind,
    entity_id: Uuid,
    model_id: &str,
    embedding_version: i32,
    embedding: &[f32],
    owner_kind: proxima_core::OwnerRefKind,
    owner_id: Option<Uuid>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO proxima_core.embeddings
            (entity_kind, entity_id, embedding_version, model_id, vec,
             owner_kind, owner_id)
         VALUES ($1, $2, $3, $4, $5::vector, $6, $7)",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(embedding_version)
    .bind(model_id)
    .bind(vector_literal(embedding))
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await?;
    sqlx::query(
        "INSERT INTO proxima_core.embedding_heads
            (entity_kind, entity_id, model_id, embedding_version, owner_kind, owner_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (entity_kind, entity_id, model_id)
         DO UPDATE SET
             embedding_version = EXCLUDED.embedding_version,
             owner_kind = EXCLUDED.owner_kind,
             owner_id = EXCLUDED.owner_id,
             updated_at = now()",
    )
    .bind(entity_kind)
    .bind(entity_id)
    .bind(model_id)
    .bind(embedding_version)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(pool)
    .await?;
    Ok(())
}

fn vector_literal(vec: &[f32]) -> String {
    let mut out = String::with_capacity(vec.len().saturating_mul(8).saturating_add(2));
    out.push('[');
    for (idx, value) in vec.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str(&value.to_string());
    }
    out.push(']');
    out
}

fn brute_cosine(stored: &[f32], query: &[f32]) -> f32 {
    let dot: f32 = stored
        .iter()
        .zip(query.iter())
        .map(|(stored, query)| stored * query)
        .sum();
    let stored_norm = stored.iter().map(|value| value * value).sum::<f32>().sqrt();
    let query_norm = query.iter().map(|value| value * value).sum::<f32>().sqrt();
    if stored_norm <= f32::EPSILON || query_norm <= f32::EPSILON {
        0.0
    } else {
        (dot / (stored_norm * query_norm)).max(0.0)
    }
}

fn code_chunk_projection() -> MemorySearchProjection {
    MemorySearchProjection {
        schema_id: SchemaId::new("proxima-code/code-chunk-v1".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Fact,
        sidecar_table: "proxima_code.code_chunk_v1".into(),
        fields: vec![
            MemorySearchProjectionField {
                column: "file_path".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "language".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "chunk_type".into(),
                kind: SearchProjectionColumnKind::Text,
            },
        ],
        tag_column: None,
        tsv_column: None,
        language_column: None,
    }
}

fn tagged_abstraction_projection() -> MemorySearchProjection {
    MemorySearchProjection {
        schema_id: SchemaId::new("proxima-test/tagged-abstraction-v1".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Abstraction,
        sidecar_table: "proxima_test.tagged_abstraction_v1".into(),
        fields: vec![
            MemorySearchProjectionField {
                column: "title".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "body".into(),
                kind: SearchProjectionColumnKind::Text,
            },
            MemorySearchProjectionField {
                column: "tags".into(),
                kind: SearchProjectionColumnKind::TextArray,
            },
        ],
        tag_column: Some("tags".into()),
        tsv_column: None,
        language_column: None,
    }
}

/// A sidecar that carries tags and *no text column at all* — the shape
/// [`SearchProjectionColumnKind::MemoryText`] exists to make legal. Every
/// other projection fixture here copies the memory's text into the
/// sidecar to stay reachable under a tag filter; this one does not, so a
/// match proves the branch really read `proxima_core.memories.text`.
async fn create_memory_text_sidecar(pool: &sqlx::PgPool) -> Result<(), sqlx::Error> {
    sqlx::query("CREATE SCHEMA IF NOT EXISTS proxima_test")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE TABLE proxima_test.memory_text_abstraction_v1 (
             memory_id uuid PRIMARY KEY REFERENCES proxima_core.memories(memory_id),
             tags text[] NOT NULL
         )",
    )
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert_memory_text_abstraction(
    pg: &proxima_storage_pg::PgStorage,
    owner: &Owner,
    memory_id: Uuid,
    text: &str,
    tags: &[&str],
    created_at: time::OffsetDateTime,
) -> Result<MemoryId, Box<dyn std::error::Error>> {
    let (owner_kind, owner_id) = owner.columns();
    sqlx::query(
        "INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version, created_at,
             kind, text, operator_kind, operator_id, input_contract_id, source_batch_id, model_id, prompt_version)
         VALUES ($1, $2, $3, 'proxima-test/memory-text-abstraction-v1', 1,
                 $4, 'Abstraction', $5, 'AtoA',
                 '00000000-0000-0000-0000-000000000331'::uuid,
                 '00000000-0000-0000-0000-000000000332'::uuid, NULL,
                 'test-model', 'test-v1')"
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(created_at)
    .bind(text)
    .execute(pg.pool_for_tests())
    .await?;
    let tags: Vec<String> = tags.iter().map(|tag| (*tag).to_string()).collect();
    sqlx::query(
        "INSERT INTO proxima_test.memory_text_abstraction_v1 (memory_id, tags)
         VALUES ($1, $2)",
    )
    .bind(memory_id)
    .bind(tags)
    .execute(pg.pool_for_tests())
    .await?;
    Ok(MemoryId::new(memory_id))
}

fn memory_text_projection() -> MemorySearchProjection {
    MemorySearchProjection {
        schema_id: SchemaId::new("proxima-test/memory-text-abstraction-v1".into()),
        schema_version: SchemaVersion::new(1),
        kind: PayloadKind::Abstraction,
        sidecar_table: "proxima_test.memory_text_abstraction_v1".into(),
        fields: vec![MemorySearchProjectionField {
            column: String::new(),
            kind: SearchProjectionColumnKind::MemoryText,
        }],
        tag_column: Some("tags".into()),
        tsv_column: None,
        language_column: None,
    }
}

fn any_kind_lexical_request(owner: &Owner, query: &str) -> MemorySearchRequest {
    MemorySearchRequest {
        owner: *owner,
        read_owners: vec![*owner],
        query: query.into(),
        mode: SearchMode::Lexical,
        supersession: SupersessionStatus::HeadsOnly,
        limit: 10,
        kind: None,
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

fn plan_seq_scans_relation(plan: &serde_json::Value, relation: &str) -> bool {
    if plan.get("Node Type").and_then(serde_json::Value::as_str) == Some("Seq Scan")
        && plan
            .get("Relation Name")
            .and_then(serde_json::Value::as_str)
            == Some(relation)
    {
        return true;
    }
    plan.get("Plans")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|plans| {
            plans
                .iter()
                .any(|child| plan_seq_scans_relation(child, relation))
        })
}

mod filters;
mod lexical;
mod pagination;
mod per_row_language;
mod plans;
mod ranking;
mod semantic;
mod stored_tsv;
