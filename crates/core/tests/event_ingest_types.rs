//! Unit smoke for `EventIngest` types.

use proxima_core::verbs::event_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, EventDraft,
};
use proxima_core::{OwnerRef, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId};
use uuid::Uuid;

fn fresh_draft() -> EventDraft {
    let user = UserId::new(Uuid::now_v7());
    let now = time::OffsetDateTime::now_utc();
    EventDraft {
        source_id: SourceId::new("test/source"),
        source_batch_id: SourceBatchId::new(Uuid::now_v7()),
        principal: OwnerRef::Personal(user),
        author_personality_instance_id: None,
        schema_id: SchemaId::new("test/fact_blob".to_string()),
        schema_version: SchemaVersion::new(1),
        payload: b"hello".to_vec(),
        rendered_text: None,
        observed_at: now,
        occurred_at: now,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("test/cited_blob".to_string()),
                schema_version: SchemaVersion::new(1),
                content_hash: [0u8; 32],
            },
            mapping: CitationMappingHint {
                schema_id: SchemaId::new("test/citation_blob".to_string()),
                schema_version: SchemaVersion::new(1),
            },
        }),
    }
}

#[test]
fn event_id_is_deterministic() {
    let draft = fresh_draft();
    let h1 = draft.event_id();
    let h2 = draft.event_id();
    assert_eq!(h1, h2);
}

#[test]
fn event_id_changes_with_payload() {
    let mut draft = fresh_draft();
    let h1 = draft.event_id();
    draft.payload = b"different".to_vec();
    let h2 = draft.event_id();
    assert_ne!(h1, h2);
}

#[test]
fn event_id_ignores_citation() {
    let mut draft = fresh_draft();
    let h1 = draft.event_id();
    draft.citation = None;
    let h2 = draft.event_id();
    assert_eq!(h1, h2);
}
