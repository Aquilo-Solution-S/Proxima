use proxima_core::personality::{MemorySnapshot, SidecarSpec};
use proxima_core::verbs::query::FactCitationReadback;
use proxima_core::{FactEntityId, MemoryId, Owner, SchemaId, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::consolidate::load_memory_by_id;
use crate::verbs::query::resolve_head;

pub(crate) async fn facts_citing_object(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    owner: &Owner,
    cited_object_id: uuid::Uuid,
    sidecars: &[SidecarSpec],
) -> Result<Vec<MemorySnapshot>, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let memory_ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT m.memory_id
           FROM proxima_core.memories m
           JOIN proxima_core.citation_mappings cm
             ON cm.memory_id = m.memory_id
           JOIN proxima_core.cited_objects co
             ON co.cited_object_id = cm.cited_object_id
          WHERE cm.cited_object_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND cm.owner_principal_kind = $2
            AND cm.owner_principal_id = $3
            AND co.owner_principal_kind = $2
            AND co.owner_principal_id = $3
            AND m.kind IS NULL
            AND m.tombstoned_at IS NULL
          ORDER BY m.created_at DESC, m.memory_id DESC",
    )
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_all(pool)
    .await
    .map_err(map_err)?;

    let mut snapshots = Vec::with_capacity(memory_ids.len());
    for memory_id in memory_ids {
        if let Some(snapshot) = load_memory_by_id(
            pool,
            pg_sidecars,
            owner,
            MemoryId::new(memory_id),
            None,
            sidecars,
        )
        .await?
        {
            snapshots.push(snapshot);
        }
    }
    Ok(snapshots)
}

pub(crate) async fn citation_of_fact(
    pool: &PgPool,
    owner: &Owner,
    fact_memory_id: MemoryId,
) -> Result<Option<FactCitationReadback>, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let row: Option<(uuid::Uuid, String, uuid::Uuid, String)> = sqlx::query_as(
        "SELECT cm.citation_mapping_id,
                cm.schema_id AS mapping_schema_id,
                co.cited_object_id,
                co.schema_id AS cited_object_schema_id
           FROM proxima_core.memories m
           JOIN proxima_core.citation_mappings cm
             ON cm.memory_id = m.memory_id
           JOIN proxima_core.cited_objects co
             ON co.cited_object_id = cm.cited_object_id
          WHERE m.memory_id = $1
            AND m.owner_principal_kind = $2
            AND m.owner_principal_id = $3
            AND cm.owner_principal_kind = $2
            AND cm.owner_principal_id = $3
            AND co.owner_principal_kind = $2
            AND co.owner_principal_id = $3
            AND m.kind IS NULL
            AND m.tombstoned_at IS NULL",
    )
    .bind(fact_memory_id.into_inner())
    .bind(owner_kind)
    .bind(owner_principal_id)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.map(
        |(citation_mapping_id, mapping_schema_id, cited_object_id, cited_object_schema_id)| {
            FactCitationReadback {
                citation_mapping_id,
                mapping_schema_id: SchemaId::new(mapping_schema_id),
                cited_object_id,
                cited_object_schema_id: SchemaId::new(cited_object_schema_id),
            }
        },
    ))
}

pub(crate) async fn citation_of_entity_head(
    pool: &PgPool,
    owner: &Owner,
    fact_entity_id: FactEntityId,
) -> Result<Option<FactCitationReadback>, StorageError> {
    let (owner_kind, owner_principal_id) = owner.columns();
    let fact_entity_uuid = fact_entity_id.into_inner();
    let heads = resolve_head(pool, owner_kind, owner_principal_id, &[fact_entity_uuid]).await?;
    let Some(head) = heads.get(&fact_entity_uuid).copied() else {
        return Ok(None);
    };
    citation_of_fact(pool, owner, MemoryId::new(head)).await
}
