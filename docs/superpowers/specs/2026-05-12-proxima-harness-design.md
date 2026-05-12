# Proxima Harness — replacing Goose with an in-process LLM loop

**Status:** design
**Date:** 2026-05-12
**Owner:** Heinrich
**Scope:** `crates/core/src/wake/target_adapter/`, new `crates/harness/`, `crates/core/src/inference/`, `crates/mcp-server/`, `flavors/code/recipes/`, the `wake_invocation_log` storage path, and one new core schema pair (`proxima-core/wake-trace-v1` Fact + `proxima-core/wake-trace-citation-v1` CitationMapping).
**Related:**
- `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md` — the spinning-wheel loop, four-param wake context, WakeEntry/InferenceTarget split, and `LocalCliGooseAdapter` interface this spec replaces.
- `docs/superpowers/specs/2026-05-09-workspace-mode-design.md` — `WorkspaceRunner` prepare/finalize trait stays; the part driven by `--with-builtin developer` goes.
- `docs/01-event-source.md` and `crates/core/src/verbs/event_ingest.rs` — `CitedObject` / `CitationMapping` primitive we reuse for JSONL persistence.

## Problem

`LocalCliGooseAdapter` (`crates/core/src/wake/target_adapter/local_cli_goose.rs`) shells `goose run --recipe ... --output-format stream-json` per wake and **regex-scans stderr** for turn count (`parse_turn_count`), turn-limit detection (`output_indicates_turn_limit`), and model-error detection (`output_indicates_model_error`). The wake's outcome — `Succeeded | Truncated | Failed` — is inferred from freeform CLI output. Recipe assembly rewrites YAML at every wake (`crates/core/src/wake/fire/recipe.rs::write_effective_recipe`) by stripping the top-level `extensions:` block and concatenating a new one that points at the in-process MCP URL with the wake token as bearer auth. Provider and model selection live in `~/.config/goose/config.yaml` — outside the engine, invisible to the audit trail, mutable without an engine record.

The pain points are structural, not cosmetic:

1. **Inferred outcomes.** Every Goose CLI release risks breaking our regex. "Truncated" vs "Failed" is a string-match heuristic.
2. **Recipe rewriting.** String-concatenated YAML, executed on every wake fire.
3. **Two config languages.** WakeEntry palette is in Postgres; the same palette is also written into the rewritten recipe's `available_tools:`; the MCP server enforces the *same* palette a third time via `McpToolScope::Palette`. Three sources, drift opportunities.
4. **Provider invisibility.** The engine doesn't know which model actually ran.
5. **Workspace tools are Goose's `developer` builtin** — third-party surface, third-party schema, third-party failure modes.
6. **Subprocess cold-start per wake.** Every fire is a fresh Goose process; no shared state, no warm pool, argv-length-bounded parameter passing.

## Decision

Replace the subprocess with an in-process Rust loop — the **Proxima Harness** — that owns the model-call/tool-dispatch cycle end-to-end. The harness lives in a new `crates/harness/` crate, plugs into the existing `TargetAdapter` seam, and obeys six non-negotiable principles.

### Six principles

1. **Native model tool-calling, every provider.** Each adapter speaks the provider's tool-use protocol directly: Mistral's `/v1/chat/completions` with `tools: [...]`, OpenAI's `/v1/responses` (Codex tier) and `/v1/chat/completions`, Anthropic's `/v1/messages`. We **never** parse model prose to detect control flow. Termination is the provider's `finish_reason` (`stop` | `tool_calls` | `length` | provider-equivalent).
2. **Tools are typed bindings dispatched in-process.** Substrate and flavor tools call `McpToolDescriptor.call` directly with a wake-token-derived `McpToolCtx`. No TCP loopback at wake time, no JSON-RPC envelope, no MCP transport layer in the hot path. Workspace tools implement a Rust `WorkspaceTool` trait. Provider tool-call arguments validate against `args_schema` before dispatch — invalid args produce a structured tool error message; the model self-corrects on the next round.
3. **Workspace toolkit is three Rust tools, not Goose's `developer` builtin.** `workspace_shell`, `workspace_text_editor`, `workspace_list_files`, each cwd-jailed to the prepared worktree. Their JSON schemas are generated from `schemars` derives. The model sees them as native function-calling tools; the harness dispatches them as Rust functions.
4. **Credentials are env vars resolved at wake time.** `InferenceTargetConfig` grows variants `Mistral`, `OpenAIResponses`, `OpenAIChat`, `Anthropic`. Each carries `base_url`, `model_id`, and `api_key_env` (the name of the env var to read). The engine reads the env at fire time — not at startup — so users can rotate keys without restart. Missing env → invocation finalizes as `Failed("credentials_missing:MISTRAL_API_KEY")`, precise and structured. **No third-party CLI config file is consulted, ever.**
5. **Outcome derives from explicit signals, never regex.** The outcome classifier sees: HTTP status, provider-reported `finish_reason`, round counter, tool-dispatch results, exception class. The classification table (below) is exhaustive and deterministic.
6. **Every wake leaves three observability traces: a JSONL transcript, `wake_invocation_log` rows, and a `wake-trace-v1` Fact in the memory graph.** No layer is optional. The Fact is the substrate-native index; the log table is the SQL-queryable cross-cut; the JSONL is the forensic raw. The JSONL persists as a `CitedObject` content-addressed by BLAKE3 and pinned to the Fact via `CitationMapping`.

### Crate layout

```
crates/harness/
  Cargo.toml
  src/
    lib.rs                         # public surface: HarnessAdapter, HarnessRun, errors
    program.rs                     # HarnessProgram: 4-param wake context → typed Conversation seed + tool palette
    conversation.rs                # Conversation, Message, ToolCall, ToolResult — provider-neutral types
    outcome.rs                     # HarnessOutcome, FinishReason, ErrorClass — feeds back to fire.rs
    loop.rs                        # the wake-loop driver: model.tool_round() → dispatch_tools() → repeat
    tools/
      mod.rs                       # ToolBinding enum: Substrate | Flavor | Workspace
      substrate_dispatch.rs        # in-process call into McpToolDescriptor.call
      workspace/
        mod.rs                     # WorkspaceTool trait
        shell.rs                   # bounded bash; timeout, output cap, exit code structural
        text_editor.rs             # view | create | str_replace | insert; path-jailed
        list_files.rs              # cwd-rooted recursive listing
    providers/
      mod.rs                       # ProviderClient trait + ProviderError
      mistral.rs                   # /v1/chat/completions, tool-calling
      openai_chat.rs               # /v1/chat/completions, tool-calling
      openai_responses.rs          # /v1/responses (Codex tier)
      anthropic.rs                 # /v1/messages (behind `claude` feature flag for v1)
    trace/
      jsonl.rs                     # in-memory JSONL buffer with size cap + truncate-marker
      wake_trace_fact.rs           # emit_wake_trace_fact: builds Fact + Citation + edges
```

### Core traits

```rust
// crates/harness/src/lib.rs

#[async_trait]
pub trait HarnessAdapter: Send + Sync {
    async fn run(&self, program: HarnessProgram, ctx: HarnessContext)
        -> Result<HarnessOutcome, HarnessError>;
}

// One impl: HarnessLoop<P: ProviderClient>. The driver is generic over
// provider; per-provider differences are contained in ProviderClient.
```

```rust
// crates/harness/src/providers/mod.rs

#[async_trait]
pub trait ProviderClient: Send + Sync {
    async fn tool_round(
        &self,
        conversation: &Conversation,
        tools: &[ToolSpec],
        cancel: CancellationToken,
    ) -> Result<RoundResult, ProviderError>;
}

pub enum RoundResult {
    /// Model wants to call N tools, then continue.
    ToolCalls { calls: Vec<ToolCall>, raw_assistant: AssistantTurn },
    /// Model finished the turn with text and no tool calls.
    Final { text: String, raw_assistant: AssistantTurn },
    /// Provider returned a "length"-style finish_reason mid-stream.
    LengthCap { partial_text: Option<String>, raw_assistant: AssistantTurn },
}

pub enum ProviderError {
    Auth,                   // HTTP 401/403
    RateLimited { retry_after: Option<Duration> }, // HTTP 429
    ContextLength,          // model-reported context_length_exceeded
    InvalidRequest(String), // 400; the request shape is wrong
    ServerError(String),    // 5xx; transient
    Network(String),        // reqwest error
    Timeout,                // tokio::time::timeout fired
    Deserialize(String),    // we couldn't parse the response envelope; bug, not normal
}
```

```rust
// crates/harness/src/conversation.rs

pub struct Conversation {
    pub system_prompt: String,
    pub user_seed: String,          // rendered four-param block
    pub turns: Vec<Turn>,           // assistant, tool-result, assistant, tool-result, ...
}

pub enum Turn {
    Assistant(AssistantTurn),       // model output: text + tool_calls
    ToolResult(ToolResultTurn),     // dispatch result; one row per call_id
}

pub struct ToolCall {
    pub call_id: String,            // provider-issued; opaque to harness
    pub tool_name: String,          // canonical name (not provider-safe rewrite)
    pub arguments: serde_json::Value,
}

pub struct ToolResultTurn {
    pub call_id: String,
    pub status: ToolStatus,         // Ok | Error
    pub content: serde_json::Value, // tool-defined; structurally typed
}
```

### The loop

```rust
// crates/harness/src/loop.rs (sketch)

pub async fn run_loop(program: HarnessProgram, ctx: HarnessContext)
    -> Result<HarnessOutcome, HarnessError>
{
    let mut conv = program.seed_conversation();
    let mut trace = TraceBuffer::new(ctx.max_trace_bytes);
    trace.start(&program, &ctx);

    for round_idx in 0..ctx.max_rounds {
        let round_started = Instant::now();
        let round = ctx.provider.tool_round(&conv, &program.tool_specs, ctx.cancel.clone()).await;
        trace.record_round_outbound(round_idx, &conv);

        match round {
            Ok(RoundResult::Final { text, raw_assistant }) => {
                conv.turns.push(Turn::Assistant(raw_assistant));
                trace.record_assistant(round_idx, &text, FinishReason::Stop, round_started.elapsed());
                ctx.log_round(round_idx, /* finish_reason */ "stop", /* tool_id */ None, ...).await;
                return Ok(HarnessOutcome::succeeded(text, round_idx + 1, trace.finalize()));
            }
            Ok(RoundResult::LengthCap { partial_text, raw_assistant }) => {
                conv.turns.push(Turn::Assistant(raw_assistant));
                trace.record_assistant(round_idx, partial_text.as_deref().unwrap_or(""), FinishReason::Length, round_started.elapsed());
                ctx.log_round(round_idx, "length", None, ...).await;
                return Ok(HarnessOutcome::truncated_length(round_idx + 1, trace.finalize()));
            }
            Ok(RoundResult::ToolCalls { calls, raw_assistant }) => {
                conv.turns.push(Turn::Assistant(raw_assistant));
                let results = dispatch_tool_calls(&calls, &program.tool_specs, &ctx).await;
                trace.record_tool_calls(round_idx, &calls, &results);
                conv.turns.push(Turn::ToolResult(/* one Turn per call_id */));
                for (call, result) in calls.iter().zip(results.iter()) {
                    ctx.log_round(round_idx, "tool_calls", Some(&call.tool_name), result.status, ...).await;
                }
                // loop continues
            }
            Err(err) => {
                trace.record_error(round_idx, &err);
                let outcome_err = classify_provider_error(err, &ctx.retry_state);
                if outcome_err.retryable && ctx.retry_state.attempt() < ctx.retry_policy.max_retries {
                    ctx.retry_state.bump();
                    sleep(outcome_err.backoff).await;
                    continue;
                }
                return Ok(HarnessOutcome::failed(outcome_err.class, round_idx, trace.finalize()));
            }
        }
    }

    // Loop exit without Final/LengthCap: we ran out of rounds while the
    // model was still requesting tool calls. This is "Truncated by
    // max_rounds", structurally distinct from provider-reported "length".
    Ok(HarnessOutcome::truncated_max_rounds(ctx.max_rounds, trace.finalize()))
}
```

The driver has zero string-matching on model output. Every branch is one of: explicit provider signal, explicit HTTP/transport error class, explicit round counter, or explicit cancellation. The classification table is fixed at this layer.

### Outcome classification (exhaustive)

| Signal observed | `HarnessOutcome.kind` | `failure_reason` (when Failed) |
|---|---|---|
| `RoundResult::Final` | `Succeeded` | — |
| `RoundResult::LengthCap` | `Truncated` | — |
| Loop counter == `max_rounds` while model still requesting tools | `Truncated` | — |
| `ProviderError::Auth` | `Failed` | `auth` |
| `ProviderError::RateLimited` after retry budget exhausted | `Failed` | `rate_limited` |
| `ProviderError::ContextLength` | `Failed` | `context_length` |
| `ProviderError::InvalidRequest(msg)` | `Failed` | `provider_invalid_request:{msg-truncated}` |
| `ProviderError::ServerError` after retry budget exhausted | `Failed` | `provider_server_error` |
| `ProviderError::Network` after retry budget exhausted | `Failed` | `provider_network` |
| `ProviderError::Timeout` (per-round) | `Failed` | `provider_timeout` |
| `ProviderError::Deserialize` | `Failed` | `provider_deserialize` (this is a bug, surface loudly) |
| Wall-clock invocation timeout (dispatcher-level) | `Failed` | `invocation_timeout` |
| Cancellation (engine shutdown) | `Failed` | `cancelled` |
| Workspace tool returned a tool-error 5+ times in a row | `Failed` | `tool_error_streak:{tool_id}` (defense-in-depth circuit breaker) |

Every row above is decidable from a structural signal. No regex appears in the classifier.

### `InferenceTargetConfig` migration

```rust
// crates/core/src/inference/types.rs (after migration)

pub enum InferenceTargetConfig {
    Mistral(MistralConfig),
    OpenAIChat(OpenAIChatConfig),
    OpenAIResponses(OpenAIResponsesConfig),  // Codex tier
    Anthropic(AnthropicConfig),              // gated behind `claude` feature for v1
    /// Deprecated. Existing rows continue to deserialize, but the
    /// dispatcher rejects fires against `LocalCli` once the Goose
    /// adapter is removed (Phase 3). Used during the migration window
    /// only.
    #[deprecated(note = "use Mistral/OpenAI/Anthropic native adapters")]
    LocalCli(LocalCliConfig),
}

pub struct MistralConfig {
    pub base_url: String,            // default "https://api.mistral.ai"
    pub model_id: String,            // e.g. "mistral-medium-3.5"
    pub api_key_env: String,         // env var name to read at wake time
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

pub struct OpenAIResponsesConfig {
    pub base_url: String,            // default "https://api.openai.com"
    pub model_id: String,            // e.g. "gpt-5-codex"
    pub api_key_env: String,         // default "OPENAI_API_KEY"
    pub reasoning_effort: Option<ReasoningEffort>,   // low | medium | high
}

pub struct OpenAIChatConfig { /* parallel to MistralConfig */ }
pub struct AnthropicConfig { /* base_url, model_id, api_key_env, thinking, etc. */ }
```

`LocalCli` stays in the enum across Phase 1 and Phase 2 so existing rows keep parsing. The dispatcher's target-resolution path routes `Mistral/OpenAI/Anthropic` to the new harness and `LocalCli` to the old Goose adapter during the migration window. Phase 3 removes the variant and the adapter together; rows still referencing `LocalCli` are surfaced in the migration script and re-pointed.

The four-tier model registry on the Shell config (`apps/proxima-shell/src-tauri/src/config/types.rs::InferenceTargetRecord`) reflects the same variants; the TOML round-trip test grows three cases.

### Workspace tools

Three Rust impls, registered in the harness, exposed to the model with `schemars`-derived JSON schemas. Each is cwd-jailed: the only filesystem root the model can name is the prepared worktree (`HarnessContext.workspace_root`, set by the existing `WorkspaceRunner::prepare` flow).

#### `workspace_shell`

```rust
#[derive(Deserialize, JsonSchema)]
pub struct ShellArgs {
    /// Command to run via `bash -lc`.
    pub command: String,
    /// Hard timeout. Default 30_000, max 120_000.
    #[serde(default = "default_shell_timeout_ms")]
    pub timeout_ms: u32,
}

#[derive(Serialize, JsonSchema)]
pub struct ShellResult {
    pub exit_code: i32,
    pub stdout: String,        // capped at 32 KB; `stdout_truncated: bool` set if hit
    pub stdout_truncated: bool,
    pub stderr: String,        // capped at 32 KB
    pub stderr_truncated: bool,
    pub duration_ms: u64,
    pub timed_out: bool,
}
```

Execution: `tokio::process::Command::new("bash").arg("-lc").arg(args.command).current_dir(workspace_root)`. Env is cleared except for `PATH`, `HOME`, `USER`, `LANG`, `TERM`. No PROXIMA_WAKE_TOKEN or PROXIMA_MCP_URL leak into the shell (those are harness-internal). Output is line-buffered, capped per stream; a SIGTERM is sent on timeout, followed by SIGKILL after 1 s.

#### `workspace_text_editor`

```rust
#[derive(Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum TextEditorArgs {
    View { path: String, view_range: Option<[u32; 2]> },
    Create { path: String, file_text: String },
    StrReplace { path: String, old_str: String, new_str: String },
    Insert { path: String, insert_line: u32, new_str: String },
}
```

`path` is resolved against `workspace_root`. Path traversal (`..`, absolute paths, symlink escape) is rejected before any IO. `str_replace` errors if `old_str` is not unique in the file (force the model to disambiguate). Result types are structurally typed — line counts, char counts, the changed range — never freeform text.

#### `workspace_list_files`

```rust
#[derive(Deserialize, JsonSchema)]
pub struct ListFilesArgs {
    pub path: String,                // relative to workspace_root
    #[serde(default = "default_max_depth")]
    pub max_depth: u8,               // default 3, max 8
    #[serde(default)]
    pub include_hidden: bool,
}

#[derive(Serialize, JsonSchema)]
pub struct ListFilesResult {
    pub entries: Vec<FsEntry>,       // capped at 500 entries; `truncated: bool`
    pub truncated: bool,
}

pub struct FsEntry {
    pub path: String,                // relative
    pub kind: FsEntryKind,           // File | Directory | Symlink
    pub size_bytes: Option<u64>,
}
```

`.git/` is silently skipped unless `include_hidden = true`.

### Substrate / flavor tool dispatch

The model sees substrate and flavor MCP tools as ordinary function-calling tools, schema generated from the existing `McpToolDescriptor.args_schema`. When the model emits a tool call, the harness:

1. Looks up the canonical tool name (already-typed via `WakeEntry.substrate_tool_palette`).
2. Constructs an `McpToolCtx` from the wake-token context — the same context that today's HTTP path builds in `crates/mcp-server/src/handler.rs::call_tool`.
3. Calls `(descriptor.call)(ctx, args)` directly. No HTTP transport. No JSON-RPC. Result is already a typed `serde_json::Value`.
4. Wraps the result as a `Turn::ToolResult` and appends to the conversation.
5. Records a `wake_invocation_log` row (`phase: "tool_call"`, `tool_id`, `status`, `duration_ms`, `message_tail`) — identical to today's MCP-path logging, just emitted from the harness instead.

The MCP server (`crates/mcp-server`) **continues to exist** for external callers — Claude Code, hosted-account-auth flows, the Shell's master-token surface. Wakes simply no longer use it. The recipe rewriter that injects `extensions:` for Goose is deleted; nothing replaces it.

### Recipe lifecycle: kill the YAML

**Decision: A1.** No more recipe YAML files.

- `system_prompt` already lives on the Root Perspective payload (`proxima-core/root-personality-perspective-v1.system_prompt`). The harness uses it directly.
- `WakeEntry` grows one column: `instructions TEXT NOT NULL DEFAULT ''`. This is the per-trigger work instruction body that today's `instructions:` recipe field carries.
- The harness composes the user-seed message as:
  ```
  {WakeEntry.instructions}

  Root perspective:    {root_perspective JSON}
  Active goals:        {active_goals JSON}
  Trigger event:       {trigger_event JSON}
  Triggering memory:   {triggering_memory JSON}
  Workspace context:   {workspace_context JSON}   # workspace mode only
  ```
- `recipe_ref` column stays on `WakeEntry` during the migration for backwards compatibility but is **unread by the harness path**. Phase 3 drops the column.
- A one-shot migration script (`scripts/migrate-recipes-to-wake-entries.rs`) reads each bundled YAML's `instructions:` field and writes it to the matching WakeEntry rows. The bundled `flavors/code/recipes/*.yaml` files are deleted in the same PR that flips the default flag.

The `recipe_resolve.rs` / `recipe_validate.rs` modules are deleted alongside the YAML files.

### Observability: three layers, all mandatory

#### Layer 1 — JSONL transcript (as `CitedObject`)

Every harness run accumulates a JSONL buffer in memory. One record per event:

- `{"record":"start", invocation_id, wake_entry_id, personality_instance_id, root_perspective_memory_id, triggering_memory_id, model_target_ref, model_id, max_rounds, max_trace_bytes}`
- `{"record":"system_prompt", round_idx:0, text}` (once)
- `{"record":"user_seed", round_idx:0, text}` (once)
- `{"record":"assistant", round_idx, text?, tool_calls?, finish_reason, prompt_tokens, completion_tokens, duration_ms, raw_response_excerpt?}`
- `{"record":"tool_call", round_idx, call_id, tool_name, args}`
- `{"record":"tool_result", round_idx, call_id, status, content_excerpt, duration_ms}`
- `{"record":"provider_error", round_idx, class, message, retry_attempt}`
- `{"record":"finish", outcome_kind, failure_reason?, rounds_used, total_prompt_tokens, total_completion_tokens, total_duration_ms}`
- `{"record":"truncated", reason:"size_cap", cap_bytes, dropped_round_start}` (only when size cap fires)

The buffer enforces a per-invocation byte cap (default 5 MB, configurable per-owner). On cap hit, the harness writes a final `truncated` marker line and stops appending — **the wake itself does not fail**; `wake_invocation_log` still records every round, just without per-message bytes.

At wake finalization, the JSONL bytes are written as a `CitedObject`:
```rust
EventDraft {
    schema_id: "proxima-core/wake-trace-jsonl-v1",
    schema_version: 1,
    payload: jsonl_bytes,
    cited_object: CitedObjectHint {
        schema_id: "proxima-core/wake-trace-jsonl-v1",
        schema_version: 1,
        content_hash: blake3(jsonl_bytes),
    },
    // ...
}
```

#### Layer 2 — `wake_invocation_log` rows

The existing `wake_invocation_log` table (`crates/core/src/personality/...` → `engine.append_wake_invocation_log`) gains one new phase: `harness_round`. Each round produces one row:

| column | source |
|---|---|
| `phase` | `"harness_round"` |
| `round_idx` (new column) | loop counter |
| `role` (new column) | `assistant` / `tool_call` / `tool_result` / `provider_error` |
| `tool_id` | tool name when applicable |
| `status` | `succeeded` / `failed` / `truncated` |
| `duration_ms` | per-round wall time |
| `prompt_tokens` (new column) | provider-reported |
| `completion_tokens` (new column) | provider-reported |
| `finish_reason` (new column) | provider-reported, normalized |
| `message_tail` | last 2 KB of assistant text or error message |

`tool_call` rows for substrate/flavor tools keep being written by the existing path (now from the harness instead of from `DevMcpServer::call_tool`). One row, one event, one phase.

#### Layer 3 — `wake-trace-v1` Fact

After the harness returns, **before** invocation finalization closes out, the engine emits one Fact per invocation:

```rust
// New schema, registered in core flavor:
//   proxima-core/wake-trace-v1   (PayloadKind::Fact)

pub struct WakeTracePayload {
    pub invocation_id: Uuid,
    pub wake_entry_id: Uuid,
    pub personality_instance_id: Uuid,
    pub model_target_ref: String,
    pub model_id: String,
    pub started_at: OffsetDateTime,
    pub finished_at: OffsetDateTime,
    pub outcome_kind: String,              // "succeeded" | "truncated" | "failed"
    pub failure_reason: Option<String>,
    pub rounds_used: u16,
    pub finish_reason: Option<String>,
    pub total_prompt_tokens: Option<u64>,
    pub total_completion_tokens: Option<u64>,
    pub tool_call_count: u32,
    pub jsonl_truncated: bool,             // true if size cap fired
}
```

Edge wiring:
- `wake_trace → core/derived_from → triggering_memory` (always)
- `wake_trace → core/derived_from → root_perspective_memory` (always)
- `wake_trace → core/derived_from → active_goal_memory` (one edge per active goal at wake time)
- `wake_trace → core/cites → cited_object` (the JSONL CitedObject)
- For every Memory the wake authored during its rounds: `wake_trace → core/authored → output_memory`

The Citation is concrete: a new schema `proxima-core/wake-trace-citation-v1 (PayloadKind::CitationMapping)` maps the wake-trace Fact's claim ("invocation X produced these outputs from these inputs across N rounds") to the JSONL evidence. `cited_object.content_hash` is the BLAKE3 of the JSONL bytes, exactly as the existing `CitedObjectHint` already supports.

Fact emission is **last and non-blocking** for invocation status: if the Fact write fails (storage outage, schema mismatch), the wake still finalizes with its real outcome and a `wake_invocation_log` row records the Fact-emit failure. The wake's correctness does not depend on its own observability.

### What changes in `fire_wake_entry`

```rust
// crates/core/src/wake/fire/fire.rs (after migration, sketch)

pub async fn fire_wake_entry(...) -> Result<bool, ProtocolError> {
    // 0. self-wake guard                                     [unchanged]
    // 1. assemble four-param context                         [unchanged]
    // 2. resolve InferenceTarget                             [behavior shift: routes to harness adapter when target kind is Mistral/OpenAI/Anthropic; LocalCli still routes to Goose adapter during migration]
    // 3. mint wake token, INSERT invocation row              [unchanged]
    //
    //    REMOVED: step 7 (write_effective_recipe). No YAML rewriting.
    //
    // 4. build HarnessProgram from:
    //    - wake_context (4 params, typed)
    //    - WakeEntry.instructions
    //    - root_perspective.system_prompt
    //    - WakeEntry.substrate_tool_palette + workspace_tool_palette (typed ToolBindings)
    //    - workspace_context (if execution_mode = Workspace)
    // 5. select adapter from InferenceTargetConfig variant.
    // 6. adapter.run(program, harness_ctx) → HarnessOutcome
    // 7. emit wake-trace Fact + CitedObject (JSONL)          [NEW; non-blocking]
    // 8. revoke wake token; finalize invocation              [unchanged]
}
```

The four-param context, idempotency keys, chain-depth guard, workspace prepare/finalize trait, self-wake exclusion, and authorship-edge wiring are all untouched. The change is entirely below the `TargetAdapter::run` seam — plus the new Fact emission.

### Provider scope for v1

**Decision: B1.** Day one: Mistral + OpenAI-Responses (Codex) + OpenAI-Chat. Anthropic adapter scaffolded behind `#[cfg(feature = "claude")]` — wired but not default-built. This keeps the v1 surface focused on the two providers Heinrich is currently driving Proxima against and avoids shipping an untested Anthropic path.

### Staged Goose removal

#### Phase 1 — harness lands, off by default

- New `crates/harness/` crate, `HarnessAdapter` impl, three providers (Mistral, OpenAI-Chat, OpenAI-Responses).
- New `InferenceTargetConfig` variants alongside the deprecated `LocalCli`.
- New `WakeEntry.instructions` column; recipe YAML still primary.
- Dispatcher routes by `InferenceTargetConfig` variant: new variants → harness; `LocalCli` → existing Goose adapter.
- New `proxima-core/wake-trace-v1` schema + Fact emission **wired for the harness path only**. The Goose path keeps writing the existing `session_log_path` JSONL on disk during this phase.
- One flavor (Code's engineer personality) is migrated to a `Mistral` target as a smoke test. All other flavors stay on `LocalCli`/Goose.
- New tests:
  - per-provider replay tests with recorded HTTP fixtures (no live calls in CI)
  - workspace tool isolation tests (path-traversal rejection, env clearing, output capping)
  - outcome classifier exhaustiveness test (one case per row of the classification table)
  - Fact emission with truncated JSONL (cap fires, marker line present, Fact still emits)

#### Phase 2 — default flip + migration

- Migrate remaining flavors to native targets. `flavors/code/recipes/*.yaml` instruction bodies move into `WakeEntry.instructions` via one-shot script.
- Recipe YAML files **deleted** in the same commit.
- `recipe_resolve.rs`, `recipe_validate.rs`, `recipe.rs::write_effective_recipe` deleted.
- Goose adapter still ships for legacy `LocalCli` rows but is marked deprecated in the UI.
- The Goose path's `session_log_path` JSONL writer is replaced with the same `wake-trace-v1` Fact emission, so observability is uniform across both adapter types during the deprecation window.

#### Phase 3 — Goose removal

- `LocalCli` variant removed from `InferenceTargetConfig` (and the Postgres rows migrated to fail-loud on read, surfaced in a one-shot migration audit).
- `crates/core/src/wake/target_adapter/local_cli_goose.rs` deleted.
- `WakeEntry.recipe_ref` column dropped.
- Goose CLI dependency removed from `scripts/` and dev docs.

Net deletion at end of Phase 3: ~700 lines of Rust, 7 YAML files, one third-party CLI dependency, and the entire "regex-the-CLI-output" pattern.

### What stays valuable that we keep

- `WakeEntry` row shape (trigger, palette, max_rounds, model_tier, inference_target_ref).
- Four-param wake context — passed as typed JSON to the harness, no template engine involved.
- `InferenceTarget` indirection (the variants change; the indirection stays).
- In-process MCP server (`crates/mcp-server`) — for external callers; wakes simply bypass it.
- `McpToolDescriptor` registration and substrate-tool pack — the harness consumes the same registry, just calls the function pointers directly.
- `WorkspaceRunner` prepare/finalize trait — the prepared worktree, the workspace-context payload, the workspace facts (`workspace-run-v1`, `workspace-decision-v1`) all keep working.
- Wake-token store and palette-scope authorization — moved from the MCP transport layer to the harness's substrate-dispatch layer; same semantics.

### Risks and open questions

1. **Tool-call streaming.** Mistral and OpenAI both support streamed tool calls. v1 explicitly **does not** stream — the harness waits for full responses per round, then dispatches. Simpler control, identical wall-time for non-interactive wakes, easier to record into the JSONL deterministically. Streaming is v1.1 if it turns out to matter.
2. **Codex variant choice.** OpenAI's `/v1/responses` endpoint is the Codex-tier surface; `/v1/chat/completions` works for general models. The spec ships both adapters in v1 to give InferenceTarget rows a choice. If `gpt-5-codex` exclusively requires `/v1/responses`, the OpenAI-Chat adapter just won't be used for Codex models.
3. **Claude Code parity.** Personalities running Claude as their inference target use the `Anthropic` adapter against `api.anthropic.com` — independent of any local Claude Code installation. The external Claude Code integration via the Shell's master-token MCP surface continues unchanged.
4. **Provider tool-call argument validation.** When a provider's tool call carries arguments that don't validate against the tool's JSON schema, the harness returns a `Turn::ToolResult { status: Error, content: {"error":"schema_violation","details":...} }` and continues. The model gets a structured correction; the harness doesn't try to coerce. Defense-in-depth circuit breaker fires only if the same tool's args fail 5+ times in a row.
5. **JSONL bytes in the DB.** Per-invocation cap default is 5 MB. Empirical: a 30-round wake with moderate tool args averages 200–500 KB. At 1 MB/wake × 1000 wakes/day that's ~1 GB/day; well within Postgres comfort. If a deployment routinely pushes past 10 MB per trace, chunking the `CitedObject` body into `wake_trace_artifact_chunk(invocation_id, chunk_idx, bytes)` is a storage-layer detail that doesn't touch the Citation API.

## Out of scope

- Streaming tool-call responses (v1.1).
- A UI for editing `WakeEntry.instructions` (Phase 1 ships SQL-only; the existing recipe-picker UI in the Personalities view changes after Phase 2).
- Hosted/remote-engine inference targets (the `RemoteModel` variant exists in storage but the harness wires only direct provider HTTP for now).
- Goose recipe import tooling — Phase 2's one-shot migration covers bundled recipes; user recipes (`~/.proxima/recipes/<owner>/`) are out of scope, the user re-writes them as WakeEntry rows.
- Per-tool circuit-breaker tuning UI (defaults are hard-coded in v1).
