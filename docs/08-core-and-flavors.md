# 08. Core And Flavors

Binding ADR:
`docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md`.

## Layers

```
core
  ids, Owner, Memory/Goal/Edge contracts
  storage traits
  frozen registry
  dispatcher
  substrate tools

flavor
  payload schemas
  relation descriptors
  event sources
  MCP tools
  personalities
  wake filter kinds

composite binary
  calls flavor register() functions
  freezes registry
  wires storage + model clients
```

No runtime registration path.

## Macro Surface

```rust
proxima_flavor! {
    name = "proxima-code",
    fact_schemas = [schema::CommitV1],
    abstraction_schemas = [schema::CommitSummaryV1],
    perspective_schemas = [
        schema::CodeDevelopmentPerspectiveV1,
        schema::CodeEngineerSelfV1,
    ],
    edge_schemas = [schema::EdgeCallsV1],
    relations = [code_calls_relation()],
    personalities = [
        personality::CommitSummaryPersonality,
        personality::CodeEngineerPersonality,
    ],
    wake_filter_kinds = [],
    mcp_tools = [mcp::CodeSearchChunksTool],
}
```

Unknown keys are compile errors.

## PersonalityFlavor

```rust
trait PersonalityFlavor {
    fn personality_type_id(&self) -> &'static str;
    fn self_schema(&self) -> SchemaId;
    fn default_self_payload(&self, owner: &Owner, overrides: Option<&Json>)
        -> PersonalitySelfDraft;
    fn system_prompt(&self) -> &'static str;
    fn tools(&self) -> Vec<Arc<dyn PersonalityTool>>;
    fn writeable_schemas(&self) -> &'static [&'static str];
    fn writeable_relations(&self) -> &'static [&'static str];
    fn default_wake_filters(&self) -> Vec<WakeFilter>;
    fn tier(&self) -> ModelTier;
    fn max_wake_chain_depth(&self) -> u16;
}
```

`personality_type_id` is type-level. `personality_instance_id` is runtime
storage identity.

## Substrate Tool Pack

Default tools available to every personality:

| Read | Write |
|---|---|
| `list_self_perspectives` | `emit_perspective` |
| `fetch_memory` | `emit_abstraction` |
| `walk_lineage` | `emit_goal` |
| `search_by_embedding` | `create_edge` |
| `list_active_goals` |  |

Flavor tools extend the palette. Write tools enforce declared schemas and
relations.

## Freeze Guards

`FlavorRegistry::freeze()` rejects:

1. `self_schema` in `writeable_schemas`.
2. `core/derived-from` or `core/supersedes` in `writeable_relations`.
3. Unregistered schema ids in `writeable_schemas`.
4. Unregistered relation ids in `writeable_relations`.
5. Duplicate MCP tool names.
6. Typed relations whose payload schema is not registered.

Freeze-time failure is startup failure.

## Inclusion

Flavor crate = inclusion unit. No feature flags.

```
proxima-mcp       = substrate + goal
proxima-code      = substrate + code
proxima-shell     = substrate + code + goal + mcp
```

Composite binaries are build artifacts, not plugin hosts.
