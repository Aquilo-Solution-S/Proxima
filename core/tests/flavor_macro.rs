//! Smoke test for proxima_flavor! and proxima_schema_id!
//! macros in M3.A.1.

use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{FactPayload, FlavorRegistry, proxima_flavor, proxima_schema_id};

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

proxima_flavor! {
    name = "proxima-core",
    fact_schemas = [ TestFactV1 ],
}

#[test]
fn flavor_macro_registers_fact_schema() {
    let mut registry = FlavorRegistry::new();
    register(&mut registry);
    let frozen = registry.freeze();
    let schemas = frozen.list();
    assert_eq!(schemas.len(), 1);
    assert_eq!(schemas[0].schema_id.as_str(), "proxima-core/test-fact");
    assert_eq!(schemas[0].schema_version.into_inner(), 1);
    assert_eq!(schemas[0].kind, PayloadKind::Fact);
}

// A FactPayload whose SCHEMA_ID is hard-coded (no proxima_schema_id!)
// and uses the wrong crate prefix.
#[derive(serde::Serialize, serde::Deserialize)]
struct WrongPrefixFact;

impl FactPayload for WrongPrefixFact {
    const SCHEMA_ID: &'static str = "wrong-crate/bad";
    const SCHEMA_VERSION: u32 = 1;
    fn render(&self) -> String {
        String::new()
    }
    fn sidecar_table() -> &'static str {
        "fact_wrong"
    }
}

// Use a separate module to avoid duplicate `register` symbol
mod nested {
    use super::WrongPrefixFact;
    use proxima_core::proxima_flavor;
    proxima_flavor! {
        name = "proxima-core",
        fact_schemas = [ WrongPrefixFact ],
    }
}

#[test]
#[should_panic(expected = "does not start with crate prefix")]
fn flavor_macro_rejects_wrong_prefix() {
    let mut registry = FlavorRegistry::new();
    nested::register(&mut registry);
}
