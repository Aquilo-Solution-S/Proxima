use proxima_core::verbs::fact_ingest::{CitationSpec, FactIngestOutcome};
use proxima_core::{FactPayload, SourceBatchId, ToolCtx, ToolError};
use proxima_storage_pg::sidecars::PgMemorySidecar;
use proxima_storage_pg::verbs::fact_ingest::{FactIngestContext, ingest_fact_with_sidecar};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

use crate::ingest::{
    ACCEPTANCE_CRITERIA_OBJECT_SCHEMA, ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
    EXECUTION_REQUEST_OBJECT_SCHEMA, EXECUTION_REQUEST_WHOLE_SCHEMA, TEST_REQUEST_OBJECT_SCHEMA,
    TEST_REQUEST_WHOLE_SCHEMA,
};
use crate::payloads::{AcceptanceCriteriaV1, ExecutionRequestV1, TestRequestV1};

use super::{ACCEPTANCE_CRITERIA_SOURCE_ID, EXECUTION_REQUEST_SOURCE_ID, TEST_REQUEST_SOURCE_ID};

async fn ingest_mcp_fact<P>(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    source_id: &'static str,
    cited_object_schema: &'static str,
    mapping_schema: &'static str,
    payload: &P,
) -> Result<FactIngestOutcome, ToolError>
where
    P: FactPayload + PgMemorySidecar + Clone,
{
    let embedding_client = ctx.engine().and_then(|engine| engine.embed_client());
    let owner = ctx.owner();
    let ingest_ctx = FactIngestContext::new(&owner, source_id, SourceBatchId::new(Uuid::now_v7()))
        .embedding_model_id(embedding_client.as_ref().map(|client| client.model_id()));
    let citation = CitationSpec::v1_for_payload(cited_object_schema, payload, mapping_schema);
    ingest_fact_with_sidecar(tx, &ingest_ctx, payload, citation)
        .await
        .map_err(ToolError::Storage)
}

pub(super) async fn ingest_execution_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    payload: &ExecutionRequestV1,
) -> Result<FactIngestOutcome, ToolError> {
    ingest_mcp_fact(
        tx,
        ctx,
        EXECUTION_REQUEST_SOURCE_ID,
        EXECUTION_REQUEST_OBJECT_SCHEMA,
        EXECUTION_REQUEST_WHOLE_SCHEMA,
        payload,
    )
    .await
}

pub(super) async fn ingest_acceptance_criteria(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    payload: &AcceptanceCriteriaV1,
) -> Result<FactIngestOutcome, ToolError> {
    ingest_mcp_fact(
        tx,
        ctx,
        ACCEPTANCE_CRITERIA_SOURCE_ID,
        ACCEPTANCE_CRITERIA_OBJECT_SCHEMA,
        ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
        payload,
    )
    .await
}

pub(super) async fn ingest_test_request(
    tx: &mut Transaction<'_, Postgres>,
    ctx: &ToolCtx,
    payload: &TestRequestV1,
) -> Result<FactIngestOutcome, ToolError> {
    ingest_mcp_fact(
        tx,
        ctx,
        TEST_REQUEST_SOURCE_ID,
        TEST_REQUEST_OBJECT_SCHEMA,
        TEST_REQUEST_WHOLE_SCHEMA,
        payload,
    )
    .await
}
