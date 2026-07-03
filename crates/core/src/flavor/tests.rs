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
        PayloadKind::Fact,
    );
    registry.add_opaque_schema_or_panic_for_tests(
        schema_id,
        SchemaVersion::new(1),
        PayloadKind::Fact,
    );
    let err = registry
        .try_freeze()
        .expect_err("duplicate schema must fail");
    assert!(matches!(err, FlavorRegistryError::DuplicateSchema { .. }));
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
fn freeze_rejects_unsatisfiable_required_tag_relation() {
    let mut registry = FlavorRegistry::new();
    registry.add_opaque_schema_or_panic_for_tests(
        SchemaId::new("proxima-test/plain-fact".to_string()),
        SchemaVersion::new(1),
        PayloadKind::Fact,
    );
    registry.add_relation_or_panic_for_tests(
        RelationDescriptor::substrate(
            "proxima-test/requires-actor",
            crate::RelationClass::Structural,
            crate::EndpointBinding::Pin,
            crate::EndpointBinding::Pin,
            crate::EntityKindMask::fact(),
            crate::EntityKindMask::fact(),
            crate::AuthorshipKindMask::external_agent(),
        )
        .with_required_tags(&[], &["actor"]),
    );
    let err = registry
        .try_freeze()
        .expect_err("unsatisfiable required tags must fail");
    assert!(matches!(
        err,
        FlavorRegistryError::UnsatisfiableRelationTags { side: "target", .. }
    ));
}

#[test]
fn freeze_rejects_duplicate_relation_names() {
    let mut registry = FlavorRegistry::new();
    let duplicate_core_relation = core_relation_descriptors()
        .into_iter()
        .next()
        .expect("core relation descriptors are seeded");
    registry.add_relation_or_panic_for_tests(duplicate_core_relation);
    let err = registry
        .try_freeze()
        .expect_err("duplicate relation must fail");
    assert!(matches!(err, FlavorRegistryError::DuplicateRelation { .. }));
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
fn default_registry_includes_all_10_substrate_mcp_tools() {
    let frozen = FlavorRegistry::new().freeze_or_panic_for_tests();
    let names: std::collections::HashSet<_> =
        frozen.list_mcp_tools().iter().map(|d| d.name).collect();
    let expected = [
        protocol_tool::CORE_SEARCH_MEMORIES,
        protocol_tool::CORE_MEMORY_SPACES,
        protocol_tool::CORE_REMEMBER,
        protocol_tool::CORE_RECORD_UTTERANCE,
        protocol_tool::CORE_DERIVE,
        protocol_tool::CORE_LINK,
        protocol_tool::CORE_GOAL,
        protocol_tool::CORE_FACT,
        protocol_tool::CORE_MEMBERSHIP,
        protocol_tool::CORE_PUBLISH,
    ];
    for name in expected {
        assert!(names.contains(name), "missing tool {name}");
    }
    assert!(
        !names.contains("core/emit_budget_decision"),
        "retired tool name must not remain registered"
    );
    assert_eq!(names.len(), 10, "exactly 10 substrate tools registered");
    for desc in frozen.list_mcp_tools() {
        assert!(
            matches!(desc.origin, McpToolOrigin::Substrate),
            "default tool {} must be substrate-origin",
            desc.name
        );
    }
}
