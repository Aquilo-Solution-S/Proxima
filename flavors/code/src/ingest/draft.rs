use proxima_core::verbs::event_ingest::{
    Citation as EventCitation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::{Owner, SchemaId, SchemaVersion, SourceBatchId, SourceId};

use super::IngestError;
use super::schemas::LOCAL_GIT_SOURCE_ID;

/// Per-fact-type citation triple: which artefact schema, which content
/// hash deduplicates the artefact within Owner, and which annotation
/// schema labels the linkage. v1 holds schema-version at 1 across the
/// flavor.
#[derive(Clone, Copy)]
pub(super) struct Citation {
    pub(super) cited_object_schema: &'static str,
    pub(super) content_hash: [u8; 32],
    pub(super) mapping_schema: &'static str,
}

pub(super) fn make_draft<P: serde::Serialize>(
    owner: &Owner,
    source_batch_id: SourceBatchId,
    payload: &P,
    schema_id: &str,
    citation: Citation,
    observed_at: time::OffsetDateTime,
) -> Result<EventDraft, IngestError> {
    let mut bytes = Vec::new();
    ciborium::ser::into_writer(payload, &mut bytes)
        .map_err(|e| IngestError::Serialize(e.to_string()))?;
    Ok(EventDraft {
        source_id: SourceId::new(LOCAL_GIT_SOURCE_ID),
        source_batch_id,
        principal: owner.principal.clone(),
        org_id: Some(owner.org_id),
        author_personality_instance_id: None,
        schema_id: SchemaId::new(schema_id.into()),
        schema_version: SchemaVersion::new(1),
        payload: bytes,
        observed_at,
        occurred_at: observed_at,
        citation: Some(EventCitation {
            object: CitedObjectHint {
                schema_id: SchemaId::new(citation.cited_object_schema.into()),
                schema_version: SchemaVersion::new(1),
                content_hash: citation.content_hash,
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new(citation.mapping_schema.into()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    })
}
