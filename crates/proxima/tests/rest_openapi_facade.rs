#![cfg(feature = "rest")]

use futures::future::BoxFuture;
use proxima::flavor::{FlavorRegistry, McpTool, McpToolAnnotations, McpToolCtx, McpToolError};

#[derive(schemars::JsonSchema, serde::Deserialize)]
struct OfflineArgs {
    query: String,
}

#[derive(schemars::JsonSchema, serde::Serialize)]
struct OfflineOutput {
    found: bool,
}

struct OfflineLookup;

impl McpTool for OfflineLookup {
    const NAME: &'static str = "offline_lookup";
    const DESCRIPTION: &'static str = "Lookup used to prove the facade projects tools.";
    const ANNOTATIONS: Option<McpToolAnnotations> =
        Some(McpToolAnnotations::new().read_only(true).open_world(false));

    type Args = OfflineArgs;
    type Output = OfflineOutput;

    fn call(
        _ctx: McpToolCtx,
        args: Self::Args,
    ) -> BoxFuture<'static, Result<Self::Output, McpToolError>> {
        Box::pin(async move {
            Ok(OfflineOutput {
                found: !args.query.is_empty(),
            })
        })
    }
}

#[test]
fn host_facade_builds_the_complete_offline_rest_document() {
    let mut registry = FlavorRegistry::new();
    registry
        .try_add_mcp_tool::<OfflineLookup>("offline")
        .expect("tool registers");
    let registry = registry.try_freeze().expect("tool registry freezes");

    let document =
        proxima::host::build_openapi_document(&registry, Some("https://proxima.example.com"));

    assert_eq!(document["openapi"], "3.2.0");
    assert_eq!(document["servers"][0]["url"], "https://proxima.example.com");
    assert!(
        document["paths"]
            .as_object()
            .is_some_and(|paths| paths.contains_key("/v1/resources/schemas")),
        "core resources are supplied by the facade: {document:#}",
    );
    assert!(
        document["paths"]
            .as_object()
            .is_some_and(|paths| paths.contains_key("/v1/tools/offline_lookup")),
        "frozen tools are supplied by the facade: {document:#}",
    );
}
