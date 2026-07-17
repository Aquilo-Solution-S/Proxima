use proxima_core::read_models::SidecarSpec;
use proxima_core::verbs::query::{FactCitationCursor, FactCitationPage, FactCitationReadback};
use proxima_core::{FactEntityId, MemoryId, OwnerRef, SchemaId, StorageError};
use sqlx::PgPool;

use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::consolidate::load_memory_by_id;

use super::{entity_owner_union, read_owner_columns, read_owner_predicate};

pub(crate) async fn facts_citing_object(
    pool: &PgPool,
    pg_sidecars: &PgSidecarRegistryFrozen,
    read_owners: &[OwnerRef],
    cited_object_id: uuid::Uuid,
    sidecars: &[SidecarSpec],
    after: Option<FactCitationCursor>,
    limit: u32,
) -> Result<FactCitationPage, StorageError> {
    if read_owners.is_empty() {
        return Ok(FactCitationPage {
            facts: Vec::new(),
            next_cursor: None,
            has_more: false,
        });
    }
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);
    let sql = format!(
        "SELECT m.memory_id, m.created_at
           FROM proxima_core.memories m
           JOIN proxima_core.citation_mappings cm
             ON cm.memory_id = m.memory_id
           JOIN proxima_core.cited_objects co
             ON co.cited_object_id = cm.cited_object_id
          WHERE cm.cited_object_id = $1
            AND EXISTS (
                SELECT 1
                  FROM {entity_owner_union} eo
                  JOIN unnest($2::proxima_core.owner_ref_kind[], $3::uuid[]) AS s(kind, id)
                    ON {read_owner_predicate}
                 WHERE eo.entity_id = m.memory_id
            )
            AND m.kind IS NULL
            AND m.tombstoned_at IS NULL
            AND ($4::timestamptz IS NULL
                 OR (m.created_at, m.memory_id) < ($4::timestamptz, $5::uuid))
          ORDER BY m.created_at DESC, m.memory_id DESC
          LIMIT $6",
        entity_owner_union = entity_owner_union(),
        read_owner_predicate = read_owner_predicate("eo", "s"),
    );
    let after_created_at = after.map(|cursor| cursor.created_at);
    let after_memory_id = after.map(|cursor| cursor.memory_id.into_inner());
    let fetch = i64::from(limit).saturating_add(1);
    // SQL-POLICY: fixed-fragment
    let mut rows: Vec<(uuid::Uuid, time::OffsetDateTime)> =
        sqlx::query_as(sqlx::AssertSqlSafe(sql))
            .bind(cited_object_id)
            .bind(&read_owner_kinds)
            .bind(&read_owner_ids)
            .bind(after_created_at)
            .bind(after_memory_id)
            .bind(fetch)
            .fetch_all(pool)
            .await
            .map_err(map_err)?;

    let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
    let has_more = rows.len() > page_len;
    rows.truncate(page_len);
    let next_cursor = (has_more && !rows.is_empty()).then(|| {
        let (memory_id, created_at) = rows.last().expect("non-empty page");
        FactCitationCursor {
            created_at: *created_at,
            memory_id: MemoryId::new(*memory_id),
        }
    });

    let mut snapshots = Vec::with_capacity(rows.len());
    for (memory_id, _created_at) in rows {
        if let Some(snapshot) =
            load_memory_by_id(pool, pg_sidecars, MemoryId::new(memory_id), sidecars).await?
        {
            snapshots.push(snapshot);
        }
    }
    Ok(FactCitationPage {
        facts: snapshots,
        next_cursor,
        has_more,
    })
}

pub(crate) async fn citation_of_fact(
    pool: &PgPool,
    fact_memory_id: MemoryId,
) -> Result<Option<FactCitationReadback>, StorageError> {
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
            AND m.kind IS NULL
            AND m.tombstoned_at IS NULL",
    )
    .bind(fact_memory_id.into_inner())
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
    read_owners: &[OwnerRef],
    fact_entity_id: FactEntityId,
) -> Result<Option<FactCitationReadback>, StorageError> {
    if read_owners.is_empty() {
        return Ok(None);
    }
    let (read_owner_kinds, read_owner_ids) = read_owner_columns(read_owners);
    let fact_entity_uuid = fact_entity_id.into_inner();
    let sql = format!(
        "SELECT fe.current_memory_id
           FROM proxima_core.fact_entities fe
          WHERE fe.fact_entity_id = $1
            AND EXISTS (
                SELECT 1
                  FROM {entity_owner_union} eo
                  JOIN unnest($2::proxima_core.owner_ref_kind[], $3::uuid[]) AS s(kind, id)
                    ON {read_owner_predicate}
                 WHERE eo.entity_id = fe.current_memory_id
            )",
        entity_owner_union = entity_owner_union(),
        read_owner_predicate = read_owner_predicate("eo", "s"),
    );
    // SQL-POLICY: fixed-fragment
    let head = sqlx::query_scalar::<_, uuid::Uuid>(sqlx::AssertSqlSafe(sql))
        .bind(fact_entity_uuid)
        .bind(&read_owner_kinds)
        .bind(&read_owner_ids)
        .fetch_optional(pool)
        .await
        .map_err(map_err)?;
    let Some(head) = head else {
        return Ok(None);
    };
    citation_of_fact(pool, MemoryId::new(head)).await
}
