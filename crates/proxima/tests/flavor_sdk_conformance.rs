use std::process::Command;

use proxima::flavor::{
    AuthorshipKindMask, EntityKindMask, FactPayload, FlavorDescriptor, FlavorProvenance,
    FlavorRegistry, FlavorRegistryError, PayloadKeyBuilder, RelationClass, RelationDescriptor,
};
use proxima_core::EndpointBinding;
use proxima_core::mcp::core_tools::SearchMemoriesTool;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ConformanceFact {
    id: Uuid,
}

impl FactPayload for ConformanceFact {
    const SCHEMA_ID: &'static str = "proxima-conformance/fact-v1";
    const SCHEMA_VERSION: u32 = 1;

    fn receipt_key(&self) -> Vec<u8> {
        let mut key = PayloadKeyBuilder::new(Self::SCHEMA_ID, Self::SCHEMA_VERSION);
        key.field_uuid("id", self.id);
        key.finish()
    }

    fn render(&self) -> String {
        format!("conformance fact {}", self.id)
    }
}

#[test]
fn duplicate_schema_relation_tool_and_flavor_return_typed_errors() {
    let mut registry = FlavorRegistry::new();
    registry.try_add_fact_schema::<ConformanceFact>().unwrap();
    registry.try_add_fact_schema::<ConformanceFact>().unwrap();
    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(err, FlavorRegistryError::DuplicateSchema { .. }));

    let mut registry = FlavorRegistry::new();
    let descriptor = RelationDescriptor::substrate(
        "proxima-conformance/rel",
        RelationClass::Structural,
        EndpointBinding::Pin,
        EndpointBinding::Pin,
        EntityKindMask::memory(),
        EntityKindMask::memory(),
        AuthorshipKindMask::external_agent(),
    );
    registry.try_add_relation(descriptor.clone()).unwrap();
    registry.try_add_relation(descriptor).unwrap();
    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(err, FlavorRegistryError::DuplicateRelation { .. }));

    let mut registry = FlavorRegistry::new();
    registry
        .try_add_mcp_tool::<SearchMemoriesTool>("core")
        .unwrap();
    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(err, FlavorRegistryError::DuplicateTool { .. }));

    let mut registry = FlavorRegistry::new();
    let descriptor = FlavorDescriptor {
        flavor_id: "proxima-conformance".to_string(),
        display_name: "Conformance".to_string(),
        package_version: "0.0.0".to_string(),
        author: None,
        provenance: FlavorProvenance::Builtin,
    };
    registry.try_add_flavor(descriptor.clone()).unwrap();
    registry.try_add_flavor(descriptor).unwrap();
    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(err, FlavorRegistryError::DuplicateFlavor { .. }));
}

#[test]
fn host_and_flavor_sdk_imports_are_separate_and_compile() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn accepts_fact<T: FactPayload>() {}

    assert_send_sync::<proxima::RuntimeBuilder>();
    assert_send_sync::<proxima::Engine>();
    accepts_fact::<ConformanceFact>();

    let _registry = FlavorRegistry::new();
}

#[test]
fn pr8_raw_surface_denial_script_passes() {
    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("crate lives under repo/crates/proxima");
    let status = Command::new("python3")
        .arg("scripts/check-pr8-api-surface.py")
        .current_dir(repo_root)
        .status()
        .expect("run PR8 API surface script");
    assert!(status.success(), "PR8 API surface script must pass");
}
