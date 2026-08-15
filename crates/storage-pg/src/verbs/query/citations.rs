use proxima_core::UploadedBlobPageSpanV1;
use proxima_core::read_models::SidecarSpec;
use proxima_core::verbs::query::{
    FactCitationCursor, FactCitationPage, FactCitationReadback, UploadedBlobRef,
};
use proxima_core::{FactEntityId, MemoryId, OwnerRef, SchemaId, StorageError};
use sqlx::{PgPool, Row};

use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::consolidate::load_memory_by_id;

use super::{read_owner_columns, read_owner_predicate};

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
                  FROM unnest($2::proxima_core.owner_ref_kind[], $3::uuid[]) AS s(kind, id)
                 WHERE {read_owner_predicate}
            )
            AND m.kind IS NULL
            AND m.tombstoned_at IS NULL
            AND ($4::timestamptz IS NULL
                 OR (m.created_at, m.memory_id) < ($4::timestamptz, $5::uuid))
          ORDER BY m.created_at DESC, m.memory_id DESC
          LIMIT $6",
        read_owner_predicate = read_owner_predicate("m", "s"),
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
    let row = sqlx::query(
        "SELECT cm.citation_mapping_id,
                cm.schema_id AS mapping_schema_id,
                co.cited_object_id,
                co.schema_id AS cited_object_schema_id,
                ps.page_from, ps.page_to,
                ps.char_range_start, ps.char_range_end,
                b.filename, b.mime, b.byte_len,
                encode(b.sha256, 'hex') AS sha256_hex,
                b.uploaded_at
           FROM proxima_core.memories m
           JOIN proxima_core.citation_mappings cm
             ON cm.memory_id = m.memory_id
           JOIN proxima_core.cited_objects co
             ON co.cited_object_id = cm.cited_object_id
           LEFT JOIN proxima_core.citation_uploaded_blob_page_span_v1 ps
             ON ps.citation_mapping_id = cm.citation_mapping_id
           LEFT JOIN proxima_core.cited_uploaded_blob_v1 b
             ON b.cited_object_id = co.cited_object_id
          WHERE m.memory_id = $1
            AND m.kind IS NULL
            AND m.tombstoned_at IS NULL",
    )
    .bind(fact_memory_id.into_inner())
    .fetch_optional(pool)
    .await
    .map_err(map_err)?;

    row.map(|row| {
        let page_span = match (
            row.get::<Option<i32>, _>("page_from"),
            row.get::<Option<i32>, _>("page_to"),
        ) {
            (Some(page_from), Some(page_to)) => Some(UploadedBlobPageSpanV1 {
                page_from: page_u32(page_from)?,
                page_to: page_u32(page_to)?,
                char_range_start: row
                    .get::<Option<i32>, _>("char_range_start")
                    .map(page_u32)
                    .transpose()?,
                char_range_end: row
                    .get::<Option<i32>, _>("char_range_end")
                    .map(page_u32)
                    .transpose()?,
            }),
            _ => None,
        };
        // The blob sidecar row is keyed by cited_object_id with every
        // column NOT NULL, so a present filename implies the whole row.
        let uploaded_blob = row
            .get::<Option<String>, _>("filename")
            .map(|filename| -> Result<UploadedBlobRef, StorageError> {
                let byte_len: i64 = row.get("byte_len");
                Ok(UploadedBlobRef {
                    filename,
                    mime: row.get("mime"),
                    byte_len: u64::try_from(byte_len).map_err(|_| {
                        StorageError::Internal(format!("negative blob byte_len {byte_len}"))
                    })?,
                    sha256_hex: row.get("sha256_hex"),
                    uploaded_at: row.get("uploaded_at"),
                })
            })
            .transpose()?;
        Ok(FactCitationReadback {
            citation_mapping_id: row.get("citation_mapping_id"),
            mapping_schema_id: SchemaId::new(row.get::<String, _>("mapping_schema_id")),
            cited_object_id: row.get("cited_object_id"),
            cited_object_schema_id: SchemaId::new(row.get::<String, _>("cited_object_schema_id")),
            page_span,
            uploaded_blob,
        })
    })
    .transpose()
}

/// Convert a page/char-range column to the wire's `u32`. The sidecar's
/// `CHECK` constraints keep these non-negative; a violation here means
/// the row bypassed them and is an internal fault, not caller input.
fn page_u32(value: i32) -> Result<u32, StorageError> {
    u32::try_from(value)
        .map_err(|_| StorageError::Internal(format!("negative page-span column {value}")))
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
                  FROM proxima_core.memories hm
                  JOIN unnest($2::proxima_core.owner_ref_kind[], $3::uuid[]) AS s(kind, id)
                    ON {read_owner_predicate}
                 WHERE hm.memory_id = fe.current_memory_id
            )",
        read_owner_predicate = read_owner_predicate("hm", "s"),
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
