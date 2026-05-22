# 12 — Tool Manifest

## Claim

Tool = build-time registered call surface + wake-entry palette entry.

| Rule | Contract |
|---|---|
| Registration | core/flavor crates only; frozen in `FlavorRegistry` at startup |
| Selection | wake entries choose allowed ids with `substrate_tool_palette` / `workspace_tool_palette` |
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
| Workspace tools | core catalog | `WORKSPACE_TOOL_CATALOG` | `workspace_tool_palette` | workspace runner + harness workspace dispatcher |

Stored ids:

| Surface | Id form |
|---|---|
| Substrate personality tool | `core/<name>` |
| Core MCP config tool | `core/<name>` |
| Flavor MCP tool | `<flavor>/<name>` |
| Workspace catalog tool | `proxima-workspace/<name>` |

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
    pub fn workspace_runner(&self, flavor_id: &str) -> Option<Arc<dyn WorkspaceRunner>>;
}
```

Prefix rules live in 08:

| Tool owner | Prefix |
|---|---|
| substrate MCP tool | `core/` |
| flavor MCP tool | `<flavor>/` |
| workspace catalog id | `proxima-workspace/` |

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
  `$defs`-free** — fully self-contained. This matters because some MCP
  clients do not resolve `$ref`; an unresolved reference renders as a
  blank field on those clients (the regression behind commit `37f209b`,
  where `update_wake_entry`'s patch argument rendered as a bare `$ref`).
- Field descriptions originate only from the Rust type — a `///`
  doc-comment or `#[schemars(description = "...")]` at the field
  definition. No code injects descriptions by field name.
- A recursive tool argument type is a **registration error**. It cannot
  be inlined into a finite `$ref`-free schema, so `mcp_tool_schema`
  panics at startup, naming the type — consistent with the
  schema-prefix and `freeze` panic model.
- Tool *outputs* are advertised by registered-schema-id reference
  (`McpToolDescriptor.produces_schema_ids`), resolved against the
  `FlavorRegistry`. Outputs are not inline-generated: the registry's
  payload schema is their single source of truth.

## Wake-Entry Selection

Wake entry fields:

```rust
pub struct WakeEntryDraft {
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub workspace_binding: Option<WakeWorkspaceBinding>,
    pub execution_mode: WakeExecutionMode,
    pub trigger_id: String,
}
```

Write-time validation:

| Field | Accepted ids |
|---|---|
| `substrate_tool_palette` | `registry.mcp_tool_ids() ∪ substrate_pack().tool_id()` |
| `workspace_tool_palette` | `workspace_tool_ids()` from `WORKSPACE_TOOL_CATALOG` |

Workspace mode also requires:

| Check | Source |
|---|---|
| `execution_mode == Workspace` implies `trigger_kind == OnMemory` | core validation |
| `workspace_binding` is present | core validation |
| `GitWorktree` binding | core git worktree prepare/finalize |
| `RegisteredRunner { flavor_id }` binding | `FlavorRegistryFrozen::workspace_runner(flavor_id)` |
| registered-runner `trigger_id` is workspace-eligible | `FlavorRegistryFrozen::is_workspace_trigger()` |

No trigger-prefix fallback. A wake with `execution_mode == Workspace`
and no binding is invalid.

## Invocation Flow

```
WakeEntry
  substrate_tool_palette
  workspace_tool_palette
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
        +--> workspace dispatcher in prepared worktree
```

Substrate MCP dispatch:

| Step | Contract |
|---|---|
| Auth | wake-token or master-token context |
| Owner | from auth context; `org_id` is not the access predicate |
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

Workspace dispatch:

| Step | Contract |
|---|---|
| Runner | flavor-owned `WorkspaceRunner` prepares the worktree |
| Palette | stored ids validated against the fixed workspace catalog |
| Tooling | `workspace_text_editor` / `workspace_list_files` are cwd-jailed to the prepared root via `jail_path` |
| Shell sandbox | `workspace_shell` runs inside the per-wake observation container via `docker exec` when the sandbox is enabled; host `bash -lc` otherwise |
| Finalize | runner records the run through ordinary storage writes |

Per-wake observation sandbox:

Each workspace wake runs inside one disposable Docker container — an
observation instrument, not an adversarial jail. The container runs as the
host uid/gid, bind-mounts a fresh `git clone` at `/workspace` and a
persistent build cache at `/cache`, and idles on `sleep infinity` while
`workspace_shell` enters it via `docker exec`. The container, its per-wake
network, and the clone are discarded when the wake ends. Proxima Shell,
Engine, MCP listener, Postgres, model credentials, and embedding clients all
stay in the host process. Configuration and env keys:
[10 §Workspace Observation Sandbox](10-configuration.md#workspace-observation-sandbox).

Two-tier jail:

| Tool | Jailed by |
|---|---|
| `workspace_shell` | the container boundary — commands run inside it via `docker exec` |
| `workspace_text_editor`, `workspace_list_files` | `jail_path`, host-side, against the prepared clone root |

Because the container runs as the host uid, the host-side editor/list tools
and the container shell operate as the same uid on the same files — there is
no ownership split.

Egress:

The container sits on a per-wake `--internal` network; the only route to the
web is a logging proxy that records every CONNECT and HTTP request line. That
log is captured as wake evidence (see [07 §Workspace Run](07-storage.md)).

Local images:

```sh
scripts/build-workspace-shell-sandbox-image.sh
```

Builds both the sandbox container and the proxy image.

Default sandbox image contents:

| Tooling | Purpose |
|---|---|
| `bash`, `git` | command execution and repo inspection |
| Rust toolchain | `cargo check`, `cargo test`, Rust edits |
| Node + `pnpm` | frontend typecheck/build when a Goal explicitly needs it |
| build libs | native Rust crate builds |

Not included: Tauri GUI runtime, browser automation, Postgres server, MCP
host, Ollama, or provider API keys. No provider secrets reach the container —
only `HTTP(S)_PROXY`, `HOME`, and `CI`.

## Persistence

Current storage:

| Data | Storage |
|---|---|
| wake tool selection | `personality_wake_entries.substrate_tool_palette`, `workspace_tool_palette` |
| wake invocation status | `personality_wake_invocations` |
| wake tool-call log tails | `personality_wake_invocation_logs` |
| tool effects | `memories`, sidecar tables, `edges`, `goals`, change events |
| workspace run artifacts | flavor-owned Fact / CitedObject schemas |

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

Until these fields land on the current `McpToolDescriptor` /
workspace-tool surfaces, docs must not claim implemented storage or
runtime enforcement. Owner-policy enforcement belongs to 13.

## Non-Goals

- No runtime schema, relation, source, prompt, or tool registration.
- No dynamic tool install path in v1.
- No OpenAI-function manifest as substrate authority.
- No MCP capability model as substrate authority.
- No generic external HTTP/WASM body transport in v1.
- No tool-specific entity lifecycle.
- No direct A/P persistence bypassing 04.

## Cross-References

| Topic | Doc |
|---|---|
| actions as ordinary Facts | [05](05-actions.md) |
| build-time vocabulary | [08](08-core-and-flavors.md) |
| owner policy and compliance vocabulary | [13 §Owner policy](13-compliance.md#owner-policy), [13 §Compliance vocabulary](13-compliance.md#compliance-vocabulary) |
| protocol clients observe changes, not Tool entities | [14](14-protocol-surface.md) |

## Anchors

- `claim`
- `tool-classes`
- `rust-surface`
- `tool-schema-contract`
- `wake-entry-selection`
- `invocation-flow`
- `persistence`
- `compliance-metadata`
- `non-goals`
- `cross-references`
