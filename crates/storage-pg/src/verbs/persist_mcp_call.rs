//! Persist one MCP call as a timeseries Fact. Replay key is the receipt hash.

use proxima_core::storage_ports::OwnerWritePermit;
use proxima_core::verbs::fact_ingest::FactWriteCommand;
use proxima_core::verbs::persist_mcp_call::{
    MCP_CALL_FACT_SCHEMA, MCP_CALL_SOURCE_ID, McpCallLogInput, McpCallLogOutcome,
};
use proxima_core::{FactPayload, SchemaId, SchemaVersion, StorageError};
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use crate::error::{internal, map_err, with_bounded_retry};
use crate::verbs::memory_timeseries::ingest_fact_timeseries;

/// Persist one MCP call log in a new transaction.
///
/// # Errors
///
/// Returns [`StorageError`] if any storage write fails.
pub async fn persist_mcp_call_atomic(
    pool: &PgPool,
    permit: &OwnerWritePermit,
    input: &McpCallLogInput,
) -> Result<McpCallLogOutcome, StorageError> {
    with_bounded_retry(move || async move {
        let mut tx = pool.begin().await.map_err(internal)?;
        let outcome = persist_mcp_call_in_tx(&mut tx, permit, input).await?;
        tx.commit().await.map_err(map_err)?;
        Ok(outcome)
    })
    .await
}

/// Persist one MCP call log using an existing transaction.
///
/// # Errors
///
/// Returns [`StorageError`] if any storage write fails.
pub async fn persist_mcp_call_in_tx(
    tx: &mut Transaction<'_, Postgres>,
    permit: &OwnerWritePermit,
    input: &McpCallLogInput,
) -> Result<McpCallLogOutcome, StorageError> {
    let mut stamped = input.clone();
    stamped.owner = *permit.owner();
    let input = &stamped;
    crate::access::owner_columns::reject_world_write_owner(&input.owner)?;
    let receipt_id = input.receipt_id();
    let ingest_key =
        receipt_id
            .into_inner()
            .iter()
            .fold(String::with_capacity(64), |mut acc, byte| {
                use std::fmt::Write as _;
                let _ = write!(acc, "{byte:02x}");
                acc
            });
    let draft = FactWriteCommand {
        schema_id: SchemaId::new(MCP_CALL_FACT_SCHEMA.to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: Some(MCP_CALL_SOURCE_ID.to_string()),
        ingest_key: Some(ingest_key),
        payload: input.payload().receipt_key(),
        rendered_text: Some(input.tool_name.clone()),
        lexical_language: None,
        receipt: None,
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    };
    let outcome = ingest_fact_timeseries(tx, &input.owner, &draft).await?;
    Ok(McpCallLogOutcome {
        receipt_id,
        fact_memory_id: outcome.memory_id,
        cited_object_id: Uuid::nil(),
        citation_mapping_id: Uuid::nil(),
        change_event_seq: outcome.change_event_seq,
        idempotent_replay: outcome.idempotent_replay,
    })
}
