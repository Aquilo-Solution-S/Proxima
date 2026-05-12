# Proxima Harness — replacing Goose with an in-process LLM loop

**Status:** design
**Date:** 2026-05-12
**Owner:** Heinrich
**Scope:** `crates/core/src/wake/target_adapter/`, new `crates/harness/`, `crates/core/src/inference/`, `crates/mcp-server/`, `flavors/code/recipes/`, the `wake_invocation_log` storage path, and three new core schemas (`proxima-core/wake-trace-v1` Fact + `proxima-core/wake-trace-jsonl-v1` CitedObject + `proxima-core/wake-trace-citation-v1` CitationMapping).
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

1. **Native model tool-calling behind one provider interface.** Every provider adapter implements `ProviderClient::tool_round` and returns the same `RoundResult` / `ProviderError` vocabulary. Mistral Chat and OpenAI Chat both speak `/v1/chat/completions`, but they are separate adapter implementations because vendor quirks belong behind the interface, not in public `compat` flags. OpenAI Responses remains separate because `/v1/responses` is a different wire protocol. We **never** parse model prose to detect control flow. Termination is the provider's structural signal (`finish_reason`, Responses `status`, or provider-equivalent).
2. **Tools are typed bindings dispatched in-process.** Substrate and flavor tools route through the same wake-visible dispatch bridge that backs the live MCP server: registered `McpToolDescriptor`s first, then wake-scoped personality substrate-pack tools (`core/fetch_memory`, `core/emit_perspective`, etc.). No TCP loopback at wake time, no JSON-RPC envelope, no MCP transport layer in the hot path. Workspace tools implement a Rust `WorkspaceTool` trait. Provider tool-call arguments validate against `args_schema` before dispatch — invalid args produce a structured tool error message; the model self-corrects on the next round.
3. **Workspace toolkit is three Rust tools, not Goose's `developer` builtin.** `workspace_shell`, `workspace_text_editor`, `workspace_list_files`, each cwd-jailed to the prepared worktree. Their JSON schemas are generated from `schemars` derives. The model sees them as native function-calling tools; the harness dispatches them as Rust functions.
4. **Credentials are env vars resolved at wake time.** `InferenceTargetConfig` is rewritten to three adapter-selector variants: `MistralChat`, `OpenAIChat`, `OpenAIResponses`. Each carries `base_url`, `model_id`, and `api_key_env` (the name of the env var to read). The engine reads the env at fire time — not at startup — so users can rotate keys without restart. Missing env → invocation finalizes as `Failed("credentials_missing:MISTRAL_API_KEY")`, precise and structured. **No third-party CLI config file is consulted, ever.**
5. **Outcome derives from explicit signals, never regex.** The outcome classifier sees: HTTP status, provider-reported `finish_reason`, round counter, tool-dispatch results, exception class. The classification table (below) is exhaustive and deterministic.
6. **Every wake leaves three observability traces: a JSONL transcript, `wake_invocation_log` rows, and a `wake-trace-v1` Fact in the memory graph.** No layer is optional. The Fact is the substrate-native index; the log table is the SQL-queryable cross-cut; the JSONL is the forensic raw. The JSONL persists as a `CitedObject` content-addressed by BLAKE3 and pinned to the Fact via `CitationMapping`.

### Crate layout

**Dependency direction (no cycle):**

```
proxima-core   defines  HarnessAdapter trait + HarnessProgram / HarnessOutcome / HarnessError
                        (mirrors the existing TargetAdapter seam at
                        crates/core/src/wake/target_adapter/mod.rs)

crates/harness depends on proxima-core. Holds the loop driver, ProviderClient
                impls, workspace tools, JSONL trace buffer.

apps/proxima-shell  depends on both, instantiates the concrete HarnessAdapter
                    and registers it on Engine at boot, exactly like the
                    LocalCliGooseAdapter is wired today.
```

`fire_wake_entry` already takes `&dyn TargetAdapter` — the v2 path will take `&dyn HarnessAdapter` the same way. Core never names a concrete provider type; the harness crate is the only place that touches `reqwest`, provider HTTP shapes, or `tokio::process::Command` for `workspace_shell`. This keeps `proxima-core` free of network/runtime deps.

```
crates/core/src/harness/        # trait + value types only, no runtime deps
  mod.rs                          # HarnessAdapter trait, HarnessProgram, HarnessOutcome,
                                  # HarnessContext, HarnessError, FinishReason, ErrorClass

crates/harness/                  # the concrete implementation
  Cargo.toml                      # depends on proxima-core; pulls reqwest, tokio runtime
  src/
    lib.rs                         # public surface: HarnessLoop (concrete adapter), prelude re-exports
    program.rs                     # HarnessProgram builder: 4-param wake context → typed Conversation seed + tool palette
    conversation.rs                # Conversation, Message, ToolCall, ToolResult — provider-neutral types
    loop.rs                        # the wake-loop driver: model.tool_round() → dispatch_tools() → repeat
    tools/
      mod.rs                       # ToolBinding enum: Substrate | Flavor | Workspace
      substrate_dispatch.rs        # in-process call into HarnessSubstrateBridge (McpToolHost impl preserves registry MCP + personality substrate pack)
      workspace/
        mod.rs                     # WorkspaceTool trait
        shell.rs                   # bounded bash; timeout, output cap, exit code structural
        text_editor.rs             # view | create | str_replace | insert; path-jailed
        list_files.rs              # cwd-rooted recursive listing
    providers/
      mod.rs                       # ProviderClient trait + ProviderError
      chat_completions_wire.rs     # crate-private (`mod`, not `pub mod`) shared DTOs/parser for chat completions — visibility enforces the no-public-compat boundary
      mistral_chat.rs              # Mistral /v1/chat/completions adapter
      openai_chat.rs               # OpenAI /v1/chat/completions adapter
      openai_responses.rs          # /v1/responses (Codex tier)
      # anthropic.rs is OUT OF v1 SCOPE — added in a follow-up spec
    trace/
      jsonl.rs                     # in-memory JSONL buffer with size cap + truncate-marker
      wake_trace_fact.rs           # emit_wake_trace_fact: builds Fact + Citation + edges
```

### Core traits

```rust
// crates/core/src/harness/mod.rs
//
// Trait + value types only. proxima-core never sees a provider impl.

#[async_trait]
pub trait HarnessAdapter: Send + Sync {
    async fn run(&self, program: HarnessProgram, ctx: HarnessContext)
        -> Result<HarnessOutcome, HarnessError>;
}

// crates/harness/src/lib.rs
//
// Concrete adapter only. One impl: HarnessLoop.
// HarnessLoop selects MistralChatClient, OpenAIChatClient, or
// OpenAIResponsesClient from the typed ProviderTarget and then runs the
// shared loop against &dyn ProviderClient. Per-provider differences live
// behind ProviderClient, not in HarnessContext or core.
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
    pub tool_name: String,          // canonical, after reverse-mapping from provider-safe
    pub arguments: serde_json::Value,
}

/// Two-way name mapping. Providers impose stricter naming rules on
/// tool/function names than Proxima's canonical schema ids, so
/// provider-safe normalization is mandatory regardless of which
/// specific characters each provider rejects. The harness uses the
/// existing `crates/core/src/mcp/mod.rs::provider_safe_tool_name`
/// helper (already consumed by `crates/mcp-server/src/handler.rs`)
/// for the forward map and stores the reverse map per round so the
/// model's `function.name` reply is resolved back to the canonical id
/// before palette/auth dispatch.
pub struct ToolSpec {
    pub canonical: String,                   // e.g. "core/emit_abstraction"
    pub provider_safe: String,               // e.g. "core_emit_abstraction"
    pub description: String,
    pub input_schema: serde_json::Value,     // JSON Schema for the model
    pub binding: ToolBinding,                // Substrate | Flavor | Workspace
}

/// Harness owns the round-trip: send provider-safe names to the model;
/// reverse-map the call's `function.name` (or equivalent) to `canonical`
/// before palette membership and `McpAuthStore` scope checks run.
/// A name the model returns that doesn't reverse-map is a structural
/// error (model fabricated a tool); the dispatcher records a
/// `phase: "tool_call", outcome: "unknown_tool"` log row and feeds an
/// error tool result back to the model rather than crashing the wake.

pub struct ToolResultTurn {
    pub call_id: String,
    pub status: ToolStatus,         // Ok | Error
    pub content: serde_json::Value, // tool-defined; structurally typed
}
```

### The loop

```rust
// crates/harness/src/loop.rs (sketch)

pub async fn run_loop(
    provider: &dyn ProviderClient,
    program: HarnessProgram,
    ctx: HarnessContext,
)
    -> Result<HarnessOutcome, HarnessError>
{
    let mut conv = program.seed_conversation();
    let mut trace = TraceBuffer::new(ctx.max_trace_bytes);
    trace.start(&program, &ctx);

    for round_idx in 0..ctx.max_rounds {
        let round_started = Instant::now();
        let round = provider.tool_round(&conv, &program.tool_specs, ctx.cancel.clone()).await;
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
    MistralChat(MistralChatConfig),
    #[serde(rename = "openai_chat")]
    OpenAIChat(OpenAIChatConfig),
    #[serde(rename = "openai_responses")]
    OpenAIResponses(OpenAIResponsesConfig),  // Codex tier
}

pub struct MistralChatConfig {
    pub base_url: String,            // default "https://api.mistral.ai"
    pub model_id: String,            // e.g. "mistral-medium-3.5"
    pub api_key_env: String,         // env var name to read at wake time
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

pub struct OpenAIChatConfig {
    pub base_url: String,            // default "https://api.openai.com"
    pub model_id: String,            // e.g. "gpt-5.1"
    pub api_key_env: String,         // default "OPENAI_API_KEY"
    pub temperature: Option<f32>,
    pub max_completion_tokens: Option<u32>,
}

pub struct OpenAIResponsesConfig {
    pub base_url: String,            // default "https://api.openai.com"
    pub model_id: String,            // e.g. "gpt-5-codex"
    pub api_key_env: String,         // default "OPENAI_API_KEY"
    pub reasoning_effort: Option<ReasoningEffort>,   // low | medium | high
}

```

**Anthropic deferred.** Out of v1 scope. Day-one harness ships MistralChat + OpenAIChat + OpenAIResponses only. A follow-up spec adds Anthropic or native Mistral Conversations as a peer adapter variant when needed. This matches the repo's no-feature-flag flavor discipline.

**Provider extension boundary.** `ProviderClient` is the unified harness contract; `InferenceTargetConfig` variants select implementations. Do not add a public `compat` struct or provider-quirk flags for auth shape, token field names, endpoint behavior, retry semantics, tool-call shape, or finish-reason normalization. If a provider differs from an existing adapter, add a small adapter implementing `ProviderClient`; share private wire helpers where useful. The boundary is enforced mechanically by Rust visibility: `chat_completions_wire` is declared `mod chat_completions_wire;` (not `pub mod`) in `providers/mod.rs`, so its entire contents are unreachable from outside the `proxima-harness` crate regardless of `pub` markers inside. Adapter modules access it via `super::chat_completions_wire`. Changing the module to `pub mod` is the single line a reviewer must reject.

**Wire discriminants.** `InferenceTargetConfig` uses `#[serde(tag = "kind", rename_all = "snake_case")]`, plus explicit OpenAI variant renames: `OpenAIChat` serializes as `openai_chat`, and `OpenAIResponses` as `openai_responses`. Do not rely on acronym inference (`open_ai_chat` / `open_a_i_chat`) in migrations, Shell TOML, or tests.

**Greenfield cut, no transition window.** The current enum is `LocalCli | RemoteModel { vendor, dialect, model_id, credentials_ref }` (verified at `crates/core/src/inference/types.rs:22-40`). Both variants are dropped in the same change that lands the new ones — there is no co-existence period, no `#[deprecated]` lane, no migration shim. Existing `inference_targets` rows are translated by a one-shot data migration that runs against every Owner before the new enum ships. The migration updates both storage discriminators: `inference_targets.kind` and `inference_targets.config->>'kind'`.

| Source row | Target variant |
|---|---|
| `RemoteModel { vendor="mistral", ... }` | `MistralChat { model_id, api_key_env: derive_from(credentials_ref), base_url: default }` |
| `RemoteModel { vendor="openai", dialect="chat", ... }` | `OpenAIChat { ... }` |
| `RemoteModel { vendor="openai", dialect="responses", ... }` | `OpenAIResponses { ... }` |
| `LocalCli { ... }` | hand-mapped to a MistralChat/OpenAIChat/OpenAIResponses target by the operator running the cut |
| anything else | migration aborts loudly |

The migration runs once, transactionally per-Owner; any row it cannot map terminates the cut before it commits. It drops the old `inference_targets_kind_chk` (`local_cli | remote_model`) and recreates it as `mistral_chat | openai_chat | openai_responses` after all rows satisfy `kind = config->>'kind'`. There is no fallback to Goose at runtime — the moment the new code is live, every wake fires through the harness or fails the dispatch entirely.

The Shell config (`apps/proxima-shell/src-tauri/src/config/types.rs::InferenceTargetRecord`) reflects the three adapter-selector variants; TOML round-trip tests cover MistralChat, OpenAIChat, and OpenAIResponses records.

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

The model sees substrate and flavor tools as ordinary function-calling tools. Tool specs come from `HarnessSubstrateBridge::list_harness_tools`, implemented by `McpToolHost` as the same combined wake-visible inventory the HTTP MCP path lists: `FlavorRegistryFrozen::list_mcp_tools()` plus `McpToolHost::substrate_tools()` / `personality::substrate_pack()`.

When the model emits a tool call, the harness:

1. Reverse-maps the provider-safe name to the canonical tool id.
2. Calls `HarnessSubstrateBridge::call_harness_tool` with the canonical id, args, owner, model author context, and wake token.
3. The `McpToolHost` implementation resolves the wake token into `McpAuthContext`, defaults `McpAuthorContext.caller_self_perspective` from the wake's root perspective when absent, and calls `McpToolHost::call_tool`, preserving the existing registry-first / `call_personality_tool` fallback path. No HTTP transport. No JSON-RPC. Result is already a typed `serde_json::Value`.
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
- `WakeEntry.recipe_ref` column is dropped in the same change (no compatibility window).

**Provisioning defaults (where flavor-shipped instructions come from after YAML).**

Killing the YAML files removes the build-time source of the default `instructions:` body that each flavor ships for its bundled personalities. Storage on `WakeEntry.instructions` doesn't answer "what does it get initialised to when a brand-new Owner sets up Proxima for the first time?"

The replacement is build-time-only, Rust-native:

```rust
// crates/core/src/personality/mod.rs (or each flavor crate)

pub struct DefaultWakeEntrySeed {
    pub trigger_kind: TriggerKind,
    pub trigger_id: TriggerId,
    pub palette: ToolPaletteSeed,
    pub max_rounds: u16,
    pub model_tier: ModelTier,
    pub execution_mode: ExecutionMode,
    pub instructions: &'static str,   // the body that used to live in recipes/*.yaml
}

pub trait Flavor {
    fn default_personalities(&self) -> Vec<DefaultPersonalitySeed>;
    // where DefaultPersonalitySeed contains the Root P perspective + a Vec<DefaultWakeEntrySeed>
}
```

The current owner-default-provisioning path — the same path that today seeds the Engineer and Execution Worker personalities on a fresh Owner — copies `instructions` straight into the new `WakeEntry.instructions` column. (The exact module is to be located during implementation; the spec deliberately doesn't pin a path that may move.) Flavor authors edit their `instructions: &'static str` constants in Rust source; the value flows through `cargo build` straight into provisioning, just like every other flavor-shipped constant. No runtime templating, no YAML, no on-disk recipe registry.

This satisfies the build-time-only contract that the flavor system already holds for tool schemas, perspective payloads, and registry entries (see `feature_no_doc_duplication` in repo discipline). The two existing recipes (`flavors/code/recipes/engineer.yaml` and `execution_worker.yaml`) become two `DefaultWakeEntrySeed` constants in `flavors/code/src/personalities.rs` — the `instructions:` field of `execution_worker.yaml` lines 43-75 moves verbatim to a `const EXECUTION_WORKER_INSTRUCTIONS: &str = ...` string.
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

At wake finalization, the JSONL bytes become the `CitedObject` *body*, content-addressed by `blake3(jsonl_bytes)`. The Fact emission (Layer 3 below) is the place where the JSONL bytes are pinned to the Fact via the `CitationMapping` — the JSONL is the cited artefact, not the Fact payload.

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

`tool_call` rows for substrate/flavor tools keep being written by the existing path (now from the harness instead of from `McpToolHost::call_tool`). One row, one event, one phase.

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

**Edge wiring (corrected against `docs/11`).** Citations are not edges — they're `Memory.citation_mapping_id` → one `CitationMapping` → one `CitedObject`. Authorship of the wake_trace Fact follows the canonical direction `Root Perspective --core/authored--> Fact`, same as every other Fact the wake emits. The output memories the wake authored during its rounds are not linked to the trace by edge; they're queryable via shared authoring Root P + the `wake_invocation_log` `[started_at, finished_at]` window.

Edges actually emitted:
- `root_perspective --core/authored--> wake_trace_fact` (written by the `persist_wake_trace` verb described below — *not* by EventIngest, which stamps external/nil authorship)
- `wake_trace_fact --core/derived-from--> triggering_memory` (always)
- `wake_trace_fact --core/derived-from--> root_perspective_memory` (always)
- `wake_trace_fact --core/derived-from--> active_goal` — **one per active Goal entity at wake time**. The edge is written with `target_kind = "Goal"`, `target_goal_id = Some(goal_id)`, `target_memory_id = None`. Goals are entities (not a `Memory.kind`); `Storage::list_active_goals` already returns `Vec<ActiveGoalSummary>` keyed on `goal_id: GoalId`.

**The `EventIngest` verb is not used for wake-trace emission.** Two reasons. (1) `EventIngest`'s memory row stamps `personality_instance_id = '00000000-...-000'` (external authorship); the wake-trace Fact must record the *authoring* `PersonalityInstanceId` so cross-personality audit queries (`Memory.personality_instance_id`) hit. (2) `EventIngest` writes only the core rows (`cited_object`, `event`, `memory`, `citation_mapping`, `change_event`) — it does not write the `wake_trace_v1` / `cited_wake_trace_jsonl_v1` / `citation_wake_trace_v1` sidecar payload rows, and it does not write the `core/authored` + `core/derived-from` edges. Reusing it would leave six rows missing and force a follow-up write that wouldn't be atomic with the Fact insertion.

Instead, a dedicated atomic storage verb — `persist_wake_trace` — lives in `crates/core/src/verbs/persist_wake_trace.rs` (typed input/outcome), with the Postgres implementation in `crates/storage-pg/src/verbs/persist_wake_trace.rs`. **The wiring goes through the existing `Storage` trait**: `proxima-core` never depends on `proxima-storage-pg` (it delegates through `Storage`, see `crates/core/src/storage.rs:61`), so the verb is added as `Storage::persist_wake_trace_atomic(&self, registry: &FlavorRegistryFrozen, input: &WakeTracePersistInput)`, `NoopStorage` returns the standard `StorageError::Internal("NoopStorage rejects writes")`, and `PgStorage` delegates to the module-level `persist_wake_trace_atomic` helper. `Engine::persist_wake_trace` (public, auth-checked) and `Engine::persist_wake_trace_internal` (crate-private, called from `fire_wake_entry` after the wake-token store has authorised the dispatcher) both call `self.storage.persist_wake_trace_atomic(&self.registry, &input)`. One transaction writes the following rows in order:

1. `cited_objects` (JSONL CitedObject row, dedup-keyed on `(owner, schema_id, content_hash)`).
2. `proxima_core.cited_wake_trace_jsonl_v1` sidecar (`body bytea`, `byte_len`, `line_count`, `truncated bool`).
3. `source_batches` upsert.
4. `events` (`proxima-core/wake-trace-v1` row).
5. `memories` (Fact, `personality_instance_id = authoring_personality_instance_id`).
6. `citation_mappings` row.
7. `proxima_core.citation_wake_trace_v1` sidecar (`byte_range_start`, `byte_range_end`).
8. `proxima_core.wake_trace_v1` sidecar (the typed Fact payload columns above).
9. `change_event` (`EntityAppend` / `Fact`).
10. `edges` — `root_perspective --core/authored--> wake_trace_fact` (Engine authorship).
11. `edges` — `wake_trace_fact --core/derived-from--> triggering_memory`.
12. `edges` — `wake_trace_fact --core/derived-from--> root_perspective_memory`.
13. `edges` — `wake_trace_fact --core/derived-from--> active_goal_i` for each active Goal entity at wake time (`target_kind = "Goal"`, `target_goal_id = Some(goal_id)`, `target_memory_id = None`).

The input is one struct (`WakeTracePersistInput`) carrying the JSONL bytes, the `WakeTracePayload` Fact fields, the authoring `PersonalityInstanceId`, the Root-Perspective `MemoryId`, the triggering `MemoryId`, and the active `GoalId`s (`Vec<GoalId>`, sourced from `Storage::list_active_goals` — these are Goal entities, not memories). The outcome (`WakeTracePersistOutcome`) returns `event_id`, `fact_memory_id`, `cited_object_id`, `citation_mapping_id`, `change_event_seq`, and an `idempotent_replay: bool` flag.

**Two distinct idempotency layers (do not conflate):**

- **Whole-verb replay** — keyed on `WakeTracePersistInput::event_id()` (BLAKE3 over source-id, owner, content_hash, **and** `invocation_id`). A re-issued persist with the same `event_id` collides on `memories.event_id` and the verb short-circuits, returning the original outcome row ids with `idempotent_replay = true`. Two *distinct* wake invocations that happen to produce byte-identical JSONL produce *different* `event_id`s (different `invocation_id`) and do NOT collapse.
- **Cited-object row dedup** — `UNIQUE (owner, schema_id, content_hash)` on `cited_objects` (docs/11 §"Idempotency"). When the whole-verb replay check misses but the content hash matches a prior CitedObject row (e.g. the wake produced JSONL identical to an earlier wake's), the `ON CONFLICT DO UPDATE … RETURNING cited_object_id` clause returns the existing row; the new wake-trace Fact + CitationMapping point at the shared CitedObject. This is row-level dedup of the artefact only — it does NOT short-circuit the Fact or the CitationMapping write.

The verb is non-blocking for invocation finalization — wrap the call in a `tokio::task::spawn` and `?`-log the result against the same `wake_invocation_log` row. The wake's outcome is decided by the harness; the trace is the recorder, not the gate.

Three new registered schemas (all in core, all `PayloadKind` per docs/11):

| Schema id | Kind | Sidecar fields |
|---|---|---|
| `proxima-core/wake-trace-v1` | Fact | `WakeTracePayload` columns above |
| `proxima-core/wake-trace-jsonl-v1` | CitedObject | `s3_path` (or local path during dev), `byte_len`, `line_count`, `truncated: bool` |
| `proxima-core/wake-trace-citation-v1` | CitationMapping | `byte_range: [u64; 2]` (optional, defaults to whole-blob) |

If we ever need an explicit "this invocation produced this Memory" relation, register it then. v1 doesn't.

Fact emission is **last and non-blocking** for invocation status: if the Fact write fails (storage outage, schema mismatch), the wake still finalizes with its real outcome and a `wake_invocation_log` row records the Fact-emit failure. The wake's correctness does not depend on its own observability.

### What changes in `fire_wake_entry`

```rust
// crates/core/src/wake/fire/fire.rs (after migration, sketch)

pub async fn fire_wake_entry(...) -> Result<bool, ProtocolError> {
    // 0. self-wake guard                                     [unchanged]
    // 1. assemble four-param context                         [unchanged]
    // 2. resolve InferenceTarget                             [variant must be MistralChat | OpenAIChat | OpenAIResponses; no other variant exists post-cut]
    // 3. mint wake token, INSERT invocation row              [unchanged]
    //    After this point, pre-run failures are wake outcomes, not
    //    propagated dispatcher errors. Missing credentials finalize as
    //    Failed("credentials_missing:{ENV}") and revoke the wake token.
    //
    //    REMOVED: step 7 (write_effective_recipe). No YAML rewriting.
    //
    // 4. build HarnessProgram from:
    //    - wake_context (4 params, typed)
    //    - WakeEntry.instructions
    //    - root_perspective.system_prompt
    //    - WakeEntry.substrate_tool_palette ids (resolved by HarnessLoop through HarnessSubstrateBridge)
    //    - WakeEntry.workspace_tool_palette (workspace tools gated by workspace_root)
    //    - workspace_context (if execution_mode = Workspace)
    // 5. select adapter from InferenceTargetConfig variant.
    // 6. adapter.run(program, harness_ctx) → HarnessOutcome
    // 7. call engine.persist_wake_trace_internal(WakeTracePersistInput {..})
    //    — crate-private path that bypasses auth_resolver (the wake-token
    //    store already authorised this dispatcher). Delegates to
    //    Storage::persist_wake_trace_atomic; PgStorage writes Fact + JSONL
    //    CitedObject + CitationMapping + sidecar rows + core/authored +
    //    core/derived-from edges in one tx. Non-blocking for invocation
    //    status; failure logs against wake_invocation_log but doesn't fail
    //    the wake.                                             [NEW]
    // 8. revoke wake token; finalize invocation              [unchanged]
}
```

The four-param context, idempotency keys, chain-depth guard, workspace prepare/finalize trait, self-wake exclusion, and authorship-edge wiring are all untouched. The change is entirely below the `HarnessAdapter::run` seam (renamed from `TargetAdapter::run` in the same cut) — plus the new Fact emission.

### Provider scope for v1

**Decision: B1, revised.** Day one: MistralChat + OpenAIChat + OpenAIResponses (Codex). Mistral/OpenAI Chat share private Chat Completions wire helpers but stay separate adapter implementations. No Anthropic, no feature flag. Adding a fourth provider becomes its own peer spec when needed — gating an untested adapter behind a `claude` flag inside v1 just builds dead code paths and breaks the repo's no-feature-flag flavor discipline.

### Single-cut Goose removal

Greenfield. No staging, no `#[deprecated]`, no coexistence window. Goose, the YAML, the `LocalCli` adapter, the `RemoteModel` enum variant, the recipe rewriter — all leave in the same change-set that lands the harness. The repo is on `road-to-v1` and no external consumer depends on the current shape.

**Lands together:**

- `crates/harness/` crate with `HarnessAdapter` impl and three provider adapters (MistralChat, OpenAIChat, OpenAIResponses).
- `InferenceTargetConfig` rewritten to the three-variant enum above; one-shot data migration translates every existing row before the cut commits (table above).
- New `WakeEntry.instructions` column populated from each flavor's `DefaultWakeEntrySeed.instructions` constant.
- `proxima-core/wake-trace-v1` Fact + `wake-trace-jsonl-v1` CitedObject + `wake-trace-citation-v1` CitationMapping schemas registered.
- `crates/core/src/wake/target_adapter/local_cli_goose.rs` deleted.
- `crates/core/src/wake/target_adapter/mod.rs` `TargetAdapter` trait replaced by `HarnessAdapter` (same seam, new contract).
- `crates/core/src/wake/fire/recipe.rs` (`write_effective_recipe`, `workspace_tool_supported`, recipe-rewrite helpers) deleted.
- `crates/core/src/wake/fire/recipe_resolve.rs` and `recipe_validate.rs` deleted.
- `flavors/code/recipes/engineer.yaml` and `execution_worker.yaml` deleted; their `instructions:` bodies move into `flavors/code/src/personalities.rs` as `&'static str` constants.
- `WakeEntry.recipe_ref` column dropped; the `extensions:` rewriter that injects MCP URLs and palettes is gone.
- Goose CLI dependency removed from `scripts/` and dev docs in the same commit.

**Tests that must be green before the cut merges:**

- per-provider replay tests with recorded HTTP fixtures (no live calls in CI)
- workspace tool isolation tests (path-traversal rejection, env clearing, output capping)
- outcome classifier exhaustiveness test (one case per row of the classification table)
- Fact emission with truncated JSONL (cap fires, marker line present, Fact still emits, CitedObject content_hash matches the truncated bytes)
- end-to-end wake against a recorded MistralChat fixture: fires, dispatches workspace tools, emits expected Memory + wake-trace Fact + CitedObject + CitationMapping
- end-to-end migration test: a database seeded with `LocalCli` and `RemoteModel` rows is translated cleanly into the new variants by the migration; one unmappable row aborts the migration.

Net deletion in the cut: ~700 lines of Rust, every recipe YAML, the entire "regex-the-CLI-output" pattern, the Goose subprocess path, and the recipe-rewriter middle layer.

### What stays valuable that we keep

- `WakeEntry` row shape (trigger, palette, max_rounds, model_tier, inference_target_ref).
- Four-param wake context — passed as typed JSON to the harness, no template engine involved.
- `InferenceTarget` indirection (the variants change; the indirection stays).
- In-process MCP server (`crates/mcp-server`) — external callers use HTTP MCP; wakes reuse `McpToolHost` as an in-process bridge and bypass only the transport.
- `McpToolDescriptor` registration and substrate-tool pack — the harness consumes the same combined wake-visible surface through `McpToolHost`'s bridge, preserving `call_personality_tool` semantics.
- `WorkspaceRunner` prepare/finalize trait — the prepared worktree, the workspace-context payload, the workspace facts (`workspace-run-v1`, `workspace-decision-v1`) all keep working.
- Wake-token store and palette-scope authorization — moved from the MCP transport layer to the harness's substrate-dispatch layer; same semantics.

### Risks and open questions

1. **Tool-call streaming.** Mistral and OpenAI both support streamed tool calls on Chat Completions. v1 explicitly **does not** stream — the harness waits for full responses per round, then dispatches. Simpler control, identical wall-time for non-interactive wakes, easier to record into the JSONL deterministically. Streaming is v1.1 if it turns out to matter.
2. **Codex variant choice.** OpenAI's `/v1/responses` endpoint is the Codex-tier surface; `/v1/chat/completions` works for compatible general models. The spec ships both OpenAI adapters in v1 to give InferenceTarget rows a choice. If `gpt-5-codex` exclusively requires `/v1/responses`, OpenAIChat just won't be used for Codex models.
3. **External Claude Code integration unaffected.** v1 ships no Anthropic provider in the harness — personalities cannot run Claude as their inference target until a follow-up spec lands one. The unrelated external integration where Claude Code connects *to* the Shell's master-token MCP surface (as a client) continues unchanged; it never went through the harness.
4. **Provider tool-call argument validation.** When a provider's tool call carries arguments that don't validate against the tool's JSON schema, the harness returns a `Turn::ToolResult { status: Error, content: {"error":"schema_violation","details":...} }` and continues. The model gets a structured correction; the harness doesn't try to coerce. Defense-in-depth circuit breaker fires only if the same tool's args fail 5+ times in a row.
5. **JSONL bytes in the DB.** Per-invocation cap default is 5 MB. Empirical: a 30-round wake with moderate tool args averages 200–500 KB. At 1 MB/wake × 1000 wakes/day that's ~1 GB/day; well within Postgres comfort. If a deployment routinely pushes past 10 MB per trace, chunking the `CitedObject` body into `wake_trace_artifact_chunk(invocation_id, chunk_idx, bytes)` is a storage-layer detail that doesn't touch the Citation API.

## Out of scope

- Streaming tool-call responses (v1.1).
- A UI for editing `WakeEntry.instructions` (v1 ships SQL-only; the existing recipe-picker UI in the Personalities view is rewritten against the new column in a follow-up).
- Hosted/remote-engine inference targets (only direct provider HTTP is wired in v1).
- Goose recipe import tooling — the one-shot data migration covers the bundled recipes shipped in this repo; user recipes living anywhere outside the repo are not migrated, the user re-writes them as `WakeEntry.instructions` rows.
- Per-tool circuit-breaker tuning UI (defaults are hard-coded in v1).
