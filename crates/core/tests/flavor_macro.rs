//! Smoke test for proxima_flavor! and proxima_schema_id!
//! macros in M3.A.1.

use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{FactPayload, FlavorRegistry, GoalPayload, proxima_flavor, proxima_schema_id};

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
    fn render(&self) -> String {
        self.body.clone()
    }
    fn sidecar_table() -> &'static str {
        "fact_test_fact_v1"
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct TestGoalV1 {
    text: String,
}

impl GoalPayload for TestGoalV1 {
    const SCHEMA_ID: &'static str = proxima_schema_id!("test-goal");
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "goal_test_goal_v1"
    }
}

proxima_flavor! {
    name = "proxima-core",
    fact_schemas = [ TestFactV1 ],
    goal_schemas = [ TestGoalV1 ],
}

#[test]
fn flavor_macro_registers_fact_schema() {
    let mut registry = FlavorRegistry::new();
    register(&mut registry);
    let frozen = registry.freeze();
    let schemas = frozen.list();
    let macro_schemas: Vec<_> = schemas
        .iter()
        .filter(|s| s.schema_id.as_str().starts_with("proxima-core/test-"))
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
        AuthorshipKindMask, EntityKindMask, RelationClass, RelationDescriptor, proxima_flavor,
    };
    proxima_flavor! {
        name = "proxima-core",
        relations = [ RelationDescriptor::substrate(
            "wrong-crate/bad",
            RelationClass::Provenance,
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
