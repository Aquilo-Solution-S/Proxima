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

use proxima_core::{
    FactEntityId, Owner, OwnerPrincipalKind, Principal, SchemaId, SchemaVersion, StorageError,
};
use sqlx::{Executor, PgConnection, PgPool, Postgres};

mod citations;
mod edges;
mod goals;
mod lineage;
mod memories;
mod rows;
mod search;

pub(crate) use citations::{citation_of_entity_head, citation_of_fact, facts_citing_object};
pub use edges::MAX_SNAPSHOT_EDGES;
pub(crate) use edges::{edge_exists, read_edges};
pub(crate) use lineage::walk_memory_lineage;
pub(crate) use memories::query_memories;
pub(crate) use search::search_memories;

pub(crate) fn read_owner_columns(
    read_owners: &[Principal],
) -> (Vec<OwnerPrincipalKind>, Vec<uuid::Uuid>) {
    let kinds = read_owners
        .iter()
        .map(|principal| principal.columns().0)
        .collect();
    let ids = read_owners
        .iter()
        .map(|principal| principal.columns().1)
        .collect();
    (kinds, ids)
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
    let (owner_kind, owner_principal_id) = owner.columns();
    let id = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT fact_entity_id
           FROM proxima_core.fact_entities
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND schema_id = $3
            AND schema_version = $4
            AND natural_key = $5::text[]",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
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
