use proxima_core::engine::Engine;
use proxima_core::error::ErrorCode;
use proxima_core::verbs::fact_ingest::{
    FactReceiptDraft, FactWriteCommand, InlineCitedObjectDraft,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthPath, AuthzContext, CitedObjectPayload, FactPayload, FlavorRegistry,
    OCCURRED_AT_FUTURE_BOUND, OwnerId, PayloadKeyBuilder, SchemaId, SchemaVersion, SourceId,
    UserId, canonical_json_bytes,
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

fn owner() -> OwnerId {
    OwnerId::from(UserId::new(Uuid::now_v7()))
}

fn engine() -> Engine {
    let mut registry = FlavorRegistry::new();
    registry.add_fact_schema_or_panic_for_tests::<TestFact>();
    registry.add_cited_object_schema_or_panic_for_tests::<TestCitedObject>();
    Engine::new(registry.freeze_or_panic_for_tests())
}

fn draft(_owner: &OwnerId) -> FactWriteCommand {
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
        }),
        occurred_at: None,
        citation: None,
        additional_references: Vec::new(),
        refs: Vec::new(),
        blob_id: None,
        kind: "fact".into(),
        request: None,
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

/// One bound on every library write path: a Fact claiming to occur more than
/// [`OCCURRED_AT_FUTURE_BOUND`] from now is refused before a witness exists,
/// whichever entry point authorizes it, not only by `core_remember`.
#[tokio::test]
async fn every_fact_authorization_refuses_an_occurrence_beyond_the_future_bound() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let engine = engine();
    let beyond =
        time::OffsetDateTime::now_utc() + OCCURRED_AT_FUTURE_BOUND + time::Duration::MINUTE;
    let refused = |err: proxima_core::error::ProtocolError| {
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("occurred_at"), "{}", err.message);
    };

    refused(
        engine
            .authorize_fact_ingest(&authz, draft(&owner).occurred_at(beyond), &[])
            .await
            .expect_err("a plain Fact write must refuse the future occurrence"),
    );
    refused(
        engine
            .authorize_fact_with_citation(
                &authz,
                draft(&owner).occurred_at(beyond),
                cited_object(),
                &[],
            )
            .await
            .expect_err("a citing Fact write must refuse the future occurrence"),
    );
    refused(
        engine
            .authorize_fact_with_citation_by_ref(
                &authz,
                draft(&owner).occurred_at(beyond),
                Uuid::now_v7(),
                &[],
            )
            .await
            .expect_err("a Fact citing a stored object must refuse the future occurrence"),
    );
}

/// A backdated occurrence is admitted and rides the witness to storage in
/// the form storage keeps: UTC, whole microseconds.
#[tokio::test]
async fn a_past_occurrence_rides_the_witness_in_stored_form() {
    let owner = owner();
    let authz = AuthzContext::single_owner(&owner, AuthPath::HostBearer);
    let past = time::OffsetDateTime::parse(
        "2024-02-29T23:59:59.123456789+02:00",
        &time::format_description::well_known::Rfc3339,
    )
    .expect("a valid RFC 3339 time");
    let stored = time::OffsetDateTime::parse(
        "2024-02-29T21:59:59.123456Z",
        &time::format_description::well_known::Rfc3339,
    )
    .expect("a valid RFC 3339 time");

    let authorized = engine()
        .authorize_fact_ingest(&authz, draft(&owner).occurred_at(past), &[])
        .await
        .expect("a past occurrence is admitted");

    assert_eq!(authorized.draft().occurred_at, Some(stored));
    assert_eq!(
        authorized
            .draft()
            .occurred_at
            .map(time::OffsetDateTime::offset),
        Some(time::UtcOffset::UTC)
    );
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
