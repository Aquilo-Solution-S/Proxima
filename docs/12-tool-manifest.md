# 12 — Tool Manifest

## Claim

Tool = build-time registered call surface.

| Rule | Contract |
|---|---|
| Registration | core/flavor crates only; frozen in `FlavorRegistry` at startup |
| Selection | auth token tool scope ∩ deployment tool-surface profile |
| Execution | MCP dispatch; external harnesses drive decisions |
| Persistence | normal Fact / Edge / Goal paths only |
| Observation | clients observe change events and stored entities, not a Tool entity |

No runtime registration tier. No install/revoke API. No `tools` table.

## Tool Classes

| Class | Owner | Vocabulary | Dispatch |
|---|---|---|---|
| Core MCP tools | core | `add_substrate_mcp_tool<T>()` | `McpToolCtx` |
| Flavor MCP tools | flavor crate | `add_mcp_tool<T>(prefix)` | `McpToolCtx` |

Stored ids:

| Surface | Id form |
|---|---|
| Core MCP tool | provider-safe registered names, currently `core_*` (for example `core_remember`, `core_goal`) |
| Flavor MCP tool | provider-safe `<flavor>_<name>` |

Registered MCP tool names are already provider-safe. Slash-separated
schema/relation ids remain separate from MCP wire ids.

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
| substrate MCP tool | `core_` |
| flavor MCP tool | `<flavor>_` |

`FlavorRegistry::freeze()` rejects duplicate MCP tool names. Schema and
relation validation remains the registry's build-time responsibility
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
- Tool outputs are advertised by registered-schema-id reference
  (`McpToolDescriptor.produces_schema_ids`) and resolved against the
  `FlavorRegistry`.

### Action-Dispatch Tools

Tools whose argument type is an internally-tagged (`action`) enum —
`core_goal`, `core_wake`, `core_personality`, `core_fact` — are normalized
into a client-safe shape after `schemars` generation, because MCP clients
reject an `inputSchema` whose root is not `type: object` or that carries a
root `oneOf`/`anyOf`/`allOf`:

- The per-variant `oneOf` is flattened into one object: a unioned top-level
  `properties` map, an `action` string-enum discriminator, and
  `additionalProperties: false`.
- Per-action field metadata is published under the `x-proxima-actions`
  schema extension — `allowed_fields`, `required_fields`, and
  `field_descriptions` keyed by action — and mirrored in the
  `proxima://tools` catalog. Fields shared across actions carry a neutral
  root description that points back to this metadata.
- **Argument validation is strict and pre-decode.** Before an action's
  arguments are deserialized, any field outside that action's
  `allowed_fields`, or a missing `required_field`, is rejected with
  JSON-RPC `-32602`. Unknown fields are an error, not silently dropped.

## Wake-Entry Detect Config

Wake entry fields:

```rust
pub struct WakeEntryDraft {
    pub wake_entry_id: Uuid,
    pub personality_instance_id: PersonalityInstanceId,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub authored_by: WakeEntryAuthoredBy,
    pub probability_promille: u16,
    pub goal_scope: WakeEntryGoalScope,
    pub instructions: String,
}
```

Write-time validation:

| Field | Contract |
|---|---|
| `(trigger_kind, trigger_id)` | unique per active personality instance |
| `trigger_id`, `label` | non-empty |
| `probability_promille` | `0..=1000` |

Wake entries carry trigger config only. Model/tool/run policy stays external.

## Invocation Flow

Live MCP dispatch:

```
MCP request
  provider-safe name
    -> canonical id
    -> McpToolDescriptor.call(McpToolCtx, args)
```

Proxima is a passive brain hub. External harnesses own model choice,
tool planning, execution policy, and cursors.

MCP dispatch contract:

| Step | Contract |
|---|---|
| Auth | host `Authenticator` or master token |
| Owner | from auth context; Owner = principal (doc 01) |
| Tool scope | token capabilities intersected with deployment profile |
| Args | action-dispatch tools validate fields strictly (see Tool Schema Contract), then JSON decoded into `McpTool::Args` |
| Output | serialized `McpTool::Output` |
| Ids | prefixed ids (`F:/A:/P:/G:/E:` form, `OutputMode::PrefixedIds`) |

## Persistence

Current storage:

| Data | Storage |
|---|---|
| tool effects | `memories`, sidecar tables, `edges`, `goals`, change events |

Not present in v1:

| Table / API | Status |
|---|---|
| `tools` | absent |
| per-tool invocation table | absent |
| per-wake invocation table | absent |
| per-wake tool allowlist storage | absent |
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
