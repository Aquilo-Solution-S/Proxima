//! Authorized flavor-read facade.
//!
//! Lifted from `flavors/code/src/store.rs`: every one of
//! these helpers routes candidate filtering through
//! [`proxima_core::Engine::query`], the same authorization/visibility path
//! Owner-scoped reads use everywhere else (owner/group scoping). Flavor
//! crates get typed, authorized
//! payload projection without ever holding a raw `PgPool` or writing SQL
//! against `proxima_core.*` themselves.
//!
//! Chunk series heads ride `Engine::query` `HeadsOnly` (`memory_head`).
//! Listing heads by NK (ingest / `open_file`) lives in storage-pg
//! `code_series_heads`, not here.

use std::collections::{HashMap, HashSet};

use proxima_core::verbs::query::{EntityKind, QueryRequest, SupersessionStatus};
use proxima_core::{
    AbstractionPayload, AuthzContext, Engine, FactPayload, MemoryId, Owner, SchemaId,
    SidecarPayload, ToolError,
};

/// Candidate id lists are bounded before they ever reach a query so a
/// pathological caller cannot force an unbounded `IN (...)`/`ANY($1)` scan.
const MAX_AUTHZ_CANDIDATES: usize = 2_000;

/// Narrow `candidates` to the ids visible to `authz` for `owner`, optionally
/// restricted to one `entity_kind`/`schema_id`. Heads-only,
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

/// Authorized, typed Fact payload fetch for a candidate id list.
///
/// Reads return the current hot head. A flavor-defined tombstone payload is
/// itself a hot head and remains observable; it is not a core query state.
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
    let payloads = authorized_payloads(
        engine,
        authz,
        owner,
        candidates,
        EntityKind::Fact,
        P::schema_id(),
        SupersessionStatus::HeadsOnly,
        limit,
    )
    .await?;
    Ok(payloads
        .into_iter()
        .filter_map(|(id, payload)| payload.downcast_ref::<P>().cloned().map(|p| (id, p)))
        .collect())
}

/// Authorized, typed Abstraction payload fetch for a candidate id list.
///
/// `SupersessionStatus::HeadsOnly` is the chunk admit: ingest keeps one
/// handle per `(owner, repo, path, index)`, so `memory_head.t` is the
/// current revision. Do not pre-filter the candidate list.
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
        limit,
    )
    .await?;
    Ok(payloads
        .into_iter()
        .filter_map(|(id, payload)| payload.downcast_ref::<P>().cloned().map(|p| (id, p)))
        .collect())
}

/// The stored owner ids of the caller's read access set (`S_read`).
///
/// For a flavor that runs its own candidate scan before admitting through
/// [`Engine::query`]. Binding this array into the scan's `owner_id = ANY($n)`
/// is what makes a `gin(owner_id, search_tsv)` projection index reachable:
/// with the owner reached through a join instead, the planner has the two
/// halves of a two-column index on two relations and uses neither. It is a
/// narrowing, never a widening — the same set the admit would have applied,
/// applied one phase earlier.
///
/// # Errors
///
/// Returns `Forbidden` when the context resolves to an empty read set.
pub async fn read_owner_ids(
    engine: &Engine,
    authz: &AuthzContext,
) -> Result<Vec<uuid::Uuid>, ToolError> {
    Ok(engine
        .authorized_read_owners(authz)
        .await?
        .into_iter()
        .map(proxima_core::OwnerRef::stored_owner_id)
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
