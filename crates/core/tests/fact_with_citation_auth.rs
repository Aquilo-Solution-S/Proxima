use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::verbs::fact_ingest::{
    FactReceiptDraft, FactWriteCommand, InlineCitedObjectDraft,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthPath, AuthzContext, CitedObjectPayload, FactPayload, FlavorRegistry, Owner, OwnerRef,
    PayloadKeyBuilder, SchemaId, SchemaVersion, SourceId, UserId, canonical_json_bytes,
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

    fn idempotency_key(&self) -> [u8; 32] {
        *blake3::hash(self.body.as_bytes()).as_bytes()
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
            observed_at: now,
            occurred_at: now,
        }),
        citation: None,
        additional_references: Vec::new(),
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

#[tokio::test]
async fn authorize_fact_with_citation_rejects_kind_mismatch() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let mut cited_object = cited_object();
    cited_object.schema_id = TestFact::schema_id();
    cited_object.schema_version = SchemaVersion::new(TestFact::SCHEMA_VERSION);

    let err = engine()
        .authorize_fact_with_citation(&authz, draft(&owner), cited_object, &[])
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
        .authorize_fact_with_citation(&authz, draft(&owner), cited_object(), &[])
        .await
        .expect("registered cited object payload must authorize");

    assert_eq!(authorized.cited_object().content_hash(), &expected);
}

#[tokio::test]
async fn authorize_fact_with_citation_rejects_unknown_schema_ids() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let mut cited_object = cited_object();
    cited_object.schema_id = SchemaId::new("test/unknown-cited-object".to_string());

    let err = engine()
        .authorize_fact_with_citation(&authz, draft(&owner), cited_object, &[])
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
