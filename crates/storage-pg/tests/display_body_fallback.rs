//! #479 follow-up: a memory whose schema declares no display text field and
//! whose payload has no `body`/`text` key still has a body: its own text
//! (the persisted sketch). Search hydration and by-id reads both fall back
//! to it, so the two paths report the same body.
#![allow(clippy::missing_errors_doc)]

use proxima_core::read_models::MemorySchemaSpec;
use proxima_core::storage::MemoryGraphIdentity;
use proxima_core::storage_ports::{
    FactIngestPort, MemoryInspectPort, MemoryReadPort, OwnerWritePermit,
};
use proxima_core::test_fixtures::owner_scope_for_tests;
use proxima_core::verbs::fact_ingest::{AuthorizedFactWrite, FactWriteCommand};
use proxima_core::verbs::query::EntityKind;
use proxima_core::{AccessKind, OwnerId, SchemaId, SchemaVersion, UserId};
use proxima_storage_pg::test_fixtures::fresh_pg;
use uuid::Uuid;

/// A schema with no sidecar and no search declaration: nothing names a
/// body field and there is no payload to read `body`/`text` from.
const BARE_SCHEMA: &str = "core/test-fact-v1";

#[tokio::test]
async fn search_and_by_id_reads_fall_back_to_the_memorys_own_text()
-> Result<(), Box<dyn std::error::Error>> {
    let (pg, _db) = fresh_pg("proxima_display_body_fallback").await;
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let scope = owner_scope_for_tests(&[owner]);
    let schema_id = SchemaId::new(BARE_SCHEMA.into());
    let specs = vec![MemorySchemaSpec::new(
        EntityKind::Fact,
        schema_id.clone(),
        SchemaVersion::new(1),
        None,
    )];
    let draft = FactWriteCommand {
        schema_id: schema_id.clone(),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: Vec::new(),
        rendered_text: Some("fallbackneedle the memory's own text\nsecond line".into()),
        lexical_language: None,
        receipt: None,
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
        request: None,
    };
    let authorized = AuthorizedFactWrite::new_for_tests(
        OwnerWritePermit::new_for_tests(owner, AccessKind::Fact),
        draft,
        None,
        Vec::new(),
    );
    let id = pg
        .ingest_authorized_fact_atomic(&authorized, &[])
        .await?
        .memory_id;

    let sketch = pg
        .load_sketches(Some(&scope), &[owner], &[id])
        .await?
        .pop()
        .ok_or("the write persisted a sketch")?
        .text;
    assert_eq!(sketch, "fallbackneedle the memory's own text");

    let identity = MemoryGraphIdentity {
        memory_id: id,
        kind: EntityKind::Fact,
        schema_id,
    };
    let hit = pg
        .load_memory_graph_payloads(Some(&scope), std::slice::from_ref(&identity), &specs, true)
        .await?
        .pop()
        .ok_or("search hydration returns the admitted memory")?;
    let read = pg
        .load_memories_by_ids(Some(&scope), &[owner], &[id], &specs)
        .await?
        .pop()
        .ok_or("the by-id read returns the memory")?;

    assert_eq!(hit.body.as_deref(), Some(sketch.as_str()), "search body");
    assert_eq!(read.text.as_deref(), Some(sketch.as_str()), "by-id body");
    assert_eq!(hit.body, read.text, "search and by-id agree");

    let bodiless = pg
        .load_memory_graph_payloads(Some(&scope), &[identity], &specs, false)
        .await?
        .pop()
        .ok_or("search hydration without bodies")?;
    assert_eq!(bodiless.body, None, "no body unless asked for");
    Ok(())
}
