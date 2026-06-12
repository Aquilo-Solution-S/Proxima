# 12 — Tool Manifest

## Claim

Tool = build-time registered call surface + wake-entry palette entry.

| Rule | Contract |
|---|---|
| Registration | core/flavor crates only; frozen in `FlavorRegistry` at startup |
| Selection | wake entries choose allowed ids with `substrate_tool_palette` |
| Execution | internal to the composite binary and wake harness |
| Persistence | normal Fact / Edge / Goal paths only |
| Observation | clients observe change events and stored entities, not a Tool entity |

No runtime registration tier. No install/revoke API. No `tools` table.

## Tool Classes

| Class | Owner | Vocabulary | Wake field | Dispatch |
|---|---|---|---|---|
| Substrate personality tools | core | `substrate_pack()` | `substrate_tool_palette` | `PersonalityToolContext` |
| Core MCP config tools | core | `add_substrate_mcp_tool<T>()` | `substrate_tool_palette` | `McpToolCtx` |
| Flavor MCP tools | flavor crate | `add_mcp_tool<T>(flavor_id)` | `substrate_tool_palette` | `McpToolCtx` |

Stored ids:

| Surface | Id form |
|---|---|
| Substrate personality tool | `core/<name>` or scoped emit id |
| Core MCP config tool | `core/<name>` |
| Flavor MCP tool | `<flavor>/<name>` |

Provider-facing names are derived per invocation with
`provider_safe_tool_name(canonical)`. The harness keeps a reverse map
from provider-safe name to canonical id before dispatch.

## Rust Surface

MCP tools:

```rust
pub trait McpTool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];

    type Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static;
    type Output: serde::Serialize + Send + 'static;

    fn call(
        ctx: McpToolCtx,
        args: Self::Args,
    ) -> BoxFuture<'static, Result<Self::Output, McpToolError>>;
}

pub struct McpToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub produces_schema_ids: &'static [&'static str],
    pub args_schema: serde_json::Value,
    pub call: McpCallFn,
}
```

Registration:

```rust
impl FlavorRegistry {
    pub(crate) fn add_substrate_mcp_tool<T: McpTool>(&mut self);
    pub fn add_mcp_tool<T: McpTool>(&mut self, expected_prefix: &str);
    pub fn freeze(self) -> FlavorRegistryFrozen;
}

impl FlavorRegistryFrozen {
    pub fn list_mcp_tools(&self) -> &[McpToolDescriptor];
    pub fn mcp_tool_ids(&self) -> HashSet<String>;
}
```

Prefix rules live in 08:

| Tool owner | Prefix |
|---|---|
| substrate MCP tool | `core/` |
| flavor MCP tool | `<flavor>/` |

`FlavorRegistry::freeze()` rejects duplicate MCP tool names. Schema and
relation validation remains the registry's build-time responsibility
(see 08 §Freeze Guards).

## Tool Schema Contract

A tool's argument type *is* its schema. Shape, field descriptions,
required/optional, and enum variants all derive from the Rust type via
`schemars`. No code mutates the generated schema out of band.

- Every MCP tool argument schema is produced by one function,
  `mcp_tool_schema<T: JsonSchema>()` in `crates/core/src/mcp/schema.rs`.
- The emitted schema is JSON Schema draft 2020-12 and **`$ref`-free /
  `$defs`-free**.
- Field descriptions originate only from the Rust type: a `///`
  doc-comment or `#[schemars(description = "...")]`.
- A recursive tool argument type is a registration error.
- Tool outputs are advertised by registered-schema-id reference
  (`McpToolDescriptor.produces_schema_ids`) and resolved against the
  `FlavorRegistry`.

## Wake-Entry Selection

Wake entry fields:

```rust
pub struct WakeEntryDraft {
    pub substrate_tool_palette: Vec<String>,
    pub required_produced_schema_ids: Vec<String>,
    pub execution_mode: WakeExecutionMode, // v1: SubstrateOnly
    pub trigger_id: String,
}
```

Write-time validation:

| Field | Accepted ids |
|---|---|
| `substrate_tool_palette` | `registry.mcp_tool_ids() ∪ substrate_pack().tool_id()` |
| `required_produced_schema_ids` | schema ids produced by the selected palette |

Scoped emit ids are accepted when their base tool is registered and their
target payload schema exists:

```
core/emit_abstraction::<schema_id>::v<schema_version>
core/emit_perspective::<schema_id>::v<schema_version>
```

`WakeExecutionMode` currently has one value: `substrate_only`.

## Invocation Flow

```
WakeEntry
  substrate_tool_palette
        |
        v
fire_wake_entry
  mint wake_token(owner, palette, root Self, trigger)
  build HarnessProgram
        |
        v
HarnessLoop
  resolve substrate palette through HarnessSubstrateBridge
  map canonical ids -> provider-safe names
  expose tool specs to provider
        |
        v
tool call
  provider-safe name -> canonical id
  dispatch by binding
        |
        +--> McpToolDescriptor.call(McpToolCtx, args)
        +--> PersonalityTool.invoke(PersonalityToolContext, args)
```

Substrate MCP dispatch:

| Step | Contract |
|---|---|
| Auth | wake-token or master-token context |
| Owner | from auth context; Owner = principal, org is a billing annotation only (doc 01) |
| Args | JSON decoded into `McpTool::Args` |
| Output | serialized `McpTool::Output` |
| Handles | wake-token calls use `OutputMode::Handles`; master-token calls use raw ids |

Substrate personality dispatch:

| Step | Contract |
|---|---|
| Auth | wake-token required |
| Palette | canonical id must be present in wake palette |
| Context | root Self Perspective, trigger memory, wake depth, read log |
| Writes | only schemas/relations permitted by palette-derived masks |

## Persistence

Current storage:

| Data | Storage |
|---|---|
| wake tool selection | `personality_wake_entries.substrate_tool_palette` |
| durable-result requirement | `personality_wake_entries.required_produced_schema_ids` |
| wake invocation status | `personality_wake_invocations` |
| wake tool-call log tails | `personality_wake_invocation_logs` |
| tool effects | `memories`, sidecar tables, `edges`, `goals`, change events |

Not present in v1:

| Table / API | Status |
|---|---|
| `tools` | absent |
| per-tool invocation table | absent |
| runtime install API | absent |
| runtime manifest upload | absent |
| signed external tool body registry | deferred |

Tool output that persists must pass the same registered schema,
relation, Owner, layering, citation, and append-only checks as any other
engine write.

## Compliance Metadata

Design-intent fields for external-effect tools:

| Field | Purpose |
|---|---|
| `data_residency: Region` | third-country / region check for data leaving the substrate |
| `recipients: Vec<RecipientId>` | Art. 19 recipient notification inventory |
| `legal_consequence: bool` | Art. 22 human-approval gate |

Declared / deferred:

| Item | v1 status |
|---|---|
| field vocabulary | defined in [13 §Compliance vocabulary](13-compliance.md#compliance-vocabulary) |
| field placement on tool descriptors | deferred |
| startup failure for missing fields | deferred |
| Owner residency allowlist enforcement for tool calls | deferred |
| recipient export from tool-call records | deferred; no per-tool invocation table |
| `legal_consequence` automatic wake blocking | deferred; human-approval pattern remains required design intent |

Until these fields land on the current `McpToolDescriptor` surface,
docs must not claim implemented storage or runtime enforcement.
Owner-policy enforcement belongs to 13.

## Non-Goals

- No runtime schema, relation, source, prompt, or tool registration.
- No dynamic tool install path in v1.
- No OpenAI-function manifest as substrate authority.
- No MCP capability model as substrate authority.
- No generic external HTTP/WASM body transport in v1.
- No tool-specific entity lifecycle.
- No direct A/P persistence bypassing 04.
