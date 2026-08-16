//! Authorized flavor-read facade.
//!
//! Lifted from `flavors/code/src/store.rs`: every one of
//! these helpers routes candidate filtering through
//! [`proxima_core::Engine::query`], the same authorization/visibility path
//! Owner-scoped reads use everywhere else (owner/group scoping plus
//! `World` readable-by-everyone). Flavor crates get typed, authorized
//! payload projection without ever holding a raw `PgPool` or writing SQL
//! against `proxima_core.*` themselves.
//!
//! [`authorized_code_chunk_head_candidates`] is the one exception that
//! still touches `proxima_core.*` SQL, and it does so from
//! `proxima-storage-pg` (a backend-owned storage adapter, not flavor code)
//! because `AbstractionPayload` has no natural-key/supersession concept to
//! ride on `Engine::query`'s `SupersessionStatus::HeadsOnly` today (see
//! that function's doc comment). It only narrows a candidate id list before
//! the caller's own [`authorized_abstraction_payloads`] call decides real
//! visibility, so it is safe to run without an owner-exact-match restriction.

use std::collections::{HashMap, HashSet};

use proxima_core::verbs::query::{EntityKind, QueryRequest, SupersessionStatus, TombstoneFilter};
use proxima_core::{
    AbstractionPayload, AuthzContext, Engine, FactPayload, MemoryId, Owner, SchemaId,
    SidecarPayload, ToolError,
};
use sqlx::PgPool;

/// Candidate id lists are bounded before they ever reach a query so a
/// pathological caller cannot force an unbounded `IN (...)`/`ANY($1)` scan.
const MAX_AUTHZ_CANDIDATES: usize = 2_000;

/// Narrow `candidates` to the ids visible to `authz` for `owner`, optionally
/// restricted to one `entity_kind`/`schema_id`. Heads-only, present-only,
/// same ordering-agnostic contract as the other helpers in this module.
///
/// # Errors
///
/// Returns whatever [`Engine::query`] returns for an unauthorized owner or
/// storage failure.
#[allow(clippy::too_many_arguments)]
pub async fn authorized_memory_ids(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    candidates: &[uuid::Uuid],
    entity_kind: EntityKind,
    schema_id: Option<SchemaId>,
    limit: usize,
) -> Result<Vec<MemoryId>, ToolError> {
    let candidates = bounded_candidates(candidates, limit);
    if candidates.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut req = QueryRequest::for_owner(owner);
    req.entity_kind = Some(entity_kind);
    req.schema_id = schema_id;
    req.supersession = SupersessionStatus::HeadsOnly;
    req.tombstones = TombstoneFilter::PresentOnly;
    req.limit = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
    req.include_payloads = false;
    req.memory_ids = candidates.iter().copied().map(MemoryId::new).collect();

    let visible = engine.query(authz, &req).await?;
    let visible_ids = visible
        .memories
        .into_iter()
        .map(|row| row.id.into_inner())
        .collect::<HashSet<_>>();

    Ok(candidates
        .into_iter()
        .filter(|id| visible_ids.contains(id))
        .take(limit)
        .map(MemoryId::new)
        .collect())
}

/// Authorized, typed Fact payload fetch (present-only) for a candidate id
/// list. See [`authorized_fact_payloads_include_tombstones`] to also
/// surface tombstoned heads.
///
/// # Errors
///
/// Returns whatever [`Engine::query`] returns for an unauthorized owner or
/// storage failure.
pub async fn authorized_fact_payloads<P>(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    candidates: &[uuid::Uuid],
    limit: usize,
) -> Result<Vec<(MemoryId, P)>, ToolError>
where
    P: FactPayload + Clone,
{
    authorized_fact_payloads_with_tombstone_filter::<P>(
        engine,
        authz,
        owner,
        candidates,
        TombstoneFilter::PresentOnly,
        limit,
    )
    .await
}

/// Authorized, typed Fact payload fetch that also surfaces tombstoned
/// heads (a caller-visible "this file was deleted" state, distinct from
/// entity-level GDPR tombstoning).
///
/// # Errors
///
/// Returns whatever [`Engine::query`] returns for an unauthorized owner or
/// storage failure.
pub async fn authorized_fact_payloads_include_tombstones<P>(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    candidates: &[uuid::Uuid],
    limit: usize,
) -> Result<Vec<(MemoryId, P)>, ToolError>
where
    P: FactPayload + Clone,
{
    authorized_fact_payloads_with_tombstone_filter::<P>(
        engine,
        authz,
        owner,
        candidates,
        TombstoneFilter::IncludeTombstoned,
        limit,
    )
    .await
}

async fn authorized_fact_payloads_with_tombstone_filter<P>(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    candidates: &[uuid::Uuid],
    tombstones: TombstoneFilter,
    limit: usize,
) -> Result<Vec<(MemoryId, P)>, ToolError>
where
    P: FactPayload + Clone,
{
    let payloads = authorized_payloads(
        engine,
        authz,
        owner,
        candidates,
        EntityKind::Fact,
        P::schema_id(),
        SupersessionStatus::HeadsOnly,
        tombstones,
        limit,
    )
    .await?;
    Ok(payloads
        .into_iter()
        .filter_map(|(id, payload)| payload.downcast_ref::<P>().cloned().map(|p| (id, p)))
        .collect())
}

/// Authorized, typed Abstraction payload fetch (present-only) for a
/// candidate id list.
///
/// `SupersessionStatus::HeadsOnly` is a no-op for Abstraction schemas that
/// never set `memories.supersedes` (e.g. `code-chunk-v1`, whose ingest
/// authors one row per source Fact rather than declaring a successor).
/// Route candidates for those schemas through
/// [`authorized_code_chunk_head_candidates`] first.
///
/// # Errors
///
/// Returns whatever [`Engine::query`] returns for an unauthorized owner or
/// storage failure.
pub async fn authorized_abstraction_payloads<P>(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    candidates: &[uuid::Uuid],
    limit: usize,
) -> Result<Vec<(MemoryId, P)>, ToolError>
where
    P: AbstractionPayload + Clone,
{
    let payloads = authorized_payloads(
        engine,
        authz,
        owner,
        candidates,
        EntityKind::Abstraction,
        P::schema_id(),
        SupersessionStatus::HeadsOnly,
        TombstoneFilter::PresentOnly,
        limit,
    )
    .await?;
    Ok(payloads
        .into_iter()
        .filter_map(|(id, payload)| payload.downcast_ref::<P>().cloned().map(|p| (id, p)))
        .collect())
}

#[allow(clippy::too_many_arguments)]
async fn authorized_payloads(
    engine: &Engine,
    authz: &AuthzContext,
    owner: Owner,
    candidates: &[uuid::Uuid],
    entity_kind: EntityKind,
    schema_id: SchemaId,
    supersession: SupersessionStatus,
    tombstones: TombstoneFilter,
    limit: usize,
) -> Result<Vec<(MemoryId, SidecarPayload)>, ToolError> {
    let candidates = bounded_candidates(candidates, limit);
    if candidates.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }

    let mut req = QueryRequest::for_owner(owner);
    req.entity_kind = Some(entity_kind);
    req.schema_id = Some(schema_id);
    req.supersession = supersession;
    req.tombstones = tombstones;
    req.limit = u32::try_from(candidates.len()).unwrap_or(u32::MAX);
    req.include_payloads = true;
    req.memory_ids = candidates.iter().copied().map(MemoryId::new).collect();

    let response = engine.query(authz, &req).await?;
    let mut by_id = response
        .memories
        .into_iter()
        .filter_map(|row| row.payload.map(|payload| (row.id.into_inner(), payload)))
        .collect::<HashMap<_, _>>();

    Ok(candidates
        .into_iter()
        .filter_map(|id| {
            by_id
                .remove(&id)
                .map(|payload| (MemoryId::new(id), payload))
        })
        .take(limit)
        .collect())
}

fn bounded_candidates(candidates: &[uuid::Uuid], _limit: usize) -> Vec<uuid::Uuid> {
    let cap = candidates.len().min(MAX_AUTHZ_CANDIDATES);
    let mut seen = HashSet::with_capacity(cap);
    candidates
        .iter()
        .copied()
        .filter(|id| seen.insert(*id))
        .take(MAX_AUTHZ_CANDIDATES)
        .collect()
}

/// Narrow a sidecar-only candidate id list (already known to be
/// `proxima-code/code-chunk-v1` rows via a flavor's own
/// `proxima_code.*`-only query) to the current `memory_head` `t` of each
/// series, in the owner-or-World scope.
///
/// Every candidate is evaluated: the input is deduplicated and processed in
/// `MAX_AUTHZ_CANDIDATES`-sized batches (bounding each SQL round-trip), NOT
/// truncated. Head status is `h.t = m.t`; ingest keeps one handle per
/// `(owner, repo, path, index)`.
///
/// This is a thin, PG-backend wrapper: see
/// [`proxima_storage_pg::verbs::query::authorized_code_chunk_head_candidates`]
/// for the SQL this delegates to (a fixed, compile-time query — not
/// generalized to an arbitrary sidecar table/natural key; see that
/// function's doc comment for why). `pool` is the flavor's own backend
/// pool (kept private on the flavor's store type; only the flavor crate
/// itself ever holds one).
///
/// # Errors
///
/// Returns `ToolError::Storage` on query failure.
pub async fn authorized_code_chunk_head_candidates(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    candidates: &[uuid::Uuid],
) -> Result<Vec<uuid::Uuid>, ToolError> {
    let candidates = deduped_candidates(candidates);
    let mut heads = Vec::new();
    for batch in candidates.chunks(MAX_AUTHZ_CANDIDATES) {
        heads.extend(
            proxima_storage_pg::query::authorized_code_chunk_head_candidates(
                pool, owner, schema_id, batch,
            )
            .await
            .map_err(ToolError::Storage)?,
        );
    }
    Ok(heads)
}

/// Nearest `code-chunk-v1` chunk memories to a query embedding, best-first,
/// restricted to `owner`'s scope and to the caller's structural filters.
///
/// The semantic sibling of [`authorized_code_chunk_head_candidates`], and
/// like it a *candidate* producer: the ids it returns carry no visibility
/// decision of their own. A flavor merges them with its own lexical
/// candidates and runs the whole merged list through
/// [`authorized_code_chunk_head_candidates`] and then
/// [`authorized_abstraction_payloads`], which is where authorization
/// actually happens.
///
/// Split out of the flavor because `proxima_core.embeddings` is a core
/// table and flavor SQL may not join it
/// (`scripts/check-architecture-guardrails.py`); see
/// [`proxima_storage_pg::query::nearest_code_chunk_candidates`] for the
/// query and for why World-owned chunks are unreachable through it.
///
/// # Errors
///
/// Returns `ToolError::Storage` on query failure, and
/// `ToolError::InvalidInput` if `query_embedding` is not the active
/// embedding dimension.
pub async fn nearest_code_chunk_candidates(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    model_id: &str,
    query_embedding: &[f32],
    filters: proxima_storage_pg::query::CodeChunkVectorFilters<'_>,
    limit: usize,
) -> Result<Vec<proxima_storage_pg::query::CodeChunkVectorCandidate>, ToolError> {
    proxima_storage_pg::query::nearest_code_chunk_candidates(
        pool,
        owner,
        schema_id,
        model_id,
        query_embedding,
        filters,
        i64::try_from(limit).unwrap_or(i64::MAX),
    )
    .await
    .map_err(ToolError::Storage)
}

/// Owner-only current file-revision heads of one repo (ingest poll).
///
/// # Errors
///
/// Returns `ToolError::Storage` on query failure.
pub async fn owned_file_revision_heads(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    repo_id: uuid::Uuid,
) -> Result<Vec<proxima_storage_pg::query::FileRevisionHeadRow>, ToolError> {
    proxima_storage_pg::query::owned_file_revision_heads(pool, owner, schema_id, repo_id)
        .await
        .map_err(ToolError::Storage)
}

/// Owner∪World current file-revision `t`s for one path (`open_file`).
///
/// # Errors
///
/// Returns `ToolError::Storage` on query failure.
pub async fn readable_file_revision_head_ts(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    repo_id: uuid::Uuid,
    file_path: &str,
) -> Result<Vec<uuid::Uuid>, ToolError> {
    proxima_storage_pg::query::readable_file_revision_head_ts(
        pool, owner, schema_id, repo_id, file_path,
    )
    .await
    .map_err(ToolError::Storage)
}

/// Owner-only present chunk indexes at current heads of one file (ingest).
///
/// # Errors
///
/// Returns `ToolError::Storage` on query failure.
pub async fn owned_present_chunk_indexes(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    repo_id: uuid::Uuid,
    file_path: &str,
) -> Result<Vec<i32>, ToolError> {
    proxima_storage_pg::query::owned_present_chunk_indexes(
        pool, owner, schema_id, repo_id, file_path,
    )
    .await
    .map_err(ToolError::Storage)
}

/// Owner∪World present chunk head `t`s for one file (`open_file`).
///
/// # Errors
///
/// Returns `ToolError::Storage` on query failure.
pub async fn readable_chunk_head_ts_for_file(
    pool: &PgPool,
    owner: Owner,
    schema_id: &SchemaId,
    repo_id: uuid::Uuid,
    file_path: &str,
) -> Result<Vec<uuid::Uuid>, ToolError> {
    proxima_storage_pg::query::readable_chunk_head_ts_for_file(
        pool, owner, schema_id, repo_id, file_path,
    )
    .await
    .map_err(ToolError::Storage)
}

fn deduped_candidates(candidates: &[uuid::Uuid]) -> Vec<uuid::Uuid> {
    let mut seen = HashSet::with_capacity(candidates.len());
    candidates
        .iter()
        .copied()
        .filter(|id| seen.insert(*id))
        .collect()
}
