# Add Your First MCP Tool

## Tool Boundary

A tool is a build-time registered call surface. It is not a runtime plugin, not
a table row, and not an autonomous action planner. External agents decide when
to call it; Proxima validates auth/owner/tool scope, decodes typed args, and
persists any effects through normal Fact/A/P/Goal write paths; no tool writes
an edge.

Do not add runtime registration endpoints.

The compiling witness is `flavors/code/src/mcp/`, served by `apps/proxima-mcp`.

## Args and Output Types

Flavor crates already depend on `futures` and `schemars`. Define an args type
with `Deserialize` and `JsonSchema`, and an output type with `Serialize`:

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExampleLookupArgs {
    #[schemars(description = "Stable external id to look up.")]
    pub external_id: String,
}

#[derive(Debug, serde::Serialize)]
pub struct ExampleLookupOutput {
    pub found: bool,
}
```

Field descriptions become the MCP schema. Keep schemas concrete and typed.

## Register at Build Time

Implement `McpTool` and register it in the flavor macro. The MCP
tool-authoring types are re-exported from `proxima::flavor` — flavor crates
import them there rather than reaching into `proxima_core::mcp`:

```rust
use proxima::flavor::{McpTool, McpToolCtx, McpToolError};

pub struct ExampleLookupTool;

impl McpTool for ExampleLookupTool {
    const NAME: &'static str = "my-flavor_lookup";
    const DESCRIPTION: &'static str = "Look up a my-flavor example row.";

    type Args = ExampleLookupArgs;
    type Output = ExampleLookupOutput;

    fn call(
        ctx: McpToolCtx,
        args: Self::Args,
    ) -> futures::future::BoxFuture<'static, Result<Self::Output, McpToolError>> {
        Box::pin(async move {
            let _owner = ctx.owner;
            let _external_id = args.external_id;
            Ok(ExampleLookupOutput { found: false })
        })
    }
}

proxima::flavor::proxima_flavor! {
    name = "my-flavor",
    display_name = "My Flavor",
    fact_schemas = [DocumentFiledV1],
    abstraction_schemas = [],
    perspective_schemas = [],
    goal_schemas = [],
    mcp_tools = [ExampleLookupTool],
}
```

Flavor MCP tool names use provider-safe `<flavor>_<tool>` names.

In-repo, add the tool under `flavors/code/src/mcp/` and list it in
`mcp_tools` in `flavors/code/src/lib.rs`.

## Use Strict Action Dispatch Only When Needed

Most tools should use one plain args struct. Use an internally tagged action enum
only when one tool intentionally exposes multiple actions. Action-dispatch tools
are flattened for client compatibility and validate allowed fields strictly; see
[../12-tool-manifest.md](../12-tool-manifest.md#action-dispatch-tools).

## Verify With tools/list and proxima://tools

```sh
cargo check -p proxima-code
cargo run -p proxima-mcp
```

The stock host links the code flavor by default. From that MCP server:

1. Call `tools/list`.
2. Read `proxima://tools`.
3. Confirm the tool appears only in binaries/profiles that include the flavor.
4. Confirm the input schema matches the Rust args type.

Live discovery is authoritative; static docs are only examples.
