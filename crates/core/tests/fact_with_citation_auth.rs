use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::verbs::fact_ingest::{
    FactReceiptDraft, FactWriteCommand, InlineCitationMappingDraft, InlineCitedObjectDraft,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthPath, AuthzContext, CitationMappingPayload, CitedObjectPayload, EntityKind, FactPayload,
    FlavorRegistry, Owner, OwnerRef, PayloadKeyBuilder, Relation, SchemaId, SchemaVersion,
    SourceBatchId, SourceId, UserId, canonical_json_bytes,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
struct TestFact {
    value: String,
}

impl FactPayload for TestFact {
    const SCHEMA_ID: &'static str = "test/fact";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("value", &self.value);
        key.finish()
    }

    fn render(&self) -> String {
        self.value.clone()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("test.fact_v1")
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TestCitedObject {
    body: String,
}

impl CitedObjectPayload for TestCitedObject {
    const SCHEMA_ID: &'static str = "test/cited-object";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "test.cited_object_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        *blake3::hash(self.body.as_bytes()).as_bytes()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TestCitationMapping {
    byte_start: u32,
    byte_end: u32,
}

impl CitationMappingPayload for TestCitationMapping {
    const SCHEMA_ID: &'static str = "test/citation-mapping";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("test.citation_mapping_v1")
    }

    fn cited_object_schema() -> SchemaId {
        TestCitedObject::schema_id()
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct MismatchedCitationMapping {
    byte_start: u32,
    byte_end: u32,
}

impl CitationMappingPayload for MismatchedCitationMapping {
    const SCHEMA_ID: &'static str = "test/mismatched-citation-mapping";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("test.mismatched_citation_mapping_v1")
    }

    fn cited_object_schema() -> SchemaId {
        SchemaId::new("test/other-cited-object".to_string())
    }
}

fn json<T: Serialize>(value: &T) -> Vec<u8> {
    let value = serde_json::to_value(value).expect("test payload serializes as JSON");
    canonical_json_bytes(&value)
}

fn owner() -> Owner {
    OwnerRef::Personal(UserId::new(Uuid::now_v7()))
}

fn engine() -> Engine {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema_or_panic_for_tests::<TestFact>();
    registry.add_cited_object_schema_or_panic_for_tests::<TestCitedObject>();
    registry.add_citation_mapping_schema_or_panic_for_tests::<TestCitationMapping>();
    registry.add_citation_mapping_schema_or_panic_for_tests::<MismatchedCitationMapping>();
    Engine::new(registry.freeze_or_panic_for_tests())
}

fn draft(_owner: &Owner) -> FactWriteCommand {
    let now = time::OffsetDateTime::now_utc();
    FactWriteCommand {
        schema_id: TestFact::schema_id(),
        schema_version: SchemaVersion::new(TestFact::SCHEMA_VERSION),
        handle: None,
        source_id: None,
        ingest_key: None,
        payload: json(&TestFact {
            value: "fact".to_string(),
        }),
        rendered_text: None,
        lexical_language: None,
        receipt: Some(FactReceiptDraft {
            source_id: SourceId::new("test/source"),
            source_batch_id: SourceBatchId::new(Uuid::now_v7()),
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        derived_from: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
    }
}

fn cited_object() -> InlineCitedObjectDraft {
    let payload = TestCitedObject {
        body: "object".to_string(),
    };
    InlineCitedObjectDraft {
        schema_id: TestCitedObject::schema_id(),
        schema_version: SchemaVersion::new(TestCitedObject::SCHEMA_VERSION),
        payload_bytes: json(&payload),
    }
}

fn mapping(schema_id: SchemaId) -> InlineCitationMappingDraft {
    InlineCitationMappingDraft {
        schema_id,
        schema_version: SchemaVersion::new(1),
        payload_bytes: json(&TestCitationMapping {
            byte_start: 0,
            byte_end: 6,
        }),
    }
}

#[tokio::test]
async fn authorize_fact_with_citation_rejects_kind_mismatch() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let mut cited_object = cited_object();
    cited_object.schema_id = TestFact::schema_id();
    cited_object.schema_version = SchemaVersion::new(TestFact::SCHEMA_VERSION);

    let err = engine()
        .authorize_fact_with_citation(
            &authz,
            Relation::Ingest,
            draft(&owner),
            cited_object,
            mapping(TestCitationMapping::schema_id()),
            &[],
        )
        .await
        .expect_err("Fact schema must not authorize as a CitedObject schema");

    assert_eq!(err.code, ErrorCode::UnknownSchema);
}

#[tokio::test]
async fn authorize_fact_with_citation_derives_cited_object_content_hash() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let expected = TestCitedObject {
        body: "object".to_string(),
    }
    .idempotency_key();

    let authorized = engine()
        .authorize_fact_with_citation(
            &authz,
            Relation::Ingest,
            draft(&owner),
            cited_object(),
            mapping(TestCitationMapping::schema_id()),
            &[],
        )
        .await
        .expect("registered cited object payload must authorize");

    assert_eq!(authorized.cited_object().content_hash(), &expected);
}

#[tokio::test]
async fn authorize_fact_with_citation_rejects_mapping_target_mismatch() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let err = engine()
        .authorize_fact_with_citation(
            &authz,
            Relation::Ingest,
            draft(&owner),
            cited_object(),
            mapping(MismatchedCitationMapping::schema_id()),
            &[],
        )
        .await
        .expect_err("mapping target schema must match cited object schema");

    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn authorize_citation_attachment_accepts_valid_pair() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let memory_id = proxima_core::MemoryId::new(Uuid::now_v7());
    let expected = TestCitedObject {
        body: "object".to_string(),
    }
    .idempotency_key();

    let authorized = engine()
        .authorize_citation_attachment(
            &authz,
            Relation::Ingest,
            owner,
            memory_id,
            EntityKind::Fact,
            cited_object(),
            mapping(TestCitationMapping::schema_id()),
        )
        .await
        .expect("registered citation attachment payloads must authorize");

    assert_eq!(authorized.memory_id(), memory_id);
    assert_eq!(authorized.owner(), &owner);
    assert_eq!(authorized.cited_object().content_hash(), &expected);
    assert_eq!(
        authorized.mapping().schema_id(),
        &TestCitationMapping::schema_id()
    );
}

#[tokio::test]
async fn authorize_citation_attachment_rejects_mapping_target_mismatch() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);

    let err = engine()
        .authorize_citation_attachment(
            &authz,
            Relation::Ingest,
            owner,
            proxima_core::MemoryId::new(Uuid::now_v7()),
            EntityKind::Fact,
            cited_object(),
            mapping(MismatchedCitationMapping::schema_id()),
        )
        .await
        .expect_err("mapping target schema must match cited object schema");

    assert_eq!(err.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn authorize_fact_with_citation_rejects_unknown_schema_ids() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let mut cited_object = cited_object();
    cited_object.schema_id = SchemaId::new("test/unknown-cited-object".to_string());

    let err = engine()
        .authorize_fact_with_citation(
            &authz,
            Relation::Ingest,
            draft(&owner),
            cited_object,
            mapping(TestCitationMapping::schema_id()),
            &[],
        )
        .await
        .expect_err("unknown cited object schema must be rejected");

    assert_eq!(err.code, ErrorCode::UnknownSchema);
}

#[test]
fn registered_cited_object_schema_exposes_sidecar_table() {
    let mut registry = FlavorRegistry::new();
    registry.add_cited_object_schema_or_panic_for_tests::<TestCitedObject>();
    let frozen = registry.freeze_or_panic_for_tests();

    let info = frozen
        .lookup_payload(
            &TestCitedObject::schema_id(),
            SchemaVersion::new(TestCitedObject::SCHEMA_VERSION),
            PayloadKind::CitedObject,
        )
        .expect("registered cited-object schema must be present");

    assert_eq!(info.sidecar_table.as_deref(), None);
}

/// A computed score is an Abstraction whose citation is its computation
/// record, so `citation_mapping_id` is legal for Abstractions as well as
/// Facts. A Perspective still never cites directly — it grounds through
/// the nodes its payload references.
#[tokio::test]
async fn an_abstraction_may_cite_and_a_perspective_may_not() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let memory_id = proxima_core::MemoryId::new(Uuid::now_v7());

    for kind in [EntityKind::Fact, EntityKind::Abstraction] {
        engine()
            .authorize_citation_attachment(
                &authz,
                Relation::Ingest,
                owner,
                memory_id,
                kind,
                cited_object(),
                mapping(TestCitationMapping::schema_id()),
            )
            .await
            .unwrap_or_else(|err| panic!("{kind:?} must be able to cite directly: {err}"));
    }

    for kind in [EntityKind::Perspective, EntityKind::Goal] {
        let err = engine()
            .authorize_citation_attachment(
                &authz,
                Relation::Ingest,
                owner,
                memory_id,
                kind,
                cited_object(),
                mapping(TestCitationMapping::schema_id()),
            )
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidArgument, "{kind:?}");
        assert!(err.message.contains("cite directly"), "{}", err.message);
    }
}
