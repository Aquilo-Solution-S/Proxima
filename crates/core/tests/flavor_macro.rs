//! Smoke test for `proxima_flavor!` and `proxima_schema_id!` macros.

use proxima_core::flavor::{
    EmbeddingRecipe, FlavorContract, ProjectionDecl, Provenance, SchemaContract, SchemaRef,
    SearchProjectionDecl, TransferRule,
};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    CitationMappingPayload, CitedObjectPayload, FactPayload, FlavorRegistry, GoalPayload,
    PayloadKeyBuilder, SchemaId, proxima_flavor, proxima_schema_id,
};

/// The fixture flavor's ordinal. Any non-zero value: zero is core's, and
/// freeze refuses a second claim on it.
const FIXTURE_ORDINAL: u16 = 7;

/// A fixture schema declaration. Every field but the identity is the
/// "declares nothing" value, because the subject of this file is what the
/// MACRO registers, not what a contract can say about it.
const fn fixture_schema(
    name: &'static str,
    kind: PayloadKind,
    sidecar_table: Option<&'static str>,
) -> SchemaContract {
    SchemaContract {
        id: SchemaRef::new("proxima-core", name, 1),
        kind,
        sidecar_table,
        search: SearchProjectionDecl::None {
            why: "a macro fixture, not a search surface",
        },
        embedding: EmbeddingRecipe::Never {
            why: "a macro fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: &[],
    }
}

/// The declaration for the flavor below. `proxima_flavor!` accepts
/// `contract =` optionally, and freeze refuses a flavor that omits it, so
/// every fixture flavor that reaches a freeze carries one.
static MACRO_FLAVOR_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "proxima-core",
    ordinal: FIXTURE_ORDINAL,
    schemas: &[
        fixture_schema("test-fact", PayloadKind::Fact, Some("fact_test_fact_v1")),
        fixture_schema("test-goal", PayloadKind::Goal, Some("goal_test_goal_v1")),
        fixture_schema("test-cited-object", PayloadKind::CitedObject, None),
        fixture_schema("test-citation-mapping", PayloadKind::CitationMapping, None),
    ],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "a macro fixture registers no search surface",
    },
};

/// The twin for the schema-less flavor below: same id, nothing declared,
/// because that module registers nothing.
static EMPTY_FLAVOR_CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "proxima-core",
    ordinal: FIXTURE_ORDINAL,
    schemas: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[],
    resources: &[],
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
    projection: ProjectionDecl::None {
        why: "a macro fixture registers no search surface",
    },
};

#[derive(serde::Serialize, serde::Deserialize)]
struct TestFactV1 {
    body: String,
}

impl FactPayload for TestFactV1 {
    // CARGO_PKG_NAME is `proxima-core` here (we're inside the
    // `core` crate's tests/), so the macro produces
    // "proxima-core/test-fact-v1". The `-v1` tail is the shape
    // `SchemaRef` renders, so the contract below can name this schema.
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-fact-v1");
    const SCHEMA_VERSION: u32 = 1;
    fn receipt_key(&self) -> Vec<u8> {
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
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-goal-v1");
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
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-cited-object-v1");
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
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-citation-mapping-v1");
    const SCHEMA_VERSION: u32 = 1;

    // A citation mapping declares no sidecar: the shared-blob dedupe arm
    // repoints citations by following foreign keys, and a sidecar naming a
    // blob by convention would be walked past. Freeze refuses a contract
    // that declares one.
    fn sidecar_table() -> Option<&'static str> {
        None
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
    contract = &MACRO_FLAVOR_CONTRACT,
}

#[test]
fn flavor_macro_registers_fact_schema() {
    let mut registry = FlavorRegistry::new();
    register(&mut registry).unwrap();
    let frozen = registry.freeze_or_panic_for_tests();
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
        "proxima-core/test-fact-v1"
    );
    assert_eq!(macro_schemas[0].schema_version.into_inner(), 1);
    assert_eq!(macro_schemas[0].kind, PayloadKind::Fact);
    assert_eq!(
        macro_schemas[1].schema_id.as_str(),
        "proxima-core/test-goal-v1"
    );
    assert_eq!(macro_schemas[1].schema_version.into_inner(), 1);
    assert_eq!(macro_schemas[1].kind, PayloadKind::Goal);
}

#[test]
fn flavor_macro_registers_schema_capability_tags() {
    let mut registry = FlavorRegistry::new();
    register(&mut registry).unwrap();
    let frozen = registry.freeze_or_panic_for_tests();

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
    register(&mut registry).unwrap();
    let frozen = registry.freeze_or_panic_for_tests();
    let schemas = frozen.list();
    let cited_schema = schemas
        .iter()
        .find(|s| s.schema_id.as_str() == "proxima-core/test-cited-object-v1")
        .expect("cited object schema registered");
    assert_eq!(cited_schema.schema_version.into_inner(), 1);
    assert_eq!(cited_schema.kind, PayloadKind::CitedObject);

    let citation_mapping_schema = schemas
        .iter()
        .find(|s| s.schema_id.as_str() == "proxima-core/test-citation-mapping-v1")
        .expect("citation mapping schema registered");
    assert_eq!(citation_mapping_schema.schema_version.into_inner(), 1);
    assert_eq!(citation_mapping_schema.kind, PayloadKind::CitationMapping);
}

mod empty_goal_schemas {
    use proxima_core::proxima_flavor;

    proxima_flavor! {
        name = "proxima-core",
        goal_schemas = [],
        contract = &super::EMPTY_FLAVOR_CONTRACT,
    }
}

#[test]
fn flavor_macro_accepts_empty_goal_schemas() {
    let mut registry = FlavorRegistry::new();
    empty_goal_schemas::register(&mut registry).unwrap();
    let frozen = registry.freeze_or_panic_for_tests();
    // The default registry may ship substrate-managed schemas. Asserting
    // absence of the macro-targeted schemas is what this test cares about.
    assert!(
        frozen
            .list()
            .iter()
            .all(|s| !s.schema_id.as_str().starts_with("proxima-core/test-")),
        "no test-flavor schemas should be registered when goal_schemas = []",
    );
}
