use proxima_core::read_models::SidecarSpec;
use proxima_core::verbs::query::{FactCitationCursor, FactCitationPage, FactCitationReadback};
use proxima_core::{MemoryId, OwnerRef, SchemaId, StorageError};
use sqlx::PgPool;

use std::collections::HashMap;

use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::consolidate::load_memories_by_ids;

use super::read_owner_columns;

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
    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(OwnerRef::stored_owner_id)
        .collect();
    let _ = read_owner_columns(read_owners);
    let after_created_at = after.map(|cursor| cursor.created_at);
    let after_memory_id = after.map(|cursor| cursor.memory_id.into_inner());
    let fetch = i64::from(limit).saturating_add(1);
    let mut rows: Vec<(uuid::Uuid, time::OffsetDateTime)> = sqlx::query_as(
        "SELECT m.t,
                COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01')
           FROM proxima_core.memory m
          WHERE m.blob_id = $1
            AND m.owner_id = ANY($2::uuid[])
            AND m.kind = 'fact'
            AND ($3::timestamptz IS NULL
                 OR (COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01'), m.t)
                    < ($3::timestamptz, $4::uuid))
          ORDER BY 2 DESC, m.t DESC
          LIMIT $5",
    )
    .bind(cited_object_id)
    .bind(&owner_ids)
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

    let ids: Vec<MemoryId> = rows
        .iter()
        .map(|(memory_id, _)| MemoryId::new(*memory_id))
        .collect();
    let loaded = load_memories_by_ids(pool, pg_sidecars, read_owners, &ids, sidecars).await?;
    let mut by_id: HashMap<MemoryId, _> = loaded
        .into_iter()
        .map(|snapshot| (snapshot.memory_id, snapshot))
        .collect();
    let snapshots = ids.into_iter().filter_map(|id| by_id.remove(&id)).collect();
    Ok(FactCitationPage {
        facts: snapshots,
        next_cursor,
        has_more,
    })
}

pub(crate) async fn citation_of_fact(
    pool: &PgPool,
    read_owners: &[proxima_core::OwnerRef],
    fact_memory_id: MemoryId,
) -> Result<Option<FactCitationReadback>, StorageError> {
    if read_owners.is_empty() {
        return Ok(None);
    }
    let owner_ids: Vec<uuid::Uuid> = read_owners
        .iter()
        .copied()
        .map(proxima_core::OwnerRef::stored_owner_id)
        .collect();
    let row: Option<(uuid::Uuid, String)> = sqlx::query_as(
        "SELECT m.blob_id, b.schema_id
           FROM proxima_core.memory m
           JOIN proxima_core.blob b ON b.blob_id = m.blob_id
          WHERE m.t = $1
            AND m.kind = 'fact'
            AND m.blob_id IS NOT NULL
            AND m.owner_id = ANY($2::uuid[])",
    )
    .bind(fact_memory_id.into_inner())
    .bind(&owner_ids)
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    Ok(row.map(|(blob_id, schema_id)| {
        let schema_id = SchemaId::new(schema_id);
        FactCitationReadback {
            citation_mapping_id: blob_id,
            mapping_schema_id: schema_id.clone(),
            cited_object_id: blob_id,
            cited_object_schema_id: schema_id,
            page_span: None,
            uploaded_blob: None,
        }
    }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn citation_sql_does_not_select_dropped_mapping_columns() {
        let src = include_str!("citations.rs");
        let needle = format!("{}{}", "NULL::int AS ", "page_from");
        assert!(
            !src.contains(&needle),
            "v008 has no citation_mappings table; do not fabricate mapping columns"
        );
    }
}
