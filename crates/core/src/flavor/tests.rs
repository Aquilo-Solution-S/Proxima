use super::*;
use crate::mcp::{McpToolCtx, McpToolError};

#[derive(schemars::JsonSchema, serde::Deserialize)]
struct EmptyDemoArgs {}

struct Demo;

impl McpTool for Demo {
    const NAME: &'static str = "proxima-test_demo";
    const DESCRIPTION: &'static str = "test";
    // Required: `try_freeze` refuses to seal a flavor tool whose behaviour
    // the owner-role gate cannot resolve.
    const ANNOTATIONS: Option<crate::mcp::McpToolAnnotations> = Some(
        crate::mcp::McpToolAnnotations::new()
            .read_only(true)
            .open_world(false),
    );
    type Args = EmptyDemoArgs;
    type Output = ();

    fn call(
        _ctx: McpToolCtx,
        _args: EmptyDemoArgs,
    ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
        Box::pin(async { Ok(()) })
    }
}

#[test]
fn freeze_rejects_duplicate_tool_names() {
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool_or_panic_for_tests::<Demo>("proxima-test");
    registry.add_mcp_tool_or_panic_for_tests::<Demo>("proxima-test");
    let err = registry.try_freeze().expect_err("duplicate tool must fail");
    assert!(matches!(err, FlavorRegistryError::DuplicateTool { .. }));
}

#[test]
fn freeze_rejects_duplicate_schema_keys() {
    let mut registry = FlavorRegistry::new();
    let schema_id = SchemaId::new("proxima-test/duplicate".to_string());
    registry.add_opaque_schema_or_panic_for_tests(
        schema_id.clone(),
        SchemaVersion::new(1),
        PayloadKind::CitedObject,
    );
    registry.add_opaque_schema_or_panic_for_tests(
        schema_id,
        SchemaVersion::new(1),
        PayloadKind::CitedObject,
    );
    let err = registry
        .try_freeze()
        .expect_err("duplicate schema must fail");
    assert!(matches!(err, FlavorRegistryError::DuplicateSchema { .. }));
}

#[test]
fn freeze_rejects_two_memory_versions_under_one_selector() {
    let mut registry = FlavorRegistry::new();
    let original = registry
        .schemas
        .iter()
        .find(|schema| schema.kind == PayloadKind::Fact)
        .expect("default registry has a Fact schema")
        .clone();
    let mut newer = original.clone();
    newer.schema_version = SchemaVersion::new(original.schema_version.into_inner() + 1);
    let mut ingress = registry
        .protocol_ingress
        .iter()
        .find(|entry| {
            entry.schema_id == original.schema_id
                && entry.schema_version == original.schema_version
                && entry.kind == original.kind
        })
        .expect("Fact schema has ingress")
        .clone();
    ingress.schema_version = newer.schema_version;
    registry.schemas.push(newer);
    registry.protocol_ingress.push(ingress);
    let err = registry
        .try_freeze()
        .expect_err("Memory selector must be unique even across versions");
    assert!(matches!(
        err,
        FlavorRegistryError::DuplicateMemorySchemaSelector {
            kind: PayloadKind::Fact,
            ..
        }
    ));
}

#[test]
fn opaque_registration_rejects_memory_and_goal_kinds() {
    for kind in [
        PayloadKind::Fact,
        PayloadKind::Abstraction,
        PayloadKind::Perspective,
        PayloadKind::Goal,
    ] {
        let mut registry = FlavorRegistry::new();
        let schema_id = SchemaId::new(format!("proxima-test/opaque-{kind:?}"));
        let err = registry
            .try_add_opaque_schema(schema_id.clone(), SchemaVersion::new(1), kind)
            .expect_err("memory and Goal schemas require typed ingress");
        assert!(matches!(
            err,
            FlavorRegistryError::OpaqueSchemaKind {
                schema_id: ref actual_id,
                schema_version,
                kind: actual_kind,
            } if actual_id == &schema_id
                && schema_version == SchemaVersion::new(1)
                && actual_kind == kind
        ));
    }
}

#[test]
fn freeze_defensively_rejects_an_internally_malformed_opaque_fact() {
    let schema_id = SchemaId::new("proxima-test/internal-opaque-fact".to_string());
    let mut registry = FlavorRegistry::new();
    registry.schemas.push(SchemaInfo::opaque(
        schema_id.clone(),
        SchemaVersion::new(1),
        PayloadKind::Fact,
    ));

    let err = registry
        .try_freeze()
        .expect_err("freeze must defend against internal descriptor drift");
    assert!(matches!(
        err,
        FlavorRegistryError::OpaqueSchemaKind {
            schema_id: ref actual_id,
            schema_version,
            kind: PayloadKind::Fact,
        } if actual_id == &schema_id && schema_version == SchemaVersion::new(1)
    ));
}

#[test]
fn freeze_rejects_duplicate_ingress_for_a_typed_schema() {
    let mut registry = FlavorRegistry::new();
    let duplicate = registry
        .protocol_ingress
        .first()
        .expect("default registry has typed ingress")
        .clone();
    let schema_id = duplicate.schema_id.clone();
    let schema_version = duplicate.schema_version;
    let kind = duplicate.kind;
    registry.protocol_ingress.push(duplicate);

    let err = registry
        .try_freeze()
        .expect_err("typed schema must resolve to exactly one ingress parser");
    assert!(matches!(
        err,
        FlavorRegistryError::SchemaIngressMismatch {
            schema_id: ref actual_id,
            schema_version: actual_version,
            kind: actual_kind,
        } if actual_id == &schema_id
            && actual_version == schema_version
            && actual_kind == kind
    ));
}

#[test]
fn freeze_rejects_orphan_ingress_without_a_typed_schema() {
    let mut registry = FlavorRegistry::new();
    let mut orphan = registry
        .protocol_ingress
        .first()
        .expect("default registry has typed ingress")
        .clone();
    let schema_id = SchemaId::new("proxima-test/orphan-ingress".to_string());
    orphan.schema_id = schema_id.clone();
    let schema_version = orphan.schema_version;
    let kind = orphan.kind;
    registry.protocol_ingress.push(orphan);

    let err = registry
        .try_freeze()
        .expect_err("every ingress parser must resolve to a typed schema");
    assert!(matches!(
        err,
        FlavorRegistryError::SchemaIngressMismatch {
            schema_id: ref actual_id,
            schema_version: actual_version,
            kind: actual_kind,
        } if actual_id == &schema_id
            && actual_version == schema_version
            && actual_kind == kind
    ));
}

#[test]
fn freeze_rejects_capability_tags_for_unregistered_schema() {
    let mut registry = FlavorRegistry::new();
    registry.add_schema_capability_tags_or_panic_for_tests(
        PayloadKind::Fact,
        SchemaId::new("proxima-test/missing".to_string()),
        SchemaVersion::new(1),
        ["actor"],
    );
    let err = registry
        .try_freeze()
        .expect_err("unregistered capability tag schema must fail");
    assert!(matches!(
        err,
        FlavorRegistryError::UnregisteredSchemaCapabilityTags { .. }
    ));
}

#[test]
fn add_mcp_tool_rejects_unprefixed_tool_name() {
    struct Bad;

    impl McpTool for Bad {
        const NAME: &'static str = "wrong/demo";
        const DESCRIPTION: &'static str = "x";
        type Args = EmptyDemoArgs;
        type Output = ();

        fn call(
            _ctx: McpToolCtx,
            _args: EmptyDemoArgs,
        ) -> futures::future::BoxFuture<'static, Result<(), McpToolError>> {
            Box::pin(async { Ok(()) })
        }
    }

    let mut registry = FlavorRegistry::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        registry.add_mcp_tool_or_panic_for_tests::<Bad>("proxima-test");
    }));
    assert!(result.is_err(), "must panic on prefix mismatch");
}
