use proxima_core::verbs::schema::{SchemaRegistry, SchemaRequest};

#[test]
fn empty_registry_returns_empty_response() {
    let registry = SchemaRegistry::new();
    let resp = registry.handle(&SchemaRequest);
    assert!(resp.schemas.is_empty());
}
