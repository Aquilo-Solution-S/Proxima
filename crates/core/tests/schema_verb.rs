use proxima_core::verbs::schema::{FlavorRegistryFrozen, SchemaRequest};

#[test]
fn empty_registry_returns_empty_response() {
    let registry = FlavorRegistryFrozen::new();
    let resp = registry.handle(&SchemaRequest);
    assert!(resp.schemas.is_empty());
}
