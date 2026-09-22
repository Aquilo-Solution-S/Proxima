use proxima_core::read_models::MemorySchemaSpec;
use proxima_core::verbs::query::{FactCitationCursor, FactCitationPage, FactCitationReadback};
use proxima_core::{MemoryId, OwnerRef, SchemaId, StorageError};
use sqlx::PgConnection;

use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::consolidate::load_memories_by_ids_on_connection;

pub(crate) async fn facts_citing_object_on_connection(
    connection: &mut PgConnection,
    pg_sidecars: &PgSidecarRegistryFrozen,
    read_owners: &[OwnerRef],
    cited_object_id: uuid::Uuid,
    schemas: &[MemorySchemaSpec],
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
    let fetch = i64::from(limit).saturating_add(1);
    let rows: Vec<(uuid::Uuid, time::OffsetDateTime)> = sqlx::query_as("SELECT m.t, COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01') FROM proxima_core.memory m WHERE m.blob_id = $1 AND m.owner_id = ANY($2::uuid[]) AND m.kind = 'fact' AND ($3::timestamptz IS NULL OR (COALESCE(uuid_extract_timestamp(m.t), TIMESTAMPTZ '1970-01-01'), m.t) < ($3::timestamptz, $4::uuid)) ORDER BY 2 DESC, m.t DESC LIMIT $5")
        .bind(cited_object_id).bind(&owner_ids).bind(after.map(|c| c.created_at)).bind(after.map(|c| c.memory_id.into_inner())).bind(fetch)
        .fetch_all(&mut *connection).await.map_err(map_err)?;
    let page_len = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut rows = rows;
    let has_more = rows.len() > page_len;
    rows.truncate(page_len);
    let next_cursor = (has_more && !rows.is_empty()).then(|| {
        let (id, at) = rows.last().expect("non-empty page");
        FactCitationCursor {
            created_at: *at,
            memory_id: MemoryId::new(*id),
        }
    });
    let ids: Vec<MemoryId> = rows.iter().map(|(id, _)| MemoryId::new(*id)).collect();
    let loaded =
        load_memories_by_ids_on_connection(connection, pg_sidecars, read_owners, &ids, schemas)
            .await?;
    Ok(FactCitationPage {
        facts: loaded,
        next_cursor,
        has_more,
    })
}

pub(crate) async fn citation_of_fact_on_connection(
    connection: &mut PgConnection,
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
        "SELECT m.blob_id, b.schema_id FROM proxima_core.memory m JOIN proxima_core.blob b ON b.blob_id = m.blob_id WHERE m.t = $1 AND m.kind = 'fact' AND m.blob_id IS NOT NULL AND m.owner_id = ANY($2::uuid[])",
    )
    .bind(fact_memory_id.into_inner()).bind(&owner_ids)
    .fetch_optional(&mut *connection).await.map_err(map_err)?;
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
