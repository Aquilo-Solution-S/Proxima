//! `Query` verb — paginated read of `memories` with optional
//! head filtering. Two head modes (docs/02 §Re-derivation, docs/03
//! §Stateful Fact schemas):
//!
//! - A/P: `NOT EXISTS (m2.supersedes = m.memory_id)` (lineage scan).
//! - Stateful Fact: `NOT EXISTS` of a row under the same NK tuple
//!   with a later `created_at` (head-by-natural-key).
//!
//! `stateful_heads` is set by the engine from the schema registry
//! before dispatch when the request is heads-only and `schema_id`
//! resolves to a stateful Fact schema.
//!
//! Payload projection: the selected memory rows are hydrated through
//! typed PG sidecar loaders registered by the owning flavor.

use std::collections::HashMap;
use std::fmt::Write as _;

use proxima_core::{
    FactEntityId, Owner, OwnerRef, OwnerRefKind, SchemaId, SchemaVersion, StorageError,
};
use sqlx::{Executor, PgConnection, PgPool, Postgres};

mod abstraction_heads;
mod citations;
mod edges;
mod goals;
mod lineage;
mod memories;
mod rows;
mod search;

pub use abstraction_heads::authorized_code_chunk_head_candidates;
pub(crate) use citations::{citation_of_entity_head, citation_of_fact, facts_citing_object};
pub use edges::MAX_SNAPSHOT_EDGES;
pub(crate) use edges::{edge_exists, read_edges};
pub(crate) use lineage::walk_memory_lineage;
pub(crate) use memories::query_memories;
pub(crate) use search::search_memories;

/// Append the same-owner successor predicate. Supersession is intra-Owner
/// (`Causa/Edges.lean` `supersession_intra_owner`), so a cross-Owner
/// successor never hides another Owner's head.
pub(super) fn push_same_home_owner_successor_predicate(
    sql: &mut String,
    successor_alias: &str,
    head_alias: &str,
) {
    write!(
        sql,
        " AND {successor_alias}.owner_kind = {head_alias}.owner_kind \
          AND {successor_alias}.owner_id IS NOT DISTINCT FROM {head_alias}.owner_id"
    )
    .expect("write to String is infallible");
}

pub(crate) fn read_owner_columns(
    read_owners: &[OwnerRef],
) -> (Vec<OwnerRefKind>, Vec<Option<uuid::Uuid>>) {
    crate::access::owner_columns::owner_arrays(read_owners)
}

pub(crate) fn read_owner_predicate(owner_alias: &str, read_set_alias: &str) -> String {
    format!(
        "{owner_alias}.owner_kind = {read_set_alias}.kind \
         AND {owner_alias}.owner_id IS NOT DISTINCT FROM {read_set_alias}.id"
    )
}

pub(crate) fn entity_owner_union() -> &'static str {
    "(SELECT memory_id AS entity_id, owner_kind, owner_id FROM proxima_core.memories \
      UNION ALL \
      SELECT goal_id AS entity_id, owner_kind, owner_id FROM proxima_core.goals)"
}

/// Resolve the aggregate `fact_entity_id` for an owner-scoped Fact
/// natural key inside an existing transaction.
///
/// # Errors
///
/// Returns `Internal` on sqlx failures.
pub async fn fact_entity_id_for(
    tx: &mut PgConnection,
    owner: &Owner,
    schema_id: &SchemaId,
    schema_version: SchemaVersion,
    natural_key: &[String],
) -> Result<Option<FactEntityId>, StorageError> {
    fact_entity_id_for_executor(tx, owner, schema_id, schema_version, natural_key).await
}

pub(crate) async fn fact_entity_id_for_pool(
    pool: &PgPool,
    owner: &Owner,
    schema_id: &SchemaId,
    schema_version: SchemaVersion,
    natural_key: &[String],
) -> Result<Option<FactEntityId>, StorageError> {
    fact_entity_id_for_executor(pool, owner, schema_id, schema_version, natural_key).await
}

async fn fact_entity_id_for_executor<'e, E>(
    executor: E,
    owner: &Owner,
    schema_id: &SchemaId,
    schema_version: SchemaVersion,
    natural_key: &[String],
) -> Result<Option<FactEntityId>, StorageError>
where
    E: Executor<'e, Database = Postgres>,
{
    let (owner_kind, owner_id) = owner.columns();
    let id = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT fact_entity_id
           FROM proxima_core.fact_entities
          WHERE owner_kind = $1
            AND owner_id = $2
            AND schema_id = $3
            AND schema_version = $4
            AND natural_key = $5::text[]",
    )
    .bind(owner_kind)
    .bind(owner_id)
    .bind(schema_id.as_str())
    .bind(schema_version.into_inner().cast_signed())
    .bind(natural_key)
    .fetch_optional(executor)
    .await
    .map_err(crate::error::map_err)?;
    Ok(id.map(FactEntityId::new))
}

pub(crate) async fn resolve_heads_by_fact_entity_id(
    pool: &PgPool,
    fact_entity_ids: &[uuid::Uuid],
) -> Result<HashMap<uuid::Uuid, uuid::Uuid>, StorageError> {
    if fact_entity_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = sqlx::query_as::<_, (uuid::Uuid, uuid::Uuid)>(
        "SELECT fact_entity_id, current_memory_id
           FROM proxima_core.fact_entities
          WHERE fact_entity_id = ANY($1::uuid[])",
    )
    .bind(fact_entity_ids)
    .fetch_all(pool)
    .await
    .map_err(crate::error::map_err)?;
    Ok(rows.into_iter().collect())
}
