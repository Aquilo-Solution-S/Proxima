# 12 — Tool Manifest

> **Status:** current + deferred sections. Deferred rows are design intent, not implementation claims.

## Claim

Tool = build-time registered call surface.

| Rule | Contract |
|---|---|
| Registration | core/flavor crates only; frozen in `FlavorRegistry` at startup |
| Selection | auth token tool scope ∩ deployment tool-surface profile |
| Execution | MCP dispatch; external harnesses drive decisions |
| Persistence | normal Fact / A/P / Goal write paths only; no tool writes an edge |
| Observation | clients observe change events and stored entities, not a Tool entity |

No runtime registration tier. No install/revoke API. No `tools` table.

## Tool Classes

| Class | Owner | Vocabulary | Dispatch |
|---|---|---|---|
| Core tools | core | internal `try_add_mcp_tool<T>("core")` adapter | `McpToolCtx` |
| Flavor tools | flavor crate | `try_add_tool<T>(prefix)` | `ToolCtx` |

`try_add_tool` delegates to `try_add_mcp_tool`: the blanket
`impl<T: Tool> McpTool for T` adapts the context and forwards `ANNOTATIONS`
and `ACTION_ARG_SPECS`, so a flavor dispatcher is registered, validated, and
gated exactly as a substrate one. Two registration bodies is what let those
two drift.

Stored ids:

| Surface | Id form |
|---|---|
| Core MCP projection | provider-safe registered names, currently `core_*` (for example `core_remember`, `core_goal`) |
| Flavor MCP projection | provider-safe `<flavor>_<name>` |

Registered MCP tool names are already provider-safe. Slash-separated
schema ids remain separate from MCP wire ids.

## Rust Surface

Flavor SDK tools:

```rust
pub trait Tool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];
    /// MCP behaviour hints. Not cosmetic: the owner-role gate asks whether a
    /// tool is read-only and demands WRITE when it cannot tell, so a read
    /// tool that declares nothing is refused to every read-only role.
    /// `try_freeze` refuses a registry containing a tool that says nothing.
    const ANNOTATIONS: Option<McpToolAnnotations> = None;
    /// The actions this tool dispatches, or `&[]` for a flat tool. THE
    /// enumeration of a dispatcher's action set — the scope gate, the tool
    /// catalog, the REST action routes, and the OpenAPI document all read it
    /// off `McpToolDescriptor::action_arg_specs`. Declaring it turns a tool
    /// into a dispatcher: its `Args` must be an internally tagged enum
    /// tagged on `action`, its arguments are validated per action before
    /// decode, and its scope keys become `tool:action` leaves rather than the
    /// bare tool name.
    const ACTION_ARG_SPECS: &'static [McpActionArgSpec] = &[];

    type Args: serde::de::DeserializeOwned + schemars::JsonSchema + Send + 'static;
    type Output: serde::Serialize + schemars::JsonSchema + Send + 'static;

    fn call(
        ctx: ToolCtx,
        args: Self::Args,
    ) -> BoxFuture<'static, Result<Self::Output, ToolError>>;
}

pub struct ToolCtx {
    owner: Owner,
    authz: AuthzContext,
    registry: Arc<FlavorRegistryFrozen>,
    caller_self_perspective: Option<MemoryId>,
    services: ToolServices,
    engine: Option<Arc<Engine>>,
}

pub struct ToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub produces_schema_ids: &'static [&'static str],
    pub args_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub call: McpCallFn,
}
```

Registration:

```rust
impl FlavorRegistry {
    pub fn try_add_tool<T: Tool>(&mut self, expected_prefix: &str) -> Result<(), FlavorRegistryError>;
    pub fn try_freeze(self) -> Result<FlavorRegistryFrozen, FlavorRegistryError>;
}

impl FlavorRegistryFrozen {
    pub fn list_mcp_tools(&self) -> &[McpToolDescriptor];
    pub fn mcp_tool_ids(&self) -> HashSet<String>;
}
```

Prefix rules live in 08:

| Tool owner | Prefix |
|---|---|
| substrate MCP tool | `core_` |
| flavor MCP tool | `<flavor>_` |

`FlavorRegistry::try_freeze()` rejects duplicate tool names. Schema
validation remains the registry's build-time responsibility
(see 08 §Freeze Guards).

## Tool Schema Contract

A tool's argument type *is* its schema. Shape, field descriptions,
required/optional, and enum variants all derive from the Rust type via
`schemars`. The only sanctioned post-generation pass is the client-safe
normalization of action-dispatch tools described below.

- Every MCP tool argument schema is produced by one function,
  `mcp_tool_schema<T: JsonSchema>()` in `crates/core/src/mcp/schema.rs`.
- The emitted schema is JSON Schema draft 2020-12 and **`$ref`-free /
  `$defs`-free**.
- Field descriptions originate only from the Rust type: a `///`
  doc-comment or `#[schemars(description = "...")]`.
- A recursive tool argument type is a registration error.
- A tool's `Output` type is its output schema, produced the same way by
  `mcp_output_schema<T: JsonSchema>()` and carried on
  `McpToolDescriptor.output_schema` / MCP `outputSchema`. It is a sibling
  of `mcp_tool_schema`, not a caller of it: the action-dispatch
  normalization below is an argument-side pass, and an output union stays a
  union. Recursion is a registration error at this end too.
- Tool outputs are *also* advertised by registered-schema-id reference
  (`McpToolDescriptor.produces_schema_ids`) and resolved against the
  `FlavorRegistry`. The two answer different questions: `output_schema` is
  the reply envelope, `produces_schema_ids` are the registry payloads the
  call writes.

### Action-Dispatch Tools

A dispatcher is any tool whose argument type is an internally-tagged enum
**and** which declares `ACTION_ARG_SPECS`. The substrate ships five —
`core_goal`, `core_fact`, `core_membership`, `core_publish`, `core_upload` —
and a flavor declares its own the same way, through `proxima_flavor!`.

Their argument schema is normalized into a client-safe shape after
`schemars` generation, because MCP clients reject an `inputSchema` whose root
is not `type: object` or that carries a root `oneOf`/`anyOf`/`allOf`:

- The per-variant `oneOf` is flattened into one object: a unioned top-level
  `properties` map, an `action` string-enum discriminator, and
  `additionalProperties: false`.
- Per-action field metadata is published under the `x-proxima-actions`
  schema extension — `allowed_fields`, `required_fields`, and
  `field_descriptions` keyed by action — and mirrored in the
  `proxima://tools` catalog. Fields shared across actions carry a neutral
  root description that points back to this metadata.
- **Argument validation is strict and pre-decode**, for every dispatcher
  including a flavor's. Before an action's arguments are deserialized, any
  field outside that action's `allowed_fields`, or a missing
  `required_field`, is rejected with JSON-RPC `-32602`. Unknown fields are an
  error, not silently dropped.

**The discriminator must literally be `action`.** Not a style preference:
`ToolScope` keys are spelled `"{tool}:{action}"`, `validate_action_args` and
`ScopeGateBehavior::enforce_scope` both read `args["action"]`, and the REST
narrowed route injects `"action"` into the body before dispatch. A dispatcher
tagged on anything else would be enumerated correctly and then gated,
validated, and routed as if it had no actions at all, so `try_freeze` refuses
to seal a registry containing one.

Three descriptions, one authority:

| Surface | What it is |
|---|---|
| `McpToolDescriptor.action_arg_specs` | THE enumeration. Every seam that asks "which actions does this tool have" reads this. |
| `x-proxima-actions` | Derived from the `Args` type by the schema pass; the client-facing description, and the only one carrying per-field prose. |
| `CoreActionMeta` | Substrate-only decoration: per-action scope key, description, produced schema ids, annotations. Never an existence claim. |

`FlavorRegistry::try_freeze` refuses a registry where the first two disagree
(see [08 §Freeze Guards](08-core-and-flavors.md#freeze-guards)).

#### Known gaps for flavor dispatchers

Two pieces of per-action decoration are substrate-only today. Both are
stated here rather than left to be discovered:

- **Per-action annotations.** `CoreActionMeta.annotations` is what gives
  `core_membership:list_members` a read-only answer under a write-annotated
  parent. A flavor dispatcher has no such table, so every one of its actions
  resolves read/write at *tool* level — in the owner-role gate, in the
  `proxima://tools` catalog, and in the REST `POST` vs `QUERY` choice. The
  hazard is the same one per-action resolution exists to close: a flavor
  dispatcher annotated `read_only` at tool level offers `QUERY` — safe, and
  auto-retryable by any proxy or client library — on a write action added to
  it later, and admits a viewer role to it besides. That hazard is
  unreachable rather than merely documented: `try_freeze` refuses a
  flavor-origin dispatcher that declares `read_only(true)` at tool level, so
  the declaration cannot be made until per-action annotations exist to
  qualify it. The fix direction is a per-action annotation slot on
  `McpActionArgSpec`, which every reader already has in hand.
- **Per-action description.** A flavor action's `description` in the catalog
  is the empty string. The fix direction is to lift the enum variant's own
  doc comment into `x-proxima-actions` in `merge_variant` (the schema pass
  already reads per-field descriptions there), so it costs the flavor author
  nothing beyond documenting the variant.

## Goal Wake Config

Goal wake fields are stored on the Goal-owned wake config carrier, not as a
standalone runtime entity:

```rust
pub struct GoalWakeConfigWrite {
    trigger: GoalWakeTrigger,
    tool_ids: Vec<GoalWakeToolId>,
    prompt: String,
    hard_memory_ids: Vec<MemoryId>,
}
```

Write-time validation:

| Field | Contract |
|---|---|
| `trigger` | exact Fact memory or Fact schema/version selector |
| `tool_ids` | non-empty canonical provider-safe ids or exact action leaf scope keys registered in the frozen build-time registry |
| `prompt` | non-empty bounded text |
| `hard_memory_ids` | unique memory ids; candidate admission checks actual owner/kind readability |

Goal-owned WakeConfig carries trigger, bounded toolset, prompt, and hard-memory context only. Model/run policy stays external.

## Invocation Flow

Live MCP dispatch:

```
MCP request
  provider-safe name
    -> canonical id
    -> McpToolDescriptor.call(McpToolCtx, args)
    -> Tool::call(ToolCtx, args) for generic SDK tools
```

Proxima is a passive brain hub. External harnesses own model choice,
tool planning, execution policy, and cursors.

MCP dispatch contract:

| Step | Contract |
|---|---|
| Auth | host `Authenticator` resolves `UserId` through current `OwnerRoles` |
| Owner | selected at session initialize, bound server-side, checked against the freshly authenticated `OwnerRoles` on every request |
| Tool scope | token capabilities intersected with deployment profile and bound-owner role |
| Args | action-dispatch tools validate fields strictly (see Tool Schema Contract), then JSON decoded into typed args |
| Output | serialized typed output, mirrored into MCP `structuredContent` and validatable against the tool's `outputSchema` |
| Ids | prefixed ids (`F:`/`A:`/`P:`/`G:` form) — the only wire reference grammar. There is no `E:`: an edge has no id to name. |

## Persistence

Current storage:

| Data | Storage |
|---|---|
| tool effects | `memories`, sidecar tables, `goals`, change events, and the `edges` rows those writes imply |

Not present in v1:

| Table / API | Status |
|---|---|
| `tools` | absent |
| per-tool invocation table | absent |
| per-wake invocation table | absent |
| generic per-wake invocation/tool-call storage | absent |
| runtime install API | absent |
| runtime manifest upload | absent |
| signed external tool body registry | deferred |

Tool output that persists must pass the same registered schema, Owner,
layering, citation, and append-only checks as any other engine write.

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

- No runtime schema, source, prompt, or tool registration.
- No connection vocabulary at all — the edge kinds are closed and no tool
  writes one.
- No dynamic tool install path in v1.
- No OpenAI-function manifest as substrate authority.
- No MCP capability model as substrate authority.
- No generic external HTTP/WASM body transport in v1.
- No tool-specific entity lifecycle.
- No direct A/P persistence bypassing 04.
