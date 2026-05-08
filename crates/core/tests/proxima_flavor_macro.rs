use proxima_core::PerspectivePayload;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct DemoSelfPayload {
    display_name: String,
}

impl PerspectivePayload for DemoSelfPayload {
    const SCHEMA_ID: &'static str = "proxima-test/self-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.self_v1"
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct DemoOutputPayload {
    summary: String,
}

impl PerspectivePayload for DemoOutputPayload {
    const SCHEMA_ID: &'static str = "proxima-test/out-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn sidecar_table() -> &'static str {
        "proxima_test.out_v1"
    }
}

proxima_core::proxima_flavor! {
    name = "proxima-test",
    perspective_schemas = [
        DemoSelfPayload,
        DemoOutputPayload,
    ],
}

#[test]
fn macro_registers_bundled_recipes_under_flavor_prefix() {
    use proxima_core::FlavorRegistry;
    use std::path::PathBuf;

    proxima_core::proxima_flavor! {
        name = "macro-test-recipes",
        recipes_root = env!("CARGO_MANIFEST_DIR"),
        recipes = ["alpha", "beta"],
    }

    let mut registry = FlavorRegistry::new();
    register(&mut registry);
    let frozen = registry.freeze();

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        frozen.bundled_recipe_path("macro-test-recipes/alpha"),
        Some(manifest_dir.join("recipes/alpha.yaml")),
    );
    assert_eq!(
        frozen.bundled_recipe_path("macro-test-recipes/beta"),
        Some(manifest_dir.join("recipes/beta.yaml")),
    );
    assert_eq!(
        frozen.bundled_recipes_for("macro-test-recipes"),
        vec!["macro-test-recipes/alpha", "macro-test-recipes/beta"],
    );
}
