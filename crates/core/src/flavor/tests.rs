use super::*;
use crate::mcp::{McpToolCtx, McpToolError};
use crate::protocol::tool as protocol_tool;

#[test]
fn schema_id_has_prefix_edge_cases() {
    // Normal prefix match — the common case.
    assert!(schema_id_has_prefix("proxima-code/commit", "proxima-code/"));
    // Empty prefix is satisfied by anything.
    assert!(schema_id_has_prefix("abc", ""));
    // Prefix equal to the whole id.
    assert!(schema_id_has_prefix("abc", "abc"));
    // Prefix longer than the id never matches.
    assert!(!schema_id_has_prefix("ab", "abc"));
    // Plain mismatch.
    assert!(!schema_id_has_prefix("wrong/x", "right/"));
    // Truncated prefix — id is a prefix of the prefix, not vice versa.
    assert!(!schema_id_has_prefix("proxima-cod", "proxima-code/"));
    // Multibyte UTF-8: byte-wise comparison must still hold.
    assert!(schema_id_has_prefix("schémä/x", "schémä/"));
    assert!(!schema_id_has_prefix("sch", "schémä/"));
}

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
fn add_mcp_tool_lists_descriptor() {
    let mut registry = FlavorRegistry::new();
    registry.add_mcp_tool_or_panic_for_tests::<Demo>("proxima-test");
    let frozen = registry.freeze_or_panic_for_tests();
    let descriptors = frozen.list_mcp_tools();
    let names: Vec<_> = descriptors.iter().map(|d| d.name).collect();
    assert!(names.contains(&"proxima-test_demo"));
    let demo = descriptors
        .iter()
        .find(|d| d.name == "proxima-test_demo")
        .expect("demo descriptor");
    assert_eq!(
        demo.origin,
        McpToolOrigin::Flavor("proxima-test".to_string())
    );
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
fn opaque_citation_kinds_freeze() {
    let mut registry = FlavorRegistry::new();
    for (schema_id, kind) in [
        ("proxima-test/opaque-object", PayloadKind::CitedObject),
        ("proxima-test/opaque-mapping", PayloadKind::CitationMapping),
    ] {
        registry
            .try_add_opaque_schema(
                SchemaId::new(schema_id.to_string()),
                SchemaVersion::new(1),
                kind,
            )
            .expect("citation schemas may be opaque");
    }

    let frozen = registry
        .try_freeze()
        .expect("valid opaque citation schemas freeze");
    assert!(frozen.schemas().iter().any(|schema| {
        schema.schema_id.as_str() == "proxima-test/opaque-object"
            && schema.kind == PayloadKind::CitedObject
            && !schema.has_typed_ingress
    }));
    assert!(frozen.schemas().iter().any(|schema| {
        schema.schema_id.as_str() == "proxima-test/opaque-mapping"
            && schema.kind == PayloadKind::CitationMapping
            && !schema.has_typed_ingress
    }));
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

#[test]
fn default_registry_includes_all_11_substrate_mcp_tools() {
    let frozen = FlavorRegistry::new().freeze_or_panic_for_tests();
    let names: std::collections::HashSet<_> =
        frozen.list_mcp_tools().iter().map(|d| d.name).collect();
    let expected = [
        protocol_tool::CORE_SEARCH_MEMORIES,
        protocol_tool::CORE_MEMORY_SPACES,
        protocol_tool::CORE_REMEMBER,
        protocol_tool::CORE_RECORD_UTTERANCE,
        protocol_tool::CORE_DERIVE,
        protocol_tool::CORE_INTERPRET,
        protocol_tool::CORE_GOAL,
        protocol_tool::CORE_FACT,
        protocol_tool::CORE_MEMBERSHIP,
        protocol_tool::CORE_PUBLISH,
        protocol_tool::CORE_UPLOAD,
    ];
    for name in expected {
        assert!(names.contains(name), "missing tool {name}");
    }
    assert!(
        !names.contains("core/emit_budget_decision"),
        "retired tool name must not remain registered"
    );
    assert_eq!(names.len(), 11, "exactly 11 substrate tools registered");
    for desc in frozen.list_mcp_tools() {
        assert!(
            matches!(desc.origin, McpToolOrigin::Substrate),
            "default tool {} must be substrate-origin",
            desc.name
        );
    }
}
