//! Unit smoke for Fact ingest types.

use proxima_core::verbs::fact_ingest::{
    Citation, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::{OwnerId, SchemaId, SchemaVersion, SourceId, UserId};
use uuid::Uuid;

fn fresh_command() -> FactWriteCommand {
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
        }),
        occurred_at: None,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new("test/cited_blob".to_string()),
                schema_version: SchemaVersion::new(1),
                content_hash: [0u8; 32],
            },
        }),
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

#[test]
fn receipt_id_is_deterministic_for_stamped_owner() {
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let command = fresh_command();
    let h1 = command.receipt_id_for_owner(owner);
    let h2 = command.receipt_id_for_owner(owner);
    assert_eq!(h1, h2);
}
