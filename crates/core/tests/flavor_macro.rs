//! Smoke test for `proxima_flavor!` and `proxima_schema_id!`
//! macros in M3.A.1.

use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    CitationMappingPayload, CitedObjectPayload, FactPayload, FlavorRegistry, GoalPayload,
    PayloadKeyBuilder, SchemaId, proxima_flavor, proxima_schema_id,
};

#[derive(serde::Serialize, serde::Deserialize)]
struct TestFactV1 {
    body: String,
}

impl FactPayload for TestFactV1 {
    // CARGO_PKG_NAME is `proxima-core` here (we're inside the
    // `core` crate's tests/), so the macro produces
    // "proxima-core/test-fact".
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-fact");
    const SCHEMA_VERSION: u32 = 1;
    fn event_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("body", &self.body);
        key.finish()
    }
    fn render(&self) -> String {
        self.body.clone()
    }
    fn sidecar_table() -> Option<&'static str> {
        Some("fact_test_fact_v1")
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TestGoalV1 {
    text: String,
}

impl GoalPayload for TestGoalV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-goal");
    const SCHEMA_VERSION: u32 = 1;

    fn goal_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_str("text", &self.text);
        key.finish()
    }

    fn sidecar_table() -> Option<&'static str> {
        Some("goal_test_goal_v1")
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TestCitedObjectV1 {
    body: Vec<u8>,
}

impl CitedObjectPayload for TestCitedObjectV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-cited-object");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "cited_object_test_v1"
    }

    fn idempotency_key(&self) -> [u8; 32] {
        *blake3::hash(&self.body).as_bytes()
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TestCitationMappingV1 {
    start: u64,
    end: u64,
}

impl CitationMappingPayload for TestCitationMappingV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-citation-mapping");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> Option<&'static str> {
        Some("citation_mapping_test_v1")
    }

    fn cited_object_schema() -> SchemaId {
        TestCitedObjectV1::schema_id()
    }
}

proxima_flavor! {
    name = "proxima-core",
    fact_schemas = [ TestFactV1 ],
    goal_schemas = [ TestGoalV1 ],
    cited_object_schemas = [ TestCitedObjectV1 ],
    citation_mapping_schemas = [ TestCitationMappingV1 ],
    schema_capability_tags = [
        (Fact, TestFactV1) => ["actor", "shared-vocab"],
        (Goal, TestGoalV1) => ["task"],
    ],
}

#[test]
fn flavor_macro_registers_fact_schema() {
    let mut registry = FlavorRegistry::new();
    register(&mut registry);
    let frozen = registry.freeze();
    let schemas = frozen.list();
    let macro_schemas: Vec<_> = schemas
        .iter()
        .filter(|s| {
            matches!(s.kind, PayloadKind::Fact | PayloadKind::Goal)
                && s.schema_id.as_str().starts_with("proxima-core/test-")
        })
        .collect();
    assert_eq!(macro_schemas.len(), 2);
    assert_eq!(
        macro_schemas[0].schema_id.as_str(),
        "proxima-core/test-fact"
    );
    assert_eq!(macro_schemas[0].schema_version.into_inner(), 1);
    assert_eq!(macro_schemas[0].kind, PayloadKind::Fact);
    assert_eq!(
        macro_schemas[1].schema_id.as_str(),
        "proxima-core/test-goal"
    );
    assert_eq!(macro_schemas[1].schema_version.into_inner(), 1);
    assert_eq!(macro_schemas[1].kind, PayloadKind::Goal);
}

#[test]
fn flavor_macro_registers_schema_capability_tags() {
    let mut registry = FlavorRegistry::new();
    register(&mut registry);
    let frozen = registry.freeze();

    let fact_tags = frozen
        .schema_capability_tags(
            &TestFactV1::schema_id(),
            proxima_core::SchemaVersion::new(TestFactV1::SCHEMA_VERSION),
            PayloadKind::Fact,
        )
        .expect("fact capability tags registered");
    assert_eq!(
        fact_tags
            .iter()
            .map(proxima_core::CapabilityTag::as_str)
            .collect::<Vec<_>>(),
        ["actor", "shared-vocab"],
    );

    let goal_tags = frozen
        .schema_capability_tags(
            &TestGoalV1::schema_id(),
            proxima_core::SchemaVersion::new(TestGoalV1::SCHEMA_VERSION),
            PayloadKind::Goal,
        )
        .expect("goal capability tags registered");
    assert_eq!(
        goal_tags
            .iter()
            .map(proxima_core::CapabilityTag::as_str)
            .collect::<Vec<_>>(),
        ["task"],
    );
}

#[test]
fn flavor_macro_registers_citation_schemas() {
    let mut registry = FlavorRegistry::new();
    register(&mut registry);
    let frozen = registry.freeze();
    let schemas = frozen.list();
    let cited_schema = schemas
        .iter()
        .find(|s| s.schema_id.as_str() == "proxima-core/test-cited-object")
        .expect("cited object schema registered");
    assert_eq!(cited_schema.schema_version.into_inner(), 1);
    assert_eq!(cited_schema.kind, PayloadKind::CitedObject);

    let citation_mapping_schema = schemas
        .iter()
        .find(|s| s.schema_id.as_str() == "proxima-core/test-citation-mapping")
        .expect("citation mapping schema registered");
    assert_eq!(citation_mapping_schema.schema_version.into_inner(), 1);
    assert_eq!(citation_mapping_schema.kind, PayloadKind::CitationMapping);
}

mod empty_goal_schemas {
    use proxima_core::proxima_flavor;

    proxima_flavor! {
        name = "proxima-core",
        goal_schemas = [],
    }
}

#[test]
fn flavor_macro_accepts_empty_goal_schemas() {
    let mut registry = FlavorRegistry::new();
    empty_goal_schemas::register(&mut registry);
    let frozen = registry.freeze();
    // The default registry now ships substrate-managed schemas (e.g.
    // core/personality_config_changed_v1). Asserting absence of the
    // macro-targeted schemas is what this test cares about.
    assert!(
        frozen
            .list()
            .iter()
            .all(|s| !s.schema_id.as_str().starts_with("proxima-core/test-")),
        "no test-flavor schemas should be registered when goal_schemas = []",
    );
}

// A misprefixed *relation*. Schema / tool / trigger prefixes are now
// compile-checked by a `const` assertion (so a misprefixed SCHEMA_ID
// fails the build and cannot be expressed in a compiled test).
// `relations` carry their prefix on a runtime `RelationDescriptor`
// field, so that arm still asserts at `register` time — this test
// covers the surviving runtime branch.
mod nested {
    use proxima_core::{
        AuthorshipKindMask, EndpointBinding, EntityKindMask, RelationClass, RelationDescriptor,
        proxima_flavor,
    };
    proxima_flavor! {
        name = "proxima-core",
        relations = [ RelationDescriptor::substrate(
            "wrong-crate/bad",
            RelationClass::Provenance,
            EndpointBinding::Pin,
            EndpointBinding::Pin,
            EntityKindMask::all(),
            EntityKindMask::all(),
            AuthorshipKindMask::core(),
        ) ],
    }
}

#[test]
#[should_panic(expected = "does not start with crate prefix")]
fn flavor_macro_rejects_wrong_prefix() {
    let mut registry = FlavorRegistry::new();
    nested::register(&mut registry);
}
