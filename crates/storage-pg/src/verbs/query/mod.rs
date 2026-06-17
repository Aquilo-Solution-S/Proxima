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
//! Payload projection: for each schema with a sidecar table, we
//! LEFT JOIN the sidecar, project the row into a typed JSON value,
//! then encode the wire payload as canonical JSON bytes.

use proxima_core::{FactEntityId, Owner, SchemaId, SchemaVersion, StorageError};
use sqlx::PgPool;

mod citations;
mod edges;
mod goals;
mod lineage;
mod memories;
mod rows;
mod search;

pub(crate) use citations::{citation_of_fact, facts_citing_object};
pub use edges::MAX_SNAPSHOT_EDGES;
pub(crate) use lineage::walk_memory_lineage;
pub(crate) use memories::query_memories;
pub(crate) use search::search_memories;

pub(crate) async fn fact_entity_id_for(
    pool: &PgPool,
    owner: &Owner,
    schema_id: &SchemaId,
    schema_version: SchemaVersion,
    natural_key: &serde_json::Value,
) -> Result<Option<FactEntityId>, StorageError> {
    let (owner_kind, owner_principal_id, owner_org_id) = owner.columns();
    let id = sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT fact_entity_id
           FROM proxima_core.fact_entities
          WHERE owner_principal_kind = $1
            AND owner_principal_id = $2
            AND owner_org_id = $3
            AND schema_id = $4
            AND schema_version = $5
            AND natural_key = $6",
    )
    .bind(owner_kind)
    .bind(owner_principal_id)
    .bind(owner_org_id)
    .bind(schema_id.as_str())
    .bind(schema_version.into_inner().cast_signed())
    .bind(natural_key)
    .fetch_optional(pool)
    .await
    .map_err(crate::error::map_err)?;
    Ok(id.map(FactEntityId::new))
}
