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
with `Deserialize` and `JsonSchema`, and an output type with `Serialize` and `JsonSchema`:

```rust
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExampleLookupArgs {
    #[schemars(description = "Stable external id to look up.")]
    pub external_id: String,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ExampleLookupOutput {
    pub found: bool,
}
```

Field descriptions become the MCP schema. Keep schemas concrete and typed.

## Register at Build Time

Implement transport-neutral `Tool`; MCP and REST adapt the same implementation.
Import the authoring types from `proxima::flavor`:

```rust
use proxima::flavor::{FlavorContract, McpToolAnnotations, ProjectionDecl, Tool, ToolContract, ToolCtx, ToolError};

pub struct ExampleLookupTool;

impl Tool for ExampleLookupTool {
    const NAME: &'static str = "my-flavor_lookup";
    const DESCRIPTION: &'static str = "Look up a my-flavor example row.";
    const ANNOTATIONS: Option<McpToolAnnotations> = Some(McpToolAnnotations::new().read_only(true).idempotent(true));

    type Args = ExampleLookupArgs;
    type Output = ExampleLookupOutput;

    fn call(
        ctx: ToolCtx,
        args: Self::Args,
    ) -> futures::future::BoxFuture<'static, Result<Self::Output, ToolError>> {
        Box::pin(async move {
            let _owner = ctx.owner();
            let _external_id = args.external_id;
            Ok(ExampleLookupOutput { found: false })
        })
    }
}

const CONTRACT: FlavorContract = FlavorContract {
    flavor_id: "my-flavor",
    ordinal: 42, // choose an unused ordinal when adding this flavor to a host
    schemas: &[],
    state_surfaces: &[],
    scopes: &[],
    kernel_surfaces: &[],
    tools: &[ToolContract { wire_name: "my-flavor_lookup", actions: &[], idempotent: true }],
    resources: &[],
    projection: ProjectionDecl::None { why: "this lookup example declares no memory schemas" },
    bespoke_erase_legs: &[],
    bespoke_transfer_legs: &[],
};

proxima::flavor::proxima_flavor! {
    name = "my-flavor",
    display_name = "My Flavor",
    fact_schemas = [],
    abstraction_schemas = [],
    perspective_schemas = [],
    goal_schemas = [],
    mcp_tools = [ExampleLookupTool],
    contract = &CONTRACT,
}
```

For an existing flavor, add the tool to its existing macro and `CONTRACT.tools`
array. Keep its schema and storage declarations. The example above registers
a lookup tool without adding a memory schema.

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
