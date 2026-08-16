use proxima_core::UploadedBlobPageSpanV1;
use proxima_core::read_models::SidecarSpec;
use proxima_core::verbs::query::{
    FactCitationCursor, FactCitationPage, FactCitationReadback, UploadedBlobRef,
};
use proxima_core::{MemoryId, OwnerRef, SchemaId, StorageError};
use sqlx::{PgPool, Row};

use crate::error::map_err;
use crate::sidecars::PgSidecarRegistryFrozen;
use crate::verbs::consolidate::load_memory_by_id;

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
        "SELECT m.blob_id AS citation_mapping_id,
                b.schema_id AS mapping_schema_id,
                m.blob_id AS cited_object_id,
                b.schema_id AS cited_object_schema_id,
                NULL::int AS page_from, NULL::int AS page_to,
                NULL::int AS char_range_start, NULL::int AS char_range_end,
                NULL::text AS filename, NULL::text AS mime, NULL::bigint AS byte_len,
                NULL::text AS sha256_hex,
                NULL::timestamptz AS uploaded_at
           FROM proxima_core.memory m
           JOIN proxima_core.blob b ON b.blob_id = m.blob_id
          WHERE m.t = $1
            AND m.kind = 'fact'
            AND m.blob_id IS NOT NULL",
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
