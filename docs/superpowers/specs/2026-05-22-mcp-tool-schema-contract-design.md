# MCP Tool Schema Generation Contract

- Date: 2026-05-22
- Status: Approved (design)
- Scope: Spec 1 of 2. Spec 2 (unify `McpTool` / `PersonalityTool` into
  one `Tool` trait) is sequenced after this and consumes this contract.

## Problem

The Rust → JSON-Schema → MCP-wire seam is the least-settled part of the
tool surface.

- `add_mcp_tool<T>` (`crates/core/src/flavor.rs`) generates an MCP tool
  argument schema with `schemars::schema_for!(T::Args)`, then post-
  processes it with two bespoke `serde_json::Value` tree-walkers:
  - `inline_local_schema_refs` — flattens `#/$defs/*` `$ref`s and
    deletes `$defs`. Recursion is "handled" by a `depth > 32` guard
    that truncates, then `$defs` is removed — leaving a dangling `$ref`
    (a broken schema) for any recursive type.
  - `describe_generated_schema_fields` — injects `description` text for
    schema properties *named* `schema_id` or `body`. The survey found
    no Rust type that owns those fields: the injection is anchored to
    field-name strings, not to a type.
- The personality-tool path diverges: the 7 `PersonalityTool` impls
  build `args_schema()` from raw `schema_for!` with **no** inlining —
  they ship `$ref`-ful schemas to the wake LLM while `McpTool`s ship
  flattened ones.
- The flattening exists for a real reason (commit `37f209b`: external
  MCP clients that do not resolve `$ref` rendered `update_wake_entry`'s
  patch as an unresolved reference) but that requirement is documented
  nowhere, and the walkers have zero direct tests.

## Principle: the Rust type is the single source of truth

A tool's argument type *is* its schema. Shape, field descriptions,
required/optional, enum variants — all derive from the Rust type
definition via `schemars`. The contract forbids any out-of-band schema
mutation: no field-name-keyed injection, no post-hoc patching. If a
field needs a description, it carries a doc-comment or
`#[schemars(description = "...")]` at its definition.

## The contract

1. Every MCP tool argument schema is produced by one function,
   `mcp_tool_schema<T: JsonSchema>() -> serde_json::Value`.
2. The emitted schema is JSON Schema draft 2020-12 and **`$ref`-free /
   `$defs`-free** — fully self-contained, so MCP clients that do not
   resolve references render every field.
3. Recursive tool argument types are a **registration error**. A
   recursive type cannot be inlined into a finite `$ref`-free schema;
   the generator panics at registration (startup), naming the type.
4. Field descriptions originate only from the Rust type (doc-comments /
   `#[schemars]`). No code injects descriptions.
5. Tool *outputs* are advertised by registered-schema-id reference
   (`McpToolDescriptor.produces_schema_ids`), resolved against the
   `FlavorRegistry`. Outputs are **not** inline-generated — the
   registry's payload schema is their single source of truth, and a
   second inline copy could drift from it.

## Architecture

### New: `crates/core/src/mcp/schema.rs`

```rust
pub(crate) fn mcp_tool_schema<T: JsonSchema>() -> serde_json::Value {
    // SchemaSettings::draft2020_12() with inline_subschemas = true,
    // into a SchemaGenerator, root schema for T, serialized to Value.
    // (exact schemars 1.2.1 method names confirmed at implementation)
    let value = /* generated schema */;
    assert!(
        !schema_contains_ref(&value),
        "MCP tool type `{}` is recursive: schemars emitted a $ref that \
         cannot be inlined. MCP tool argument types must be \
         non-recursive.",
        std::any::type_name::<T>(),
    );
    value
}

fn schema_contains_ref(value: &serde_json::Value) -> bool { /* ~10-line scan */ }
```

`schemars` 1.2.1 `SchemaSettings::inline_subschemas` inlines every
non-recursive subschema and emits a `$ref` *only* for recursive types
(verified against the crate source). So a leftover `$ref` is an exact
signal of recursion — the recursion check is a free side-effect of the
chosen generator setting, not separate machinery. `schema_contains_ref`
only *detects*; it never transforms.

### Changed: `crates/core/src/flavor.rs`

- `add_mcp_tool<T>` schema block collapses to
  `let args_schema = mcp_tool_schema::<T::Args>();`.
- **Deleted:** `inline_local_schema_refs`, `inline_local_schema_refs_inner`,
  `local_def_ref_key`, `describe_generated_schema_fields` (~120 lines).

### Changed: the 7 `PersonalityTool` impls

Each `args_schema()` body becomes `mcp_tool_schema::<MyArgs>()`. This
removes the divergent raw-`schema_for!` path immediately — both tool
paths use the one function before the Spec 2 trait collapse. (These
call sites are absorbed when `PersonalityTool` is merged in Spec 2.)

### Description audit

Tools that currently rely on `describe_generated_schema_fields`
injection (the survey points at `update_wake_entry`) have doc-comments
added to the affected argument-type fields, so no field loses its
description when the injector is deleted.

## Data flow

Rust `Args` type → `schemars` (inline_subschemas) → `$ref`-free
`Value` → `schema_contains_ref` assertion → `McpToolDescriptor.args_schema`
→ `mcp-server` forwards verbatim → MCP wire.

## Error handling

- Recursive tool type → `panic!` at registration / startup, naming the
  type. Consistent with the existing prefix-check / `freeze` panic
  model (startup failure, not runtime).
- `serde_json::to_value` of a `schemars` schema → `.expect(...)`
  (structurally near-impossible; matches current code).
- No runtime error path: schema generation is startup-only.

## Testing

`crates/core/src/mcp/schema.rs` (or a test file):

- A nested-struct `Args` (which default `schemars` would `$ref`) comes
  out fully inlined — no `$ref`, no `$defs`.
- A deliberately recursive type makes `mcp_tool_schema` panic
  (`#[should_panic]`).
- A `#[schemars(description = "...")]` / doc-comment on a field
  survives into the emitted schema.
- Generalize the existing `flavor_registry.rs`
  `update_wake_entry_patch_schema_is_object` test against the new path.

## Documentation

`docs/12-tool-manifest.md` gains a "Tool schema contract" section:
argument schemas are `$ref`-free draft-2020-12 produced by
`mcp_tool_schema`; descriptions are authored on the Rust types; outputs
are advertised by registered-schema-id; recursive tool types are a
registration error; and *why* schemas are `$ref`-free — external MCP
clients that do not resolve `$ref` (commit `37f209b`).

## Out of scope (Spec 2)

- Merging `McpTool` and `PersonalityTool` into one `Tool` trait.
- Merging `McpToolCtx` and `PersonalityToolContext`.
- Converging the two `mcp-server` dispatch paths.
- Inline output-schema generation / MCP `outputSchema` emission.

## File-level change summary

| File | Change |
|---|---|
| `crates/core/src/mcp/schema.rs` | new — `mcp_tool_schema`, `schema_contains_ref`, tests |
| `crates/core/src/mcp/mod.rs` | declare `mod schema;` |
| `crates/core/src/flavor.rs` | `add_mcp_tool` one-liner; delete 4 functions |
| `crates/core/src/personality/tools/*.rs` | 7 `args_schema()` impls → `mcp_tool_schema` |
| tools relying on injected descriptions | add doc-comments (audit; `update_wake_entry`) |
| `crates/core/tests/flavor_registry.rs` | generalize the patch-schema test |
| `docs/12-tool-manifest.md` | new "Tool schema contract" section |
