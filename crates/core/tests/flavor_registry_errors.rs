use std::sync::Arc;

use proxima_core::authz::OwnerResolver;
use proxima_core::error::ProtocolError;
use proxima_core::mcp::{McpTool, McpToolCtx, McpToolError};
use proxima_core::verbs::schema::PayloadKind;
use proxima_core::{
    AuthzContext, DependencySatisfactionRule, FlavorDescriptor, FlavorProvenance, FlavorRegistry,
    FlavorRegistryError, MemoryId, MemoryInspectPort, Owner, SchemaId, SchemaVersion, StorageError,
};

#[derive(schemars::JsonSchema, serde::Deserialize)]
struct EmptyArgs {}

struct DemoTool;

impl McpTool for DemoTool {
    const NAME: &'static str = "proxima-test_demo";
    const DESCRIPTION: &'static str = "test";
    type Args = EmptyArgs;
    type Output = ();

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyArgs,
    ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

struct WrongPrefixTool;

impl McpTool for WrongPrefixTool {
    const NAME: &'static str = "wrong_demo";
    const DESCRIPTION: &'static str = "test";
    type Args = EmptyArgs;
    type Output = ();

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyArgs,
    ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

struct ProviderUnsafeTool;

impl McpTool for ProviderUnsafeTool {
    const NAME: &'static str = "proxima-test/demo";
    const DESCRIPTION: &'static str = "test";
    type Args = EmptyArgs;
    type Output = ();

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyArgs,
    ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug)]
struct TestRule(&'static str);

#[async_trait::async_trait]
impl DependencySatisfactionRule for TestRule {
    fn target_schema_id(&self) -> &'static str {
        self.0
    }

    async fn is_satisfied(
        &self,
        _storage: &dyn MemoryInspectPort,
        _owner: &Owner,
        _dependency_memory_id: MemoryId,
    ) -> Result<bool, StorageError> {
        Ok(true)
    }
}

#[derive(Debug)]
struct TestResolver;

impl OwnerResolver for TestResolver {
    fn resolve(&self, _authz: &AuthzContext, requested: &Owner) -> Result<Owner, ProtocolError> {
        Ok(*requested)
    }
}

#[test]
fn duplicate_schema_is_typed_freeze_error() {
    let schema_id = SchemaId::new("proxima-test/duplicate".to_string());
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_opaque_schema(schema_id.clone(), SchemaVersion::new(1), PayloadKind::Fact)
        .unwrap();
    registry
        .try_add_opaque_schema(schema_id.clone(), SchemaVersion::new(1), PayloadKind::Fact)
        .unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::DuplicateSchema {
            schema_id: ref id,
            schema_version,
            kind: PayloadKind::Fact,
        } if id == &schema_id && schema_version == SchemaVersion::new(1)
    ));
}

#[test]
fn duplicate_tool_is_typed_freeze_error() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_mcp_tool::<DemoTool>("proxima-test")
        .unwrap();
    registry
        .try_add_mcp_tool::<DemoTool>("proxima-test")
        .unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::DuplicateTool {
            name: "proxima-test_demo"
        }
    ));
}

#[test]
fn duplicate_flavor_is_typed_freeze_error() {
    let descriptor = FlavorDescriptor {
        flavor_id: "proxima-test".to_string(),
        display_name: "Proxima Test".to_string(),
        package_version: "0.0.0".to_string(),
        author: None,
        provenance: FlavorProvenance::Builtin,
    };
    let mut registry = FlavorRegistry::new();
    registry.try_add_flavor(descriptor.clone()).unwrap();
    registry.try_add_flavor(descriptor).unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::DuplicateFlavor { flavor_id } if flavor_id == "proxima-test"
    ));
}

#[test]
fn duplicate_dependency_rule_is_typed_freeze_error() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_dependency_satisfaction_rule("proxima-test/fact", Arc::new(TestRule("x")))
        .unwrap();
    registry
        .try_add_dependency_satisfaction_rule("proxima-test/fact", Arc::new(TestRule("x")))
        .unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::DuplicateDependencyRule { schema_id }
            if schema_id == "proxima-test/fact"
    ));
}

#[test]
fn duplicate_owner_resolver_is_typed_add_error() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_set_owner_resolver(Arc::new(TestResolver))
        .unwrap();

    let err = registry
        .try_set_owner_resolver(Arc::new(TestResolver))
        .unwrap_err();
    assert_eq!(err, FlavorRegistryError::DuplicateOwnerResolver);
}

#[test]
fn invalid_capability_tag_is_typed_add_error() {
    let schema_id = SchemaId::new("proxima-test/fact".to_string());
    let mut registry = FlavorRegistry::new();

    let err = registry
        .try_add_schema_capability_tags(
            PayloadKind::Fact,
            schema_id.clone(),
            SchemaVersion::new(1),
            ["NotValid"],
        )
        .unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::InvalidCapabilityTag {
            schema_id: ref id,
            schema_version,
            kind: PayloadKind::Fact,
            tag,
            ..
        } if id == &schema_id && schema_version == SchemaVersion::new(1) && tag == "NotValid"
    ));
}

#[test]
fn invalid_tool_names_are_typed_add_errors() {
    let mut registry = FlavorRegistry::new();
    let err = registry
        .try_add_mcp_tool::<WrongPrefixTool>("proxima-test")
        .unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::InvalidToolName {
            name: "wrong_demo",
            ..
        }
    ));

    let err = registry
        .try_add_mcp_tool::<ProviderUnsafeTool>("proxima-test")
        .unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::InvalidToolName {
            name: "proxima-test/demo",
            ..
        }
    ));
}

#[test]
fn unregistered_schema_capability_tags_are_typed_freeze_error() {
    let schema_id = SchemaId::new("proxima-test/missing".to_string());
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_schema_capability_tags(
            PayloadKind::Fact,
            schema_id.clone(),
            SchemaVersion::new(1),
            ["actor"],
        )
        .unwrap();

    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::UnregisteredSchemaCapabilityTags {
            schema_id: ref id,
            schema_version,
            kind: PayloadKind::Fact,
        } if id == &schema_id && schema_version == SchemaVersion::new(1)
    ));
}

mod bad_opaque_prefix_flavor {
    proxima_core::proxima_flavor! {
        name = "proxima-test",
        display_name = "Proxima Test Bad Opaque",
        fact_schemas = [],
        abstraction_schemas = [],
        perspective_schemas = [],
        goal_schemas = [],
        opaque_cited_object_schemas = ["wrong-prefix/blob-v1"],
        opaque_citation_mapping_schemas = [],
        mcp_tools = [],
    }
}

#[test]
fn schema_ingress_mismatch_is_typed_register_error() {
    let mut registry = FlavorRegistry::new();
    let err = bad_opaque_prefix_flavor::register(&mut registry).unwrap_err();
    assert!(matches!(
        err,
        FlavorRegistryError::SchemaIngressMismatch {
            schema_id,
            schema_version,
            kind: PayloadKind::CitedObject,
        } if schema_id == SchemaId::new("wrong-prefix/blob-v1".to_string())
            && schema_version == SchemaVersion::new(1)
    ));
}
