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

// A flavor MCP-tool author implements the tool via `proxima::flavor`
// alone — no direct `proxima_core::mcp` reach-through. This mirrors
// `docs/tutorials/add-first-mcp-tool.md`.
mod mcp_tool_authoring {
    use futures::future::BoxFuture;
    use proxima::flavor::{McpTool, McpToolCtx, McpToolError};

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct ExampleLookupArgs {
        external_id: String,
    }

    #[derive(Debug, serde::Serialize)]
    struct ExampleLookupOutput {
        found: bool,
    }

    struct ExampleLookupTool;

    impl McpTool for ExampleLookupTool {
        const NAME: &'static str = "conformance_lookup";
        const DESCRIPTION: &'static str = "Look up a conformance example row.";

        type Args = ExampleLookupArgs;
        type Output = ExampleLookupOutput;

        fn call(
            ctx: McpToolCtx,
            args: Self::Args,
        ) -> BoxFuture<'static, Result<Self::Output, McpToolError>> {
            Box::pin(async move {
                let _ = (ctx.owner, args.external_id);
                Ok(ExampleLookupOutput { found: false })
            })
        }
    }

    #[test]
    fn flavor_module_exposes_mcp_tool_authoring_surface() {
        fn assert_mcp_tool<T: McpTool>() {}
        assert_mcp_tool::<ExampleLookupTool>();
        assert_eq!(ExampleLookupTool::NAME, "conformance_lookup");
    }
}

/// An out-of-tree flavor can reach the zero-page-bound rule through the
/// SDK, in both of the error types the two tool traits use.
///
/// The rule was implemented twice before this — once in core for `McpTool`
/// and once in the code flavor for `Tool` — because the helper was
/// `pub(crate)` and the two traits carry different error enums. A flavor
/// that depends on `proxima` alone had no route to either copy, so the
/// third implementation would have been someone else's, spelled
/// differently. One `ToolError`-returning function serves both: `?`
/// promotes it through `From<ToolError> for McpToolError`.
#[test]
fn the_zero_page_bound_rule_is_reachable_from_the_sdk() {
    use proxima::flavor::{McpToolError, ToolError, reject_zero_limit};

    fn flavor_tool_body(limit: Option<u32>) -> Result<u32, ToolError> {
        reject_zero_limit(limit)?;
        Ok(limit.unwrap_or(10).min(50))
    }

    fn mcp_tool_body(limit: Option<u32>) -> Result<u32, McpToolError> {
        reject_zero_limit(limit)?;
        Ok(limit.unwrap_or(10).min(50))
    }

    assert!(matches!(
        flavor_tool_body(Some(0)),
        Err(ToolError::InvalidInput(_))
    ));
    assert!(matches!(
        mcp_tool_body(Some(0)),
        Err(McpToolError::InvalidInput(_))
    ));
    assert_eq!(flavor_tool_body(None).unwrap(), 10);
    assert_eq!(mcp_tool_body(Some(500)).unwrap(), 50);
}
