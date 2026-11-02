use proxiima::spiice::{
    FactPayload, PayloadKeyBuilder, SpiiceDescriptor, SpiiceProvenance, SpiiceRegistry,
    SpiiceRegistryError,
};
use proxiima_core::mcp::core_tools::SearchMemoriesTool;
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct ConformanceFact {
    id: Uuid,
}

impl FactPayload for ConformanceFact {
    const SCHEMA_ID: &'static str = "proxiima-conformance/fact-v1";
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
fn duplicate_schema_tool_and_spiice_return_typed_errors() {
    let mut registry = SpiiceRegistry::new();
    registry.try_add_fact_schema::<ConformanceFact>().unwrap();
    registry.try_add_fact_schema::<ConformanceFact>().unwrap();
    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(err, SpiiceRegistryError::DuplicateSchema { .. }));

    let mut registry = SpiiceRegistry::new();
    registry
        .try_add_mcp_tool::<SearchMemoriesTool>("core")
        .unwrap();
    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(err, SpiiceRegistryError::DuplicateTool { .. }));

    let mut registry = SpiiceRegistry::new();
    let descriptor = SpiiceDescriptor {
        spiice_id: "proxiima-conformance".to_string(),
        display_name: "Conformance".to_string(),
        package_version: "0.0.0".to_string(),
        author: None,
        provenance: SpiiceProvenance::Builtin,
    };
    registry.try_add_spiice(descriptor.clone()).unwrap();
    registry.try_add_spiice(descriptor).unwrap();
    let err = registry.try_freeze().unwrap_err();
    assert!(matches!(err, SpiiceRegistryError::DuplicateSpiice { .. }));
}

#[test]
fn host_and_spiice_sdk_imports_are_separate_and_compile() {
    fn assert_send_sync<T: Send + Sync>() {}
    fn accepts_fact<T: FactPayload>() {}

    assert_send_sync::<proxiima::RuntimeBuilder>();
    assert_send_sync::<proxiima::Engine>();
    accepts_fact::<ConformanceFact>();

    let _registry = SpiiceRegistry::new();
}

// A spiice MCP-tool author implements the tool via `proxiima::spiice`
// alone — no direct `proxiima_core::mcp` reach-through. This mirrors
// `docs/build-a-spiice.md`.
mod mcp_tool_authoring {
    use futures::future::BoxFuture;
    use proxiima::spiice::{Tool, ToolCtx, ToolError};

    #[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
    struct ExampleLookupArgs {
        external_id: String,
    }

    #[derive(Debug, serde::Serialize, schemars::JsonSchema)]
    struct ExampleLookupOutput {
        found: bool,
    }

    struct ExampleLookupTool;

    impl Tool for ExampleLookupTool {
        const TITLE: &'static str = "Test tool";
        const NAME: &'static str = "conformance_lookup";
        const DESCRIPTION: &'static str = "Look up a conformance example row.";

        type Args = ExampleLookupArgs;
        type Output = ExampleLookupOutput;

        fn call(
            ctx: ToolCtx,
            args: Self::Args,
        ) -> BoxFuture<'static, Result<Self::Output, ToolError>> {
            Box::pin(async move {
                let _ = (ctx.owner(), args.external_id);
                Ok(ExampleLookupOutput { found: false })
            })
        }
    }

    #[test]
    fn spiice_module_exposes_mcp_tool_authoring_surface() {
        fn assert_mcp_tool<T: Tool>() {}
        assert_mcp_tool::<ExampleLookupTool>();
        assert_eq!(ExampleLookupTool::NAME, "conformance_lookup");
    }
}

/// Zero-page-bound is reachable from the SDK: one `ToolError` helper, `?`
/// into `McpToolError` via `From`.
#[test]
fn the_zero_page_bound_rule_is_reachable_from_the_sdk() {
    use proxiima::host::McpToolError;
    use proxiima::spiice::{ToolError, reject_zero_limit};

    fn spiice_tool_body(limit: Option<u32>) -> Result<u32, ToolError> {
        reject_zero_limit(limit)?;
        Ok(limit.unwrap_or(10).min(50))
    }

    fn mcp_tool_body(limit: Option<u32>) -> Result<u32, McpToolError> {
        reject_zero_limit(limit)?;
        Ok(limit.unwrap_or(10).min(50))
    }

    assert!(matches!(
        spiice_tool_body(Some(0)),
        Err(ToolError::InvalidInput(_))
    ));
    assert!(matches!(
        mcp_tool_body(Some(0)),
        Err(McpToolError::InvalidInput(_))
    ));
    assert_eq!(spiice_tool_body(None).unwrap(), 10);
    assert_eq!(mcp_tool_body(Some(500)).unwrap(), 50);
}

/// The public parser uses the same UUID on transport-neutral tool paths;
/// trusted caller provenance remains available without a presentation service.
#[test]
fn wire_references_round_trip_through_a_transport_neutral_tool_ctx() {
    use proxiima::host::{AuthPath, AuthzContext, MemoryId, OwnerId, UserId, parse_wire_uuid};
    use proxiima::spiice::{ToolCaller, ToolCtx, ToolServices};

    let registry = std::sync::Arc::new(SpiiceRegistry::new().try_freeze().unwrap());
    let owner = OwnerId::from(UserId::new(Uuid::now_v7()));
    let ctx = ToolCtx::new(
        owner,
        AuthzContext::single_owner(&owner, AuthPath::System),
        registry,
        ToolServices::new(),
    )
    .with_caller(Some(
        ToolCaller::new("test/model", "test-client", "1.0")
            .with_trusted_model_id("acme/runner-v3")
            .expect("a well-formed runner id binds"),
    ));

    assert_eq!(
        ctx.caller(),
        Some(
            &ToolCaller::new("test/model", "test-client", "1.0")
                .with_trusted_model_id("acme/runner-v3")
                .expect("a well-formed runner id binds")
        )
    );

    // Trusted model provenance is visible to an out-of-tree spiice as a
    // field on the caller it already reads. `ToolCaller::new` keeps its
    // three-argument shape and binds nothing, so a spiice that never asks
    // for authenticated provenance is unaffected.
    let caller = ctx.caller().expect("caller attached");
    assert_eq!(caller.trusted_model_id.as_deref(), Some("acme/runner-v3"));
    assert_eq!(
        ToolCaller::new("test/model", "test-client", "1.0").trusted_model_id,
        None
    );

    let fact = MemoryId::new(Uuid::now_v7());
    let handle = fact.into_inner().to_string();
    assert_eq!(parse_wire_uuid(&handle).unwrap(), fact.into_inner());
    for raw in [format!("F:{handle}"), format!("remote:{handle}")] {
        assert!(matches!(
            parse_wire_uuid(&raw),
            Err(proxiima::host::McpToolError::InvalidInput(_))
        ));
    }
}
