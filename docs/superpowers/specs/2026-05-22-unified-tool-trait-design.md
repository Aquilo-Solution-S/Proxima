# Unified `Tool` Trait

- Date: 2026-05-22
- Status: Approved (design)
- Scope: Spec 2 of 2. Depends on Spec 1
  ([MCP tool schema generation contract](2026-05-22-mcp-tool-schema-contract-design.md)) —
  consumes its `mcp_tool_schema` generator.

## Problem

The engine has two tool execution models that are the same thing on the
wire (an MCP tool offered to an LLM) but two separate Rust abstractions:

- `McpTool` (`crates/core/src/mcp/mod.rs`) — type-level: assoc. consts +
  `type Args`/`type Output`, monomorphized, erased into
  `McpToolDescriptor`. 66 impls. Ctx: `McpToolCtx`. Error:
  `McpToolError`.
- `PersonalityTool` (`crates/core/src/personality/tool.rs`) —
  `dyn`-object: `&self`, `Arc<dyn>`, hand-built `args_schema() -> Value`.
  7 impls. Ctx: `PersonalityToolContext`. Error: `ProtocolError`,
  output `PersonalityToolResult { content: Value, is_error }`.

Consequences (survey-confirmed):

- Two `mcp-server` dispatch paths that already share the
  `McpToolHost::call_tool` entry and both serialize output to JSON —
  they diverge only in context construction and error handling.
- `PersonalityTool` returns the engine-wide `ProtocolError`; dispatch
  *lossily* wraps it into `McpToolError::Other(string)`.
- The two contexts share a core (`engine`, `owner`) and split on a
  genuine wake extension. `PersonalityToolContext.type_id` is set but
  never read. `McpToolCtx` carries `pool` and `registry` that are
  derivable from `engine`. `handles` is wake-scoped — `McpToolCtx`
  holds it as `Option` and only wake-dispatched tools use it.

## Design

### `Tool` trait

One trait, `McpTool`'s type-level shape plus a declared wake
requirement:

```rust
pub trait Tool: Send + Sync + 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    const PRODUCES_SCHEMA_IDS: &'static [&'static str] = &[];
    /// When true, the dispatcher rejects a call without a WakeContext
    /// and palette assembly omits the tool from non-wake surfaces.
    const REQUIRES_WAKE: bool = false;
    type Args: DeserializeOwned + JsonSchema + Send + 'static;
    type Output: Serialize + Send + 'static;
    fn call(ctx: ToolCtx, args: Self::Args)
        -> BoxFuture<'static, Result<Self::Output, ToolError>>;
}
```

The 7 personality tools' `tool_id()` / `description()` methods become
consts; `args_schema()` is deleted — derived from `type Args` via Spec
1's `mcp_tool_schema`. `PersonalityToolResult` is deleted; its
`is_error` path becomes `Err(ToolError)`, its `content` becomes a typed
`Output` (a personality tool may set `type Output = serde_json::Value`).

### Context

One `ToolCtx`; wake state isolated in an `Option<WakeContext>`:

```rust
pub struct ToolCtx {
    pub engine: Arc<Engine>,   // pool() / registry() / storage() accessors
    pub owner: Owner,
    pub author: ToolAuthor,    // external caller OR personality instance
    pub mode: OutputMode,
    pub wake: Option<WakeContext>,
}

pub struct WakeContext {
    pub handles: Arc<HandleTable>,
    pub root_perspective: MemoryId,
    pub triggering_event: MemoryId,
    pub triggering_depth: WakeChainDepth,
    pub writeable_schemas: Vec<String>,
    pub writeable_relations: Vec<String>,
    pub read_log: ReadLog,     // the wake read-log provenance mechanism, moved intact
}
```

- `engine` is mandatory (`Arc<Engine>`, not `Option`); `pool` and
  `registry` are no longer fields — they are accessors on `engine`.
- `handles` moves into `WakeContext` — handles are per-wake by
  construction.
- `PersonalityToolContext.type_id` is dropped (dead).
- `ToolAuthor` unifies author identity across an external/master-token
  caller and a personality instance. Exact encoding (a new enum vs.
  extending `McpAuthorContext`) is an implementation decision.
- A wake-only tool opens with `let w = ctx.wake()?;`. The read-log
  methods (`record_read`, `snapshot_provenance`) move onto
  `WakeContext` / `ReadLog`.

### Descriptor & registration

`McpToolDescriptor` → `ToolDescriptor`, gaining `requires_wake: bool`;
`args_schema` is produced by Spec 1's `mcp_tool_schema`. The erased
call type `McpCallFn` → `ToolCallFn`.

All tools — including the 7 former personality tools — register through
the one `FlavorRegistry` path. `substrate_pack()` and the separate
`Arc<dyn PersonalityTool>` palette are deleted. The wake tool palette
becomes a filtered *view* of the single registry (the filter is
`requires_wake` + existing palette authorization).

### Dispatch

The two `mcp-server` paths merge into one `call_tool`:

1. Look up the `ToolDescriptor`.
2. Build `ToolCtx` — `wake: Some(WakeContext { .. })` when invoked
   inside a wake, `None` otherwise.
3. If `descriptor.requires_wake && ctx.wake.is_none()` → reject with a
   uniform error.
4. `(descriptor.call)(ctx, args).await`; serialize `Output` to JSON
   (already common to both paths today).

### Errors

`McpToolError` → `ToolError` — the single tool-layer error. Personality
tools stop returning `ProtocolError`. A `From<ProtocolError> for
ToolError` conversion replaces the current lossy
`McpToolError::Other(err.to_string())` wrapping at the dispatch
boundary. `ProtocolError` remains the verb-layer error; tools simply no
longer surface it directly.

## Deleted

`McpTool`, `PersonalityTool`, `PersonalityToolContext`,
`PersonalityToolResult`, `McpToolCtx`, `substrate_pack`, and the second
`mcp-server` dispatch path.

## Migration

- 66 `McpTool` impls → `Tool`: near-mechanical — `impl Tool`,
  `McpToolCtx` → `ToolCtx`, `McpToolError` → `ToolError`,
  `ctx.handles` → `ctx.wake()?.handles`, `ctx.pool` → `ctx.pool()`,
  `ctx.registry` → `ctx.registry()`. A tool that cannot function
  without handles sets `REQUIRES_WAKE = true`.
- 7 `PersonalityTool` impls → `Tool`: heavier per tool — typed
  `Args`/`Output`, consts, `ctx.wake()?`, `REQUIRES_WAKE = true`,
  `PersonalityToolResult` → `Result<Output, ToolError>`.
- The implementation plan phases this: introduce `Tool` / `ToolCtx` /
  `ToolDescriptor` alongside the existing types, migrate impls in
  batches behind a compiling tree, converge dispatch, then delete the
  old types.

## Error handling

- A `REQUIRES_WAKE` tool invoked without a `WakeContext` → uniform
  dispatch-level rejection (a `ToolError` variant), not a per-tool
  ad-hoc check.
- A wake-only tool that still reads `ctx.wake()` defensively returns a
  clear `ToolError` if it is `None`.
- Schema generation errors (recursive types) are Spec 1's concern —
  startup panic.

## Testing

- A `REQUIRES_WAKE` tool invoked with `wake: None` is rejected.
- A non-wake tool runs with `wake: None`.
- A migrated personality tool round-trips typed `Args`/`Output`
  through the erased `ToolCallFn`.
- Existing per-tool tests (66 + 7) pass after migration.
- One dispatch test covers the merged `call_tool` for both a wake and
  a non-wake invocation.

## Decisions recorded

- **Renames** `McpTool`→`Tool`, `McpToolError`→`ToolError`,
  `McpToolCtx`→`ToolCtx`, `McpToolDescriptor`→`ToolDescriptor`,
  `McpCallFn`→`ToolCallFn`. Accepted: the `Mcp` prefix is misleading
  once the trait also powers the wake loop. ~73 impl sites plus
  dispatch — mechanical churn.
- **`engine` mandatory** on `ToolCtx`; `pool`/`registry` become
  accessors. Accepted: removes redundant fields; every `ToolCtx`
  construction site (including tests) provides an `Arc<Engine>`.
- **`REQUIRES_WAKE`** is a runtime-checked const, not a compile-time
  guarantee — a base/`WakeTool` two-trait split was rejected because
  it contradicts the one-trait goal.

## Out of scope

- Inline output-schema generation / MCP `outputSchema` emission
  (outputs stay advertised by registered-schema-id — Spec 1 §contract).
- Reworking `ProtocolError` / the verb-layer error model.
- Changes to the wake decision loop beyond what dispatch convergence
  requires.

## File-level change summary

| Area | Change |
|---|---|
| `crates/core/src/mcp/mod.rs` | `McpTool`→`Tool`, `McpToolCtx`→`ToolCtx` + `WakeContext`, `McpToolDescriptor`→`ToolDescriptor` (+`requires_wake`), `McpToolError`→`ToolError`, `ToolAuthor` |
| `crates/core/src/personality/tool.rs`, `context.rs` | deleted — `PersonalityTool`, `PersonalityToolContext`, `PersonalityToolResult` |
| `crates/core/src/personality/tools/*.rs` | 7 tools re-implemented as `Tool` |
| `crates/core/src/flavor.rs` | `add_mcp_tool`→`add_tool` etc.; descriptor type |
| `crates/core/src/personality/tools/mod.rs` | `substrate_pack` deleted |
| 66 `McpTool` impls (core + flavors) | mechanical migration to `Tool` |
| `crates/mcp-server/src/handler.rs`, `server.rs` | two dispatch paths → one; palette from the single registry |
| `crates/core/src/error.rs` | `From<ProtocolError> for ToolError` |
| `docs/12-tool-manifest.md` | updated to one trait / one descriptor |
