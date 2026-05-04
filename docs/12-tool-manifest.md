# 12 — Tool Manifest

Tools split into two tiers:

| Tier | Brings | Trust | Registration |
|---|---|---|---|
| **T1** | Manifest + body. Uses *existing* registered Fact schemas and relations. | Sandboxed (WASM) and/or signed. | Runtime, via API. |
| **T2** | Schemas + sources + tools + prompts. | Audited release. | Build-time (08). |

T1 is the marketplace. T2 is unchanged from 08. **T1 cannot invent schemas or relations**; capability declarations are validated against the linker-frozen set at install time.

A/P authorship is operator-only regardless of tier. Tools may only emit Facts.

## Manifest format

`tools/<name>.toml`:

```toml
[tool]
id          = "code/forgejo-comment-issue"
version     = 1
description = "Post a comment on a Forgejo issue."

# OpenAI-compatible function definition. Fed verbatim to the decider LLM.
[tool.function]
name        = "forgejo_comment_issue"
description = "Post a comment on a Forgejo issue."
parameters  = { type = "object", properties = {
    repo  = { type = "string" },
    issue = { type = "integer" },
    body  = { type = "string" }
}, required = ["repo", "issue", "body"] }

# Engine-enforced. Not LLM-visible.
[tool.capabilities]
emit_facts     = ["forgejo-comment-posted"]   # registered FactPayload SCHEMA_IDs
emit_relations = ["code/comment-on-issue"]    # registered RelationDescriptor ids
owner_scope    = "invocation"                 # "invocation" | "user" | "group:<id>"

[tool.body]
kind = "wasm"                                 # "wasm" | "mcp" | "http"
module = "sha256:abcd...ef01"

# Alternatives:
# [tool.body] kind = "mcp"
# server    = "stdio:/usr/local/bin/forgejo-mcp"   # or "sse:https://..."
# tool_name = "comment_issue"
#
# [tool.body] kind = "http"
# endpoint = "https://forgejo-bridge.aquilo.internal/comment"
# auth     = "bearer:env:FORGEJO_BRIDGE_TOKEN"

[tool.signature]
signer    = "aquilo-solutions"
algorithm = "ed25519"
sig       = "..."
```

`[tool.function]` is the OpenAI / Anthropic function-call shape verbatim; passed to the decider unchanged. `[tool.capabilities]` and `[tool.body]` are Proxima-specific and never reach the LLM.

## Rust types

```rust
#[derive(Deserialize)]
pub struct ToolManifest { pub tool: Tool }

#[derive(Deserialize)]
pub struct Tool {
    pub id:           ToolId,
    pub version:      u32,
    pub description:  String,
    pub function:     OpenAIFunction,
    pub capabilities: Capabilities,
    pub body:         Body,
    pub signature:    Signature,
}

#[derive(Deserialize, Serialize)]
pub struct OpenAIFunction {
    pub name:        String,
    pub description: String,
    pub parameters:  serde_json::Value,        // JSON Schema; opaque to engine
}

#[derive(Deserialize)]
pub struct Capabilities {
    pub emit_facts:     Vec<SchemaId>,
    pub emit_relations: Vec<RelationId>,
    pub owner_scope:    OwnerScope,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Body {
    Wasm { module: ContentHash },
    Mcp  { server: McpEndpoint, tool_name: String },
    Http { endpoint: Url, auth: AuthRef },
}
```

## Registry surface

```rust
trait ToolRegistry {
    fn install(&mut self, m: ToolManifest, allowlist: &[SignerId]) -> Result<ToolId, InstallError>;
    fn revoke(&mut self, id: ToolId)        -> Result<(), RevokeError>;
    fn lookup(&self, id: ToolId)            -> Option<&Tool>;
    fn available_for(&self, ctx: &CallContext) -> Vec<&OpenAIFunction>;
}

enum InstallError {
    UnknownFactSchema(SchemaId),
    UnknownRelation(RelationId),
    BadSignature,
    UntrustedSigner,
    DuplicateToolId,
    BodyUnreachable,
}
```

`install` validates `capabilities.emit_facts` and `capabilities.emit_relations` against the registered (build-time) set. Unknown id ⇒ reject. T1 cannot widen what T2 has frozen.

`available_for` returns the exact OpenAI function set the decider sees; nothing more.

## Invocation flow

For how tools integrate with the SYSTEM EventSource, see [05-actions.md](docs/05-actions.md).

1. Decider receives `available_for(ctx)`; selects a tool with arguments.
2. Engine validates args against `manifest.function.parameters` (jsonschema crate).
3. Engine dispatches `body`:
   - `Wasm`: wasmtime; host imports = read-only context.
   - `Mcp`:  JSON-RPC `tools/call` to server; parse `CallToolResult`.
   - `Http`: POST args; parse JSON.
4. Body returns `ToolResult { fact: FactPayloadJson, relations: Vec<EdgeRef> }`.
5. Engine validates `fact.schema_id ∈ capabilities.emit_facts` and every `relation.id ∈ capabilities.emit_relations`.
6. Engine emits `SYSTEM` event → Fact + structural edges in one transaction ([05](docs/05-actions.md)).
7. Engine returns `memory_id` to the decider.

A failed validation aborts; nothing is persisted; the failure is recorded on `tool_invocations.error`.

## Storage

```sql
CREATE TABLE tools (
    id              text PRIMARY KEY,
    version         int  NOT NULL,
    manifest_json   jsonb NOT NULL,
    manifest_hash   text NOT NULL,
    signer          text NOT NULL,
    body_kind       text NOT NULL,
    installed_at    timestamptz NOT NULL DEFAULT now(),
    revoked_at      timestamptz,
    UNIQUE (id, version)
);

CREATE TABLE tool_invocations (
    id              uuid PRIMARY KEY,
    tool_id         text NOT NULL REFERENCES tools(id),
    tool_version    int  NOT NULL,
    owner_principal_kind text NOT NULL,
    owner_principal_id   text NOT NULL,
    owner_org_id         text NOT NULL,
    args_json       jsonb NOT NULL,
    started_at      timestamptz NOT NULL,
    finished_at     timestamptz,
    result_memory   uuid REFERENCES memory(id),
    error           text
);
```

Append-only in the audit sense: revoke sets `revoked_at`; never `DELETE`.

## State and long-running tools

- **No side storage.** Tools persist state only via Facts (a `tool-state` schema) or per-call ephemerally. Anything else rots the audit trail.
- **Streaming / async.** Long-running invocations land as two Facts joined by `request_id`: an `invoked` Fact at dispatch and a `result` Fact when the body returns. Falls out of 05's "effect re-enters via EventSource path"; no special wiring.

## Why this layering

- **OpenAI shape for the LLM-facing slice.** Universal across decider models; zero glue.
- **MCP as a body transport.** Mount Anthropic / GitHub / Slack tool servers without inventing a wire format. MCP's capability model is *not* used; Proxima's `[tool.capabilities]` block is authoritative.
- **Capability declaration on top.** Output Fact schema, allowed relations, owner scope — none of which OpenAI or MCP define. This is the engine-enforceable part.

## What this does not do

- Does not let T1 author Abstractions or Perspectives.
- Does not let T1 register new Fact schemas or relations.
- Does not replace `Registrant` machinery — there is none. T2 stays the typed-Rust path described in 08.
- Does not validate semantic correctness of the body's output beyond schema conformance and capability scope.

## Anchors

- `manifest-format`
- `openai-compatible-function-definition`
- `engine-enforced-not-llm-visible`
- `rust-types`
- `registry-surface`
- `invocation-flow`
- `storage`
- `state-and-long-running-tools`
- `why-this-layering`
- `what-this-does-not-do`
- `relationship-to-t1-marketplace-12`
- `non-goals-for-v1`
- `why-formalize-now`
- `cross-references`
erences`
