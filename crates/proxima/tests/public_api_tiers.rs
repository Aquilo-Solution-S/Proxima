#[test]
fn host_api_imports_from_root() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<proxima::RuntimeBuilder>();
    assert_send_sync::<proxima::RuntimeConfig>();
    assert_send_sync::<proxima::Engine>();
    let owner: proxima::Owner = proxima::company_owner(uuid::Uuid::nil());
    let _authz: proxima::AuthzContext = proxima::AuthzContext::denied_for_owner(&owner);
    let _cursor: proxima::Cursor = proxima::Cursor::empty();
}

#[test]
fn flavor_sdk_imports_from_flavor_module() {
    use proxima::flavor::{FactPayload, FlavorBundle, FlavorRegistry, SchemaId};
    fn _needs_bundle<T: FlavorBundle>() {}
    fn _needs_fact<T: FactPayload>() {}
    let _ = SchemaId::new("test/schema-v1".to_owned());
    let _ = FlavorRegistry::new();
}
