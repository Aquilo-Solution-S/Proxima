//! Atomic `persist_mcp_call` storage verb.

use proxima_core::verbs::persist_mcp_call::{
    MCP_CALL_CITATION_SCHEMA, MCP_CALL_FACT_SCHEMA, MCP_CALL_IO_SCHEMA, MCP_CALL_SOURCE_ID,
    McpCallLogInput, McpCallLogOutcome,
};
use proxima_core::{MemoryId, StorageError};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{internal, map_err};

/// Persist one MCP call log in a new transaction.
///
/// # Errors
///
/// Returns [`StorageError`] if any storage write fails.
pub async fn persist_mcp_call_atomic(
    pool: &PgPool,
    input: &McpCallLogInput,
) -> Result<McpCallLogOutcome, StorageError> {
    let mut tx = pool.begin().await.map_err(internal)?;
    let outcome = persist_mcp_call_in_tx(&mut tx, input).await?;
    tx.commit().await.map_err(map_err)?;
    Ok(outcome)
}

/// Persist one MCP call log using an existing transaction.
///
/// # Errors
///
/// Returns [`StorageError`] if any storage write fails.
#[allow(clippy::too_many_lines)]
pub async fn persist_mcp_call_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    input: &McpCallLogInput,
) -> Result<McpCallLogOutcome, StorageError> {
    crate::access::owner_columns::reject_world_write_owner(&input.owner)?;
    let io_content_hash = input.io_content_hash();
    let receipt_id = input.receipt_id();
    let receipt_id_bytes = receipt_id.into_inner();

    let (owner_kind, owner_id) = input.owner.columns();

    let existing = sqlx::query_as::<_, (Uuid, Uuid, Uuid)>(
        r"SELECT m.memory_id,
                  m.citation_mapping_id,
                  cm.cited_object_id
             FROM proxima_core.memories m
             JOIN proxima_core.citation_mappings cm
               ON cm.citation_mapping_id = m.citation_mapping_id
             WHERE m.receipt_id = $1
               AND m.tombstoned_at IS NULL",
    )
    .bind(&receipt_id_bytes[..])
    .fetch_optional(tx.as_mut())
    .await
    .map_err(map_err)?;

    if let Some((memory_id, citation_mapping_id, cited_object_id)) = existing {
        let seq = sqlx::query_scalar::<_, Uuid>(
            r"SELECT seq FROM proxima_core.change_event
                 WHERE entity_memory_id = $1 ORDER BY seq ASC LIMIT 1",
        )
        .bind(memory_id)
        .fetch_one(tx.as_mut())
        .await
        .map_err(map_err)?;

        return Ok(McpCallLogOutcome {
            receipt_id,
            fact_memory_id: MemoryId::new(memory_id),
            cited_object_id,
            citation_mapping_id,
            change_event_seq: seq,
            idempotent_replay: true,
        });
    }

    let cited_object_id = Uuid::now_v7();
    let citation_mapping_id = Uuid::now_v7();
    let memory_id = Uuid::now_v7();
    let source_batch_id = Uuid::now_v7();
    let change_seq = Uuid::now_v7();

    let cited_object_id = sqlx::query_scalar::<_, Uuid>(
        r"INSERT INTO proxima_core.cited_objects
            (cited_object_id, schema_id, owner_kind,
             owner_id, content_hash)
         VALUES ($1, $2, $3, $4, $5)
         ON CONFLICT (owner_kind, owner_id,
                      schema_id, content_hash)
         DO UPDATE SET schema_id = EXCLUDED.schema_id
         RETURNING cited_object_id",
    )
    .bind(cited_object_id)
    .bind(MCP_CALL_IO_SCHEMA)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(&io_content_hash[..])
    .fetch_one(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        r"INSERT INTO proxima_core.cited_mcp_call_io_v1
            (cited_object_id, byte_len, truncated, body)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (cited_object_id) DO NOTHING",
    )
    .bind(cited_object_id)
    .bind(i64::try_from(input.io_byte_len_original).unwrap_or(i64::MAX))
    .bind(input.io_truncated)
    .bind(&input.io_body[..])
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;
    sqlx::query(
        r"INSERT INTO proxima_core.source_batches
            (id, source_id, owner_kind,
             owner_id)
         VALUES ($1, $2, $3, $4)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(source_batch_id)
    .bind(MCP_CALL_SOURCE_ID)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        r"INSERT INTO proxima_core.fact_receipts
            (receipt_id, source, source_batch_id,
             owner_kind, owner_id,
             schema_id, schema_version, observed_at, occurred_at)
         VALUES ($1, $2, $3, $4, $5, $6, 1, $7, $8)",
    )
    .bind(&receipt_id_bytes[..])
    .bind(MCP_CALL_SOURCE_ID)
    .bind(source_batch_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(MCP_CALL_FACT_SCHEMA)
    .bind(input.observed_at)
    .bind(input.occurred_at)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        r"INSERT INTO proxima_core.memories
            (memory_id, owner_kind, owner_id, schema_id, schema_version,
             receipt_id, citation_mapping_id)
         VALUES ($1, $2, $3, $4, 1, $5, $6)",
    )
    .bind(memory_id)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(MCP_CALL_FACT_SCHEMA)
    .bind(&receipt_id_bytes[..])
    .bind(citation_mapping_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        r"INSERT INTO proxima_core.citation_mappings
            (citation_mapping_id, schema_id, memory_id,
             cited_object_id, owner_kind,
             owner_id)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(citation_mapping_id)
    .bind(MCP_CALL_CITATION_SCHEMA)
    .bind(memory_id)
    .bind(cited_object_id)
    .bind(owner_kind)
    .bind(owner_id)
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    // The mcp-call-io citation is a pure link — the citation_mappings row
    // above is the whole mapping. No sidecar table, no extra row.

    sqlx::query(
        r"INSERT INTO proxima_core.mcp_call_logged_v1
            (memory_id, tool_name, actor_oid, actor_upn, ok, error,
             latency_ms, io_byte_len, io_truncated, io_content_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(memory_id)
    .bind(input.tool_name.as_str())
    .bind(input.actor_oid.as_str())
    .bind(input.actor_upn.as_str())
    .bind(input.ok)
    .bind(input.error.as_deref())
    .bind(i64::from(input.latency_ms))
    .bind(i64::try_from(input.io_byte_len_original).unwrap_or(i64::MAX))
    .bind(input.io_truncated)
    .bind(&io_content_hash[..])
    .execute(tx.as_mut())
    .await
    .map_err(map_err)?;

    sqlx::query(
        r"INSERT INTO proxima_core.change_event
            (seq, owner_kind, owner_id,
             kind, entity_kind,
             entity_memory_id, entity_schema_id, entity_schema_version)
         VALUES ($1, $2, $3, 'EntityAppend', 'Fact', $4, $5, 1)",
    )
    .bind(change_seq)
    .bind(owner_kind)
    .bind(owner_id)
    .bind(memory_id)
    .bind(MCP_CALL_FACT_SCHEMA)
    .execute(&mut **tx)
    .await
    .map_err(map_err)?;

    Ok(McpCallLogOutcome {
        receipt_id,
        fact_memory_id: MemoryId::new(memory_id),
        cited_object_id,
        citation_mapping_id,
        change_event_seq: change_seq,
        idempotent_replay: false,
    })
}
