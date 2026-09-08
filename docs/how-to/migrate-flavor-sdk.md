# Migrate the Flavor SDK

Rust source changes. Existing database migrations, payload key bytes, and
served MCP/REST tool schemas remain unchanged.

| Previous Rust API | Replacement |
|---|---|
| `TypedFactIngest`, `ingest_typed_fact`, `ingest_typed_fact_with`, UoW `ingest_typed` | `FactWrite::new(owner, source_id, &payload)` and `ingest_fact` |
| typed Fact `.derived_from(...)` | `.refs(memory_ids)`; Facts have no derivation origins |
| raw `FactWriteCommand.derived_from` / `with_derived_from` | `additional_references` / `with_additional_references` |
| `AuthorDerivedRequestInput`, `author_derived_authorized`, UoW `author_derived` / `author_derived_all` | `DerivedMemory`, `derive_memory` / `derive_memories` |
| caller-selected `MemoryOperatorKind` | inferred from authorized origin rows and typed output payload |
| raw output schema/kind/version and generic `model_id` | typed payload metadata; generic `model_id` was unused |
| derived request's `memory_id` / `supersedes` | `MemoryTarget::Series(SeriesHandle)` / `MemoryTarget::Revision(prior_t)` |
| UoW `create_goal(dynamic_request, write_act_t)` | `create_goal(GoalCreateRequest<P>)`; episode write-act attachment stays internal |
| flavor `GoalCreatePayloadWriteRequest` | typed `GoalCreateRequest<P>`; dynamic protocol DTO remains Host API |
| `QueryRequest::for_owner(owner)` | `QueryRequest::readable()`; full authorized readable set |
| Query `owner` / `read_owners`; edge-read `owner` | removed inert request fields; Engine supplies authorization separately |
| authorized candidate/payload read helpers' `owner` argument | removed; candidate IDs are filtered by the caller's readable set |
| flavor `McpTool` / `McpToolCtx` / `McpToolError` | `Tool` / `ToolCtx` / `ToolError` |
| flavor `operator_label(&ctx, value)` | `ctx.operator_label(value)` |
| flavor `McpToolPresentation` | host wiring supplies presentation; flavor tools import `McpPresentationExt` |

## Identity and transactions

- Keep returned `memory_id` and `goal_id` for references and reads. A `SeriesHandle` is a version stream, not a row.
- Independent conclusions use distinct series handles. A→A derivation retains both conclusions. Revision advances the selected series and retains its history; it still requires origins.
- `Series(handle)` replays matching head metadata/pins. Changed origins append a version; unchanged origins with changed refs conflict. `Revision(prior_t)` appends on every successful call.
- Natural-key Fact series selection runs inside the storage transaction, including concurrent writes and earlier uncommitted observations. `.handle(...)` overrides automatic selection. Receipt replay still returns the original row.
- `FactWrite` selects an authorized destination without narrowing the caller's readable target set.
- Standalone writes commit before return. A `UnitOfWork` commits explicitly; drop rolls back all its writes. Pending Goal assignments must be Perspectives in the Goal's owner space; evidence may reference readable Facts or Abstractions.
- Split host Fact authorization followed by automatic natural-key admission must supply the typed payload during authorization. Storage verifies the same natural-key values at persistence.

## Reads and tools

`QueryRequest::readable()` has no owner selector. Legacy incoming JSON owner
fields are ignored; they cannot affect authorization. Known-ID reads use
`get_memory` / `get_memories`. Search retains its explicit corpus-owner selector.
Candidate helpers reject a zero limit, including an empty candidate list.

Implement one `Tool` trait; the existing adapters serve it over MCP and REST.
Keep `mcp_tools = [...]` build-time registration and existing wire names.
`McpPresentationExt` supplies MCP reference parsing/formatting on `ToolCtx`.
Host transport adapters remain available under `proxima::host`.

Complete examples: [typed operations](../reference/public-api.md#typed-consumer-operations),
[derived memory](../09-developing-flavors.md#deriving-abstractions),
[tool authoring](../tutorials/add-first-mcp-tool.md).
