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
| Host tools | host binary | `RuntimeBuilder::host_tools(Arc<dyn McpHostTools>)`, listed per caller | `ToolCall` through the registry's request behaviors |

`try_add_tool` delegates to `try_add_mcp_tool`: the blanket
`impl<T: Tool> McpTool for T` adapts the context and forwards `EFFECT`
and `ACTION_ARG_SPECS`, so a flavor dispatcher is registered, validated, and
gated exactly as a substrate one. Two registration bodies is what let those
two drift.

Stored ids:

| Surface | Id form |
|---|---|
| Core MCP projection | provider-safe registered names, currently `core_*` (for example `core_remember`, `core_goal`) |
| Flavor MCP projection | provider-safe `<flavor>_<name>` |

Host tools are not registered: they are listed per caller at request time,
flat, gated by their name and declared `effect`, and never shadow a
registry tool ([10 §MCP Endpoint and Authentication](10-configuration.md#mcp-endpoint-and-authentication)).
A palette's keys come from `McpToolDescriptor::palette_keys()` — the bare
name of a flat tool, `tool:action` for every action of either dispatcher
vocabulary — and `owner_only_keys()` is its owner-audience subset.

Registered MCP tool names are already provider-safe. Slash-separated
schema ids remain separate from MCP wire ids.

## Rust Surface

Flavor SDK tools:

```rust
pub trait Tool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];
    /// What a flat tool does (§Tool Effect). Required on a flat tool,
    /// refused on a dispatcher, whose actions each declare one.
    const EFFECT: Option<ToolEffect> = None;
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
    caller: Option<ToolCaller>,
    caller_self_perspective: Option<MemoryId>,
    services: ToolServices,
    engine: Option<Arc<Engine>>,
}

pub struct ToolCaller {
    pub model_id: String,              // operator label; may be caller-supplied
    pub trusted_model_id: Option<String>, // certified by the authenticated edge
    pub client_name: String,
    pub client_version: String,
}

pub struct McpToolDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub origin: McpToolOrigin,
    pub produces_schema_ids: &'static [&'static str],
    pub args_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub action_arg_specs: &'static [McpActionArgSpec],
    pub argv_action_specs: &'static [McpArgvActionSpec],
    pub effect: Option<ToolEffect>,       // flat tools only
    pub audience: McpToolAudience,
    pub call: McpCallFn,
}

pub struct McpActionArgSpec {
    pub action: &'static str,
    pub allowed_fields: &'static [&'static str],
    pub required_fields: &'static [&'static str],
    pub effect: ToolEffect,
    pub audience: McpToolAudience,
}
```

### Tool Effect

```rust
pub enum ToolEffect { ReadOnly, Additive(Replay), Destructive(Replay) }
pub enum Replay { Idempotent, NonIdempotent }
```

The one behaviour declaration: `Tool::EFFECT` on a flat tool,
`McpActionArgSpec::effect` / `McpArgvActionSpec::effect` on each action of a
dispatcher, `McpHostTool::effect` on a host tool. Five legal values; a read
carries no destructive or idempotent claim. Everything else is derived:

| Reader | Derived answer |
|---|---|
| owner-role gate, `tools/list` visibility | `ReadOnly` → `may_read`; else `may_write` |
| REST method | `ReadOnly` → `QUERY` + `POST`; else `POST` ([17 §Methods](17-rest-surface.md#methods-post-for-writes-query-for-reads)) |
| MCP / REST / `proxima://tools` hints | `McpToolAnnotations::registered(effect)`: `readOnlyHint`; `destructiveHint`, `idempotentHint` for a write only; `openWorldHint: false`. Host tools: `McpToolAnnotations::host(effect)`, no `openWorldHint` |
| `UnitOfWork::erase_own_series` | admits a tool call only when the dispatched action's effect is `Destructive` ([13 §Flavor-scoped erase](13-compliance.md#flavor-scoped-erase)) |

A dispatcher's tool-level effect is `ToolEffect::join` over its actions:
strength `ReadOnly < Additive < Destructive`, idempotent only when every
action is. One destructive action makes the tool destructive on the wire.
Per caller, the join runs over the actions that caller may see.

`ToolContract` states no behaviour: the contract names the tool and its
actions; the effect lives on the tool, once. `try_freeze` refuses a flat tool
with no `EFFECT` (`UndeclaredToolBehavior`) and a dispatcher that declares
one (`DispatcherToolEffect`).

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
`schemars`. Post-generation passes apply client-safe argument normalization
and object-root output normalization described below.

- Every MCP tool argument schema is produced by one function,
  `mcp_tool_schema<T: JsonSchema>()` in `crates/core/src/mcp/schema.rs`.
- The emitted schema is JSON Schema draft 2020-12 and **`$ref`-free /
  `$defs`-free**.
- Field descriptions originate only from the Rust type: a `///`
  doc-comment or `#[schemars(description = "...")]`. A doc comment's hard
  wraps are rejoined; blank lines and list items keep their break.
- A recursive tool argument type is a registration error.
- **The wire is plain JSON Schema** — `tools/list`, `GET /v1/tools`:
  no `x-` keyword, no `$schema` (MCP's default dialect is 2020-12), no root
  `title` (a Rust type name), no non-standard `format` (`uint32`, `int64`,
  …; bounds stay as `minimum`). Budgeted by
  `apps/proxima-mcp/tests/end_to_end.rs` (`TOOLS_LIST_BUDGET`).
- A tool's `Output` type is its output schema, produced the same way by
  `mcp_output_schema<T: JsonSchema>()` — under the **serialize** contract,
  so a `#[serde(skip_serializing_if)]` field is optional — and carried on
  `McpToolDescriptor.output_schema`. It is a sibling of `mcp_tool_schema`,
  not a caller of it: the action-dispatch normalization below is an
  argument-side pass. MCP requires an object output root: object-only
  `anyOf` / `oneOf` unions gain `type: "object"` without changing their
  branches. Non-object outputs and recursion fail registration; use an
  empty struct instead of `()`. Host schemas use the same rule; invalid
  host tools are omitted with a warning. The handler refuses non-object
  `structuredContent` as an internal error.
- MCP `outputSchema` is `mcp_wire_output_schema(output_schema)`: validation
  keywords only — no `description`, `title`, `examples`, `default`. Model
  APIs carry a tool's name, description and input schema, never its output
  schema, so output prose is payload; the registry keeps the documented
  schema for the REST OpenAPI document. Host tools are projected the same
  way.
- Tool outputs are *also* advertised by registered-schema-id reference
  (`McpToolDescriptor.produces_schema_ids`) and resolved against the
  `FlavorRegistry`. The two answer different questions: `output_schema` is
  the reply envelope, `produces_schema_ids` are the registry payloads the
  call writes.

### Action-Dispatch Tools

A dispatcher is any tool whose argument type is an internally-tagged enum
**and** which declares `ACTION_ARG_SPECS`. The substrate ships five —
`core_goal`, `core_fact`, `core_membership`, `core_transfer`, `core_upload` —
and a flavor declares its own the same way, through `proxima_flavor!`.

Registration derives a typed per-action contract from the `Args` enum —
`McpToolDescriptor.dispatcher_schema: Option<McpDispatcherSchema>`, one
`McpActionSchema { action, description, argument_schema }` per variant —
and renders the client-facing `inputSchema` from it, because MCP clients
reject a root that is not `type: object` or carries `oneOf`/`anyOf`/`allOf`:

```text
{ type: object, required: [action], additionalProperties: false,
  properties: {
    action: { enum: [..], description: guide },   one line per action:
                                                  "- set: <variant doc>
                                                   Required: a, b. Optional: c."
    <field>: as declared, when every action naming it agrees;
             else nullability widened, prose "Depends on `action`:
             - set, modify: <text>\n- mark_achieved: <text>" } }
```

- `McpDispatcherSchema::render(permitted)` is the one renderer: whole at
  registration (`args_schema`), narrowed per caller by
  `McpToolDescriptor::input_schema`, so a caller's enum, field set, guide
  and per-action prose name only actions it may run.
- Action prose is the variant's doc comment, substrate and flavor alike.
- `proxima://tools` and the OpenAPI action operations read
  `McpActionSchema.argument_schema` and its derived field sets.
- Registration refuses a field two actions give incompatible schemas.
- **Argument validation is strict and pre-decode**, for every dispatcher
  including a flavor's. Before an action's arguments are deserialized, any
  field outside that action's `allowed_fields`, or a missing
  `required_field`, is rejected with JSON-RPC `-32602`. Unknown fields are an
  error, not silently dropped.

When a conditional `if` branch tests a property's value, its schema must also
list that property in `required` when absence is not meant to match. JSON
Schema's `properties` keyword validates only present properties; the explicit
requirement keeps an absent condition field from entering the wrong branch.
An action-root applicator may omit `type` because the closed action root
already fixes the instance to an object; an explicit non-object type cannot
also introduce action-root `properties` or `required` fields.

**The discriminator must literally be `action`.** Not a style preference:
`ToolScope` keys are spelled `"{tool}:{action}"`, `validate_action_args` and
`ScopeGateBehavior::enforce_scope` both read `args["action"]`, and the REST
narrowed route injects `"action"` into the body before dispatch. A dispatcher
tagged on anything else would be enumerated correctly and then gated,
validated, and routed as if it had no actions at all, so `try_freeze` refuses
to seal a registry containing one.

Three carriers, split authority:

| Surface | What it is |
|---|---|
| `McpToolDescriptor.action_arg_specs` | THE enumeration and per-action behavior authority. Every scope/role gate, catalog, REST method gate, and OpenAPI operation reads its `effect`. |
| `McpToolDescriptor.dispatcher_schema` | Derived from the `Args` type by the schema pass: per action, the variant description and the action-only `argument_schema` its field sets derive from. |
| `CoreActionMeta` | Substrate-only decoration: per-action scope key and produced schema ids. Never an existence or behavior claim. |

`FlavorRegistry::try_freeze` refuses a registry where the first two disagree
(see [08 §Freeze Guards](08-core-and-flavors.md#freeze-guards)).

Per-action behavior has no parent to inherit — a dispatcher declares no
`EFFECT` — and does not consult `CoreActionMeta`. `McpActionArgSpec.effect`
is the sole answer for substrate and flavor dispatchers; anything but
`ReadOnly` is write authorization and `POST`-only REST exposure. Mixed
read/write dispatchers therefore admit a viewer and `QUERY` only on the
read-only action.

An action's enum-variant doc comment becomes
`McpActionSchema.description`; the action guide, the tool catalog and the
OpenAPI action operation render that derived text. No second description
constant or runtime registry exists.

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
    -> adapter maps McpAuthorContext into ToolCaller
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
| field vocabulary | host-defined; 13 no longer specifies one (see [13 §Declared metadata](13-compliance.md#declared-metadata)) |
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
