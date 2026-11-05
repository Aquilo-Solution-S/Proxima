//! Smoke test for `proxiima_spiice!` and `proxiima_schema_id!` macros.

use proxiima_core::spiice::{
    EmbeddingRecipe, ProjectionDecl, Provenance, SchemaContract, SchemaRef, SearchProjectionDecl,
    SpiiceContract, TransferRule,
};
use proxiima_core::verbs::schema::PayloadKind;
use proxiima_core::{
    CitedObjectPayload, FactPayload, GoalPayload, PayloadKeyBuilder, SpiiceRegistry,
    proxiima_schema_id, proxiima_spiice,
};

/// The fixture spiice's ordinal. Any non-zero value: zero is core's, and
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
        id: SchemaRef::new("proxiima-core", name, 1),
        kind,
        sidecar_table,
        search: SearchProjectionDecl::None {
            why: "a macro fixture, not a search surface",
            tag_column: None,
        },
        embedding: EmbeddingRecipe::Never {
            why: "a macro fixture, not a memory",
        },
        transfer: TransferRule::StaysOnKey,
        provenance: Provenance::None,
        surfaces: &[],
        natural_key_columns: proxiima_core::NaturalKeyDeclaration::EMPTY,
    }
}

/// The declaration for the spiice below. `proxiima_spiice!` accepts
/// `contract =` optionally, and freeze refuses a spiice that omits it, so
/// every fixture spiice that reaches a freeze carries one.
static MACRO_SPIICE_CONTRACT: SpiiceContract = SpiiceContract {
    retired_facts: &[],
    spiice_id: "proxiima-core",
    ordinal: FIXTURE_ORDINAL,
    schemas: &[
        fixture_schema("test-fact", PayloadKind::Fact, Some("fact_test_fact_v1")),
        fixture_schema("test-goal", PayloadKind::Goal, Some("goal_test_goal_v1")),
        fixture_schema("test-cited-object", PayloadKind::CitedObject, None),
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

/// The twin for the schema-less spiice below: same id, nothing declared,
/// because that module registers nothing.
static EMPTY_SPIICE_CONTRACT: SpiiceContract = SpiiceContract {
    retired_facts: &[],
    spiice_id: "proxiima-core",
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
    // CARGO_PKG_NAME is `proxiima-core` here (we're inside the
    // `core` crate's tests/), so the macro produces
    // "proxiima-core/test-fact-v1". The `-v1` tail is the shape
    // `SchemaRef` renders, so the contract below can name this schema.
    const SCHEMA_ID: &'static str = proxiima_schema_id!("test-fact-v1");
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

impl proxiima_core::PayloadProjection for TestFactV1 {
    const KEY_FIELDS: &'static [proxiima_core::KeyField<Self>] = &[];
    fn display(&self) -> proxiima_core::PayloadDisplay {
        proxiima_core::PayloadDisplay {
            body: Some(self.body.clone()),
            ..Default::default()
        }
    }
    fn durable(&self) -> Result<proxiima_core::DurablePayload, String> {
        let mut builder = proxiima_core::DurablePayloadBuilder::new();
        builder.field("body", &self.body)?;
        Ok(builder.finish())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TestGoalV1 {
    text: String,
}

impl GoalPayload for TestGoalV1 {
    const SCHEMA_ID: &'static str = proxiima_schema_id!("test-goal-v1");
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

impl proxiima_core::PayloadProjection for TestGoalV1 {
    const KEY_FIELDS: &'static [proxiima_core::KeyField<Self>] = &[];
    fn display(&self) -> proxiima_core::PayloadDisplay {
        proxiima_core::PayloadDisplay::default()
    }
    fn durable(&self) -> Result<proxiima_core::DurablePayload, String> {
        let mut builder = proxiima_core::DurablePayloadBuilder::new();
        builder.field("text", &self.text)?;
        Ok(builder.finish())
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TestCitedObjectV1 {
    body: Vec<u8>,
}

impl CitedObjectPayload for TestCitedObjectV1 {
    const SCHEMA_ID: &'static str = proxiima_schema_id!("test-cited-object-v1");
    const SCHEMA_VERSION: u32 = 1;

    fn idempotency_key(&self) -> [u8; 32] {
        *blake3::hash(&self.body).as_bytes()
    }
}

impl proxiima_core::PayloadProjection for TestCitedObjectV1 {
    const KEY_FIELDS: &'static [proxiima_core::KeyField<Self>] = &[];
    fn display(&self) -> proxiima_core::PayloadDisplay {
        proxiima_core::PayloadDisplay::default()
    }
    fn durable(&self) -> Result<proxiima_core::DurablePayload, String> {
        let mut builder = proxiima_core::DurablePayloadBuilder::new();
        builder.field("body", &self.body)?;
        Ok(builder.finish())
    }
}

proxiima_spiice! {
    name = "proxiima-core",
    fact_schemas = [ TestFactV1 ],
    goal_schemas = [ TestGoalV1 ],
    cited_object_schemas = [ TestCitedObjectV1 ],
    schema_capability_tags = [
        (Fact, TestFactV1) => ["actor", "shared-vocab"],
        (Goal, TestGoalV1) => ["task"],
    ],
    contract = &MACRO_SPIICE_CONTRACT,
}

#[test]
fn spiice_macro_registers_fact_schema() {
    let mut registry = SpiiceRegistry::new();
    register(&mut registry).unwrap();
    let frozen = registry.freeze_or_panic_for_tests();
    let schemas = frozen.list();
    let macro_schemas: Vec<_> = schemas
        .iter()
        .filter(|s| {
            matches!(s.kind, PayloadKind::Fact | PayloadKind::Goal)
                && s.schema_id.as_str().starts_with("proxiima-core/test-")
        })
        .collect();
    assert_eq!(macro_schemas.len(), 2);
    assert_eq!(
        macro_schemas[0].schema_id.as_str(),
        "proxiima-core/test-fact-v1"
    );
    assert_eq!(macro_schemas[0].schema_version.into_inner(), 1);
    assert_eq!(macro_schemas[0].kind, PayloadKind::Fact);
    assert_eq!(
        macro_schemas[1].schema_id.as_str(),
        "proxiima-core/test-goal-v1"
    );
    assert_eq!(macro_schemas[1].schema_version.into_inner(), 1);
    assert_eq!(macro_schemas[1].kind, PayloadKind::Goal);
}

#[test]
fn spiice_macro_registers_schema_capability_tags() {
    let mut registry = SpiiceRegistry::new();
    register(&mut registry).unwrap();
    let frozen = registry.freeze_or_panic_for_tests();

    let fact_tags = frozen
        .schema_capability_tags(
            &TestFactV1::schema_id(),
            proxiima_core::SchemaVersion::new(TestFactV1::SCHEMA_VERSION),
            PayloadKind::Fact,
        )
        .expect("fact capability tags registered");
    assert_eq!(
        fact_tags
            .iter()
            .map(proxiima_core::CapabilityTag::as_str)
            .collect::<Vec<_>>(),
        ["actor", "shared-vocab"],
    );

    let goal_tags = frozen
        .schema_capability_tags(
            &TestGoalV1::schema_id(),
            proxiima_core::SchemaVersion::new(TestGoalV1::SCHEMA_VERSION),
            PayloadKind::Goal,
        )
        .expect("goal capability tags registered");
    assert_eq!(
        goal_tags
            .iter()
            .map(proxiima_core::CapabilityTag::as_str)
            .collect::<Vec<_>>(),
        ["task"],
    );
}

#[test]
fn spiice_macro_registers_cited_object_schema() {
    let mut registry = SpiiceRegistry::new();
    register(&mut registry).unwrap();
    let frozen = registry.freeze_or_panic_for_tests();
    let schemas = frozen.list();
    let cited_schema = schemas
        .iter()
        .find(|s| s.schema_id.as_str() == "proxiima-core/test-cited-object-v1")
        .expect("cited object schema registered");
    assert_eq!(cited_schema.schema_version.into_inner(), 1);
    assert_eq!(cited_schema.kind, PayloadKind::CitedObject);
}

mod empty_goal_schemas {
    use proxiima_core::proxiima_spiice;

    proxiima_spiice! {
        name = "proxiima-core",
        goal_schemas = [],
        contract = &super::EMPTY_SPIICE_CONTRACT,
    }
}

#[test]
fn spiice_macro_accepts_empty_goal_schemas() {
    let mut registry = SpiiceRegistry::new();
    empty_goal_schemas::register(&mut registry).unwrap();
    let frozen = registry.freeze_or_panic_for_tests();
    // The default registry may ship substrate-managed schemas. Asserting
    // absence of the macro-targeted schemas is what this test cares about.
    assert!(
        frozen
            .list()
            .iter()
            .all(|s| !s.schema_id.as_str().starts_with("proxiima-core/test-")),
        "no test-spiice schemas should be registered when goal_schemas = []",
    );
}
