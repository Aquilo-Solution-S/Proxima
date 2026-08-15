//! Unit smoke for Fact ingest types.

use proxima_core::verbs::fact_ingest::{
    Citation, CitationMappingHint, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::{OwnerRef, SchemaId, SchemaVersion, SourceBatchId, SourceId, UserId};
use uuid::Uuid;

fn fresh_command() -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: SchemaId::new("test/fact_blob".to_string()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: b"hello".to_vec(),
        rendered_text: None,
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
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
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

#[test]
fn receipt_id_is_deterministic_for_stamped_owner() {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let command = fresh_command();
    let h1 = command.receipt_id_for_owner(owner);
    let h2 = command.receipt_id_for_owner(owner);
    assert_eq!(h1, h2);
}

#[test]
fn receipt_id_changes_with_payload() {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let mut command = fresh_command();
    let h1 = command.receipt_id_for_owner(owner);
    command.payload = b"different".to_vec();
    let h2 = command.receipt_id_for_owner(owner);
    assert_ne!(h1, h2);
}

#[test]
fn receipt_id_ignores_citation() {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let mut command = fresh_command();
    let h1 = command.receipt_id_for_owner(owner);
    command.citation = None;
    let h2 = command.receipt_id_for_owner(owner);
    assert_eq!(h1, h2);
}

#[test]
fn receiptless_fact_write_has_no_receipt_id() {
    let owner = OwnerRef::Personal(UserId::new(Uuid::now_v7()));
    let mut command = fresh_command();
    command.receipt = None;
    assert_eq!(command.receipt_id_for_owner(owner), None);
}
