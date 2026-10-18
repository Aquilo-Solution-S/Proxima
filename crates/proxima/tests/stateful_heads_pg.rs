//! `Engine::query` with `SupersessionStatus::HeadsOnly` against a stateful
//! Fact schema returns the latest observation per natural-key tuple.
//!
//! The suite writes example notes, whose natural key is
//! `(notebook_id, key)`:
//! - 3 notes under `NK = (notebook, "a")`
//! - 1 note under `NK = (notebook, "b")`
//!
//! Heads-only must return 2 rows: the most recent of `NK_a` + `NK_b`.

#[path = "fixtures/example_engine.rs"]
#[allow(dead_code, reason = "each suite uses part of the shared fixture")]
mod example_engine;

use example_engine::{ExampleEngine, next_millisecond};
use proxima_core::verbs::fact_ingest::{
    Citation, CitedObjectHint, FactReceiptDraft, FactWriteCommand,
};
use proxima_core::verbs::query::{QueryRequest, SupersessionStatus};
use proxima_core::{
    AuthPath, AuthzContext, FactPayload, FactWrite, FlavorRegistry, MemoryId, OwnerId,
    PayloadKeyBuilder, SchemaId, SchemaVersion, SourceId, UserId,
};
use proxima_example::NoteV1;
use uuid::Uuid;

const TEST_CITED_BLOB_SCHEMA_ID: &str = "test/cited_blob";

fn make_owner() -> (OwnerId, AuthzContext) {
    let user = UserId::new(Uuid::now_v7());
    (
        OwnerId::from(user),
        proxima_core::test_fixtures::authenticated_context(AuthzContext::for_subject(
            user,
            AuthPath::HostBearer,
        )),
    )
}

fn register_test_schemas(registry: &mut FlavorRegistry) {
    registry.add_fact_schema_or_panic_for_tests::<StatelessFactV1>();
    registry.add_opaque_cited_object_schema(
        SchemaId::new(TEST_CITED_BLOB_SCHEMA_ID.into()),
        SchemaVersion::new(1),
    );
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StatelessFactV1 {
    value: String,
}

impl FactPayload for StatelessFactV1 {
    const SCHEMA_ID: &'static str = "test/stateless-fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("value", &self.value);
        key.finish()
    }

    fn render(&self) -> String {
        self.value.clone()
    }
}

/// A content hash distinct per payload; the cited object only has to name
/// one.
fn content_hash(payload: &[u8]) -> [u8; 32] {
    let mut hash = [0_u8; 32];
    for (slot, byte) in hash.iter_mut().zip(payload) {
        *slot = *byte;
    }
    hash
}

fn fresh_draft(schema: &str, payload: &[u8]) -> FactWriteCommand {
    FactWriteCommand {
        schema_id: SchemaId::new(schema.into()),
        schema_version: SchemaVersion::new(1),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: payload.to_vec(),
        rendered_text: None,
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/source"),
        }),
        occurred_at: None,
        citation: Some(Citation {
            object: CitedObjectHint {
                schema_id: SchemaId::new(TEST_CITED_BLOB_SCHEMA_ID.into()),
                schema_version: SchemaVersion::new(1),
                content_hash: content_hash(payload),
            },
        }),
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

/// Write one note through the Engine. A note with the natural key of an
/// earlier one joins that note's series at ingest.
async fn write_note(
    h: &ExampleEngine,
    authz: &AuthzContext,
    owner: OwnerId,
    notebook_id: Uuid,
    key: &str,
    body: &str,
) -> MemoryId {
    h.engine
        .ingest_fact(
            authz,
            FactWrite::new(
                owner,
                "stateful-heads",
                &NoteV1 {
                    notebook_id,
                    key: key.to_owned(),
                    body: body.to_owned(),
                    written_at: time::OffsetDateTime::now_utc(),
                },
            ),
        )
        .await
        .expect("the note is admitted")
        .memory_id
}

fn note_query(supersession: SupersessionStatus) -> QueryRequest {
    QueryRequest {
        entity_kind: None,
        schema_id: Some(SchemaId::new(NoteV1::SCHEMA_ID.into())),
        supersession,
        limit: 100,
        ..QueryRequest::readable()
    }
}

#[tokio::test]
async fn heads_only_returns_latest_per_natural_key() {
    let h = ExampleEngine::new("proxima_stateful_heads", register_test_schemas).await;
    let (owner, authz) = make_owner();
    let notebook = h.notebook(&owner).await;

    // 3 revisions of note a, each in a later millisecond.
    write_note(&h, &authz, owner, notebook, "a", "v1").await;
    next_millisecond().await;
    write_note(&h, &authz, owner, notebook, "a", "v2").await;
    next_millisecond().await;
    let r3 = write_note(&h, &authz, owner, notebook, "a", "v3").await;
    // 1 revision of note b.
    next_millisecond().await;
    let r_b = write_note(&h, &authz, owner, notebook, "b", "b1").await;

    // The note's registered NK columns put every revision of one key on
    // one handle at ingest, so the head scan needs no per-schema filter.
    let resp = h
        .engine
        .query(&authz, &note_query(SupersessionStatus::HeadsOnly))
        .await
        .expect("heads-only query");
    let ids: Vec<MemoryId> = resp.memories.iter().map(|m| m.id).collect();
    assert_eq!(ids.len(), 2, "expected 2 heads, got {ids:?}");
    assert!(ids.contains(&r3), "expected latest note a head ({r3:?})");
    assert!(ids.contains(&r_b), "expected note b head ({r_b:?})");

    // IncludeSuperseded — all 4 rows visible.
    let resp_all = h
        .engine
        .query(&authz, &note_query(SupersessionStatus::IncludeSuperseded))
        .await
        .expect("include-superseded query");
    assert_eq!(
        resp_all.memories.len(),
        4,
        "expected 4 rows with IncludeSuperseded"
    );
    h.pool.close().await;
}

#[tokio::test]
async fn heads_only_no_op_for_stateless_fact_schema() {
    // A schema with no NK columns: heads-only falls through to the
    // A/P-style supersedes scan, which (since Facts have no supersedes)
    // returns every row.
    let h = ExampleEngine::new("proxima_stateful_heads", register_test_schemas).await;
    let (_owner, authz) = make_owner();

    // Two distinct sidecarless Facts.
    for payload in [b"c1" as &[u8], b"c2"] {
        let draft = fresh_draft(StatelessFactV1::SCHEMA_ID, payload);
        h.engine
            .fact_ingest(&authz, draft)
            .await
            .expect("the stateless Fact is admitted");
    }
    let req = QueryRequest {
        entity_kind: None,
        schema_id: Some(SchemaId::new(StatelessFactV1::SCHEMA_ID.into())),
        supersession: SupersessionStatus::HeadsOnly,
        limit: 100,
        ..QueryRequest::readable()
    };
    let resp = h
        .engine
        .query(&authz, &req)
        .await
        .expect("heads-only query");
    assert_eq!(
        resp.memories.len(),
        2,
        "stateless Facts: every row is a head"
    );
    h.pool.close().await;
}

#[tokio::test]
async fn heads_only_supersedes_older_same_principal_nk_revision() {
    let h = ExampleEngine::new("proxima_stateful_heads", register_test_schemas).await;
    let (owner, authz) = make_owner();
    let notebook = h.notebook(&owner).await;

    let first = write_note(&h, &authz, owner, notebook, "shared", "rev-1").await;
    next_millisecond().await;
    let second = write_note(&h, &authz, owner, notebook, "shared", "rev-2").await;

    let resp = h
        .engine
        .query(&authz, &note_query(SupersessionStatus::HeadsOnly))
        .await
        .expect("heads-only query");
    let ids: Vec<MemoryId> = resp.memories.iter().map(|m| m.id).collect();
    // Same owner, same NK: only the newer revision is a head.
    assert!(
        !ids.contains(&first),
        "older same-handle revision is not the head"
    );
    assert!(
        ids.contains(&second),
        "newer same-handle head remains visible"
    );
    h.pool.close().await;
}

#[tokio::test]
async fn owner_snapshot_heads_only_folds_stateful_fact_schemas() {
    let h = ExampleEngine::new("proxima_stateful_heads", register_test_schemas).await;
    let (owner, authz) = make_owner();
    let notebook = h.notebook(&owner).await;

    let a_v1 = write_note(&h, &authz, owner, notebook, "a", "a1").await;
    next_millisecond().await;
    let a_v2 = write_note(&h, &authz, owner, notebook, "a", "a2").await;
    let mut req = QueryRequest::readable();
    req.limit = 100;
    let resp = h.engine.query(&authz, &req).await.expect("owner snapshot");
    let ids: Vec<MemoryId> = resp.memories.iter().map(|m| m.id).collect();
    assert!(!ids.contains(&a_v1));
    assert!(ids.contains(&a_v2));
    h.pool.close().await;
}
