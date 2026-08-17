use proxima_core::verbs::fact_ingest::{CitationSpec, FactIngestOutcome};
use proxima_core::{EdgeEndpoint, FactPayload, ToolError, TypedFactIngest, UnitOfWork};

use crate::ingest::{
    ACCEPTANCE_CRITERIA_OBJECT_SCHEMA, ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
    EXECUTION_REQUEST_OBJECT_SCHEMA, EXECUTION_REQUEST_WHOLE_SCHEMA, TEST_REQUEST_OBJECT_SCHEMA,
    TEST_REQUEST_WHOLE_SCHEMA,
};
use crate::payloads::{AcceptanceCriteriaV1, ExecutionRequestV1, TestRequestV1};

use super::{ACCEPTANCE_CRITERIA_SOURCE_ID, EXECUTION_REQUEST_SOURCE_ID, TEST_REQUEST_SOURCE_ID};

/// Provenance on a dispatch-boundary Fact write: origins become index rows
/// in the Fact's transaction.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct FactProvenance<'a> {
    pub(super) derived_from: &'a [EdgeEndpoint],
}

async fn ingest_mcp_fact<P>(
    uow: &mut UnitOfWork<'_>,
    source_id: &'static str,
    cited_object_schema: &'static str,
    mapping_schema: &'static str,
    payload: &P,
    provenance: FactProvenance<'_>,
) -> Result<FactIngestOutcome, ToolError>
where
    P: FactPayload + Clone,
{
    let citation = CitationSpec::v1_for_payload(cited_object_schema, payload, mapping_schema);
    uow.ingest_typed(
        TypedFactIngest::new(source_id, payload)
            .citation(citation)
            .derived_from(provenance.derived_from.iter().copied()),
    )
    .await
    .map_err(ToolError::Protocol)
}

pub(super) async fn ingest_execution_request(
    uow: &mut UnitOfWork<'_>,
    payload: &ExecutionRequestV1,
    provenance: FactProvenance<'_>,
) -> Result<FactIngestOutcome, ToolError> {
    ingest_mcp_fact(
        uow,
        EXECUTION_REQUEST_SOURCE_ID,
        EXECUTION_REQUEST_OBJECT_SCHEMA,
        EXECUTION_REQUEST_WHOLE_SCHEMA,
        payload,
        provenance,
    )
    .await
}

pub(super) async fn ingest_acceptance_criteria(
    uow: &mut UnitOfWork<'_>,
    payload: &AcceptanceCriteriaV1,
    provenance: FactProvenance<'_>,
) -> Result<FactIngestOutcome, ToolError> {
    ingest_mcp_fact(
        uow,
        ACCEPTANCE_CRITERIA_SOURCE_ID,
        ACCEPTANCE_CRITERIA_OBJECT_SCHEMA,
        ACCEPTANCE_CRITERIA_WHOLE_SCHEMA,
        payload,
        provenance,
    )
    .await
}

pub(super) async fn ingest_test_request(
    uow: &mut UnitOfWork<'_>,
    payload: &TestRequestV1,
    provenance: FactProvenance<'_>,
) -> Result<FactIngestOutcome, ToolError> {
    ingest_mcp_fact(
        uow,
        TEST_REQUEST_SOURCE_ID,
        TEST_REQUEST_OBJECT_SCHEMA,
        TEST_REQUEST_WHOLE_SCHEMA,
        payload,
        provenance,
    )
    .await
}
