use proxima_core::verbs::query::{
    EntityKind, MAX_SEARCH_PAGE_LIMIT, MemorySearchPage, MemorySearchRequest, MemorySearchResult,
    SearchMode,
};
use proxima_core::verbs::schema::MemorySearchProjection;
use proxima_core::{MemoryId, OwnerRef, SchemaId, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::tuning::PgTuning;

pub(crate) async fn search_memories(
    pool: &PgPool,
    req: &MemorySearchRequest,
    projections: &[MemorySearchProjection],
    tuning: &PgTuning,
) -> Result<MemorySearchPage, StorageError> {
    if matches!(req.kind, Some(EntityKind::Goal)) || req.limit == 0 {
        return Ok(MemorySearchPage {
            results: Vec::new(),
            has_more: false,
        });
    }
    let _ = (projections, tuning);
    search_memories_timeseries(pool, req).await
}

async fn search_memories_timeseries(
    pool: &PgPool,
    req: &MemorySearchRequest,
) -> Result<MemorySearchPage, StorageError> {
    let owner_ids: Vec<uuid::Uuid> = req
        .read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let pattern = format!("%{}%", req.query.replace('%', "\\%").replace('_', "\\_"));
    let limit = i64::from(req.limit.min(MAX_SEARCH_PAGE_LIMIT).saturating_add(1));
    let kind_filter = match req.kind {
        Some(EntityKind::Fact) => Some("fact"),
        Some(EntityKind::Abstraction) => Some("abstraction"),
        Some(EntityKind::Perspective) => Some("perspective"),
        Some(EntityKind::Goal) | None => None,
    };
    if matches!(req.mode, SearchMode::Semantic)
        && let (Some(embedding), Some(model_id)) =
            (req.query_embedding.as_deref(), req.embedding_model_id.as_deref())
    {
        return search_memories_timeseries_semantic(
            pool,
            &owner_ids,
            kind_filter,
            model_id,
            embedding,
            limit,
            req.limit,
        )
        .await;
    }
    let rows: Vec<(
        uuid::Uuid,
        String,
        String,
        time::OffsetDateTime,
        Option<String>,
    )> = sqlx::query_as(
        "SELECT m.t,
                m.kind::text,
                h.schema_id,
                COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01'),
                LEFT(COALESCE(n.body, u.text, d.body, i.claim, ''), 240)
           FROM proxima_core.memory_head h
           JOIN proxima_core.memory m ON m.handle = h.handle AND m.t = h.t
           LEFT JOIN proxima_core.agent_note_v1 n ON n.t = m.t
           LEFT JOIN proxima_core.utterance_v1 u ON u.t = m.t
           LEFT JOIN proxima_core.agent_derivation_v1 d ON d.t = m.t
           LEFT JOIN proxima_core.interpretation_v1 i ON i.t = m.t
          WHERE m.owner_id = ANY($1::uuid[])
            AND ($3::text IS NULL OR m.kind::text = $3)
            AND (
                n.title ILIKE $2 ESCAPE '\\'
                OR n.body ILIKE $2 ESCAPE '\\'
                OR u.text ILIKE $2 ESCAPE '\\'
                OR d.body ILIKE $2 ESCAPE '\\'
                OR i.claim ILIKE $2 ESCAPE '\\'
            )
          ORDER BY m.t DESC
          LIMIT $4",
    )
    .bind(&owner_ids)
    .bind(&pattern)
    .bind(kind_filter)
    .bind(limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let page_len = usize::try_from(req.limit.min(MAX_SEARCH_PAGE_LIMIT)).unwrap_or(usize::MAX);
    let has_more = rows.len() > page_len;
    let results = rows
        .into_iter()
        .take(page_len)
        .filter_map(|(t, kind, schema_id, created_at, snippet)| {
            let kind = match kind.as_str() {
                "fact" => EntityKind::Fact,
                "abstraction" => EntityKind::Abstraction,
                "perspective" => EntityKind::Perspective,
                _ => return None,
            };
            Some(MemorySearchResult {
                memory_id: MemoryId::new(t),
                kind,
                schema_id: SchemaId::new(schema_id),
                created_at,
                snippet: snippet.unwrap_or_default(),
                score: 1.0,
                lexical_score: 1.0,
                similarity_score: 0.0,
            })
        })
        .collect();
    Ok(MemorySearchPage { results, has_more })
}

async fn search_memories_timeseries_semantic(
    pool: &PgPool,
    owner_ids: &[uuid::Uuid],
    kind_filter: Option<&str>,
    model_id: &str,
    embedding: &[f32],
    fetch_limit: i64,
    page_limit: u32,
) -> Result<MemorySearchPage, StorageError> {
    let rows: Vec<(uuid::Uuid, String, String, time::OffsetDateTime, f32)> = sqlx::query_as(
        "SELECT m.t,
                m.kind::text,
                h.schema_id,
                COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01'),
                GREATEST(0.0, (1 - (emb.vec <=> $4::vector)))::real
           FROM proxima_core.embeddings emb
           JOIN proxima_core.embedding_heads head
             ON head.entity_id = emb.entity_id
            AND head.model_id = emb.model_id
            AND head.embedding_version = emb.embedding_version
           JOIN proxima_core.memory m ON m.t = emb.entity_id
           JOIN proxima_core.memory_head h ON h.handle = m.handle AND h.t = m.t
          WHERE m.owner_id = ANY($1::uuid[])
            AND emb.model_id = $2
            AND ($3::text IS NULL OR m.kind::text = $3)
          ORDER BY emb.vec <=> $4::vector
          LIMIT $5",
    )
    .bind(owner_ids)
    .bind(model_id)
    .bind(kind_filter)
    .bind(crate::pgvector::literal(embedding))
    .bind(fetch_limit)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;
    let page_len = usize::try_from(page_limit.min(MAX_SEARCH_PAGE_LIMIT)).unwrap_or(usize::MAX);
    let has_more = rows.len() > page_len;
    let results = rows
        .into_iter()
        .take(page_len)
        .filter_map(|(t, kind, schema_id, created_at, score)| {
            let kind = match kind.as_str() {
                "fact" => EntityKind::Fact,
                "abstraction" => EntityKind::Abstraction,
                "perspective" => EntityKind::Perspective,
                _ => return None,
            };
            Some(MemorySearchResult {
                memory_id: MemoryId::new(t),
                kind,
                schema_id: SchemaId::new(schema_id),
                created_at,
                snippet: String::new(),
                score,
                lexical_score: 0.0,
                similarity_score: score,
            })
        })
        .collect();
    Ok(MemorySearchPage { results, has_more })
}
