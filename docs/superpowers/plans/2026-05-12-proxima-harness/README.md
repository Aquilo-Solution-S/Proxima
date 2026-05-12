# Proxima Harness Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans` to implement this plan task-by-task. Each task lives in its own file; steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the `LocalCliGooseAdapter` subprocess + recipe-YAML rewriter with an in-process Rust LLM harness that owns the wake loop, dispatches tools natively, and persists every session as a `wake-trace-v1` Fact + JSONL `CitedObject`.

**Architecture:** A new `crates/harness/` crate plugs into the wake dispatcher through a `HarnessAdapter` trait defined in `proxima-core`. The harness drives a `ProviderClient` per round; MistralChat, OpenAIChat, and OpenAIResponses are small adapter implementations behind that unified interface. Mistral/OpenAI Chat share private Chat Completions wire helpers, but vendor quirks stay adapter-private — no public `compat` object. The harness dispatches substrate + flavor + workspace tools in-process and emits structural outcomes from provider signals (never regex). The wake-trace Fact is written by a dedicated atomic storage verb (`persist_wake_trace`) that handles personality-attributed authorship + sidecar rows + provenance edges in one transaction — `EventIngest` cannot be reused because it stamps external/nil authorship and writes no sidecars or edges. Greenfield single-cut: Goose, recipe YAML, `LocalCli`/`RemoteModel` variants, and the recipe rewriter all leave in the same atomic commit that wires the harness into `fire_wake_entry`.

**Tech Stack:** Rust 2024 edition, `reqwest` (rustls), `tokio`, `schemars` v1, `serde_json`, `async-trait`, `blake3`, `sqlx` (postgres), `tracing`. New crate `crates/harness/`; touches `crates/core`, `crates/storage-pg`, `flavors/code`, `apps/proxima-engine`, `apps/proxima-shell`, `apps/proxima-code`, `apps/proxima-mcp`.

**Reference spec:** `docs/superpowers/specs/2026-05-12-proxima-harness-design.md`. The spec is authoritative — when this plan summarises, the spec wins.

---

## Phase landability summary

| Phase | Lands as | Affects existing wake path? |
|---|---|---|
| 1. `HarnessAdapter` trait + value types + outcome classifier — **DONE** | own commit | no — additive in `proxima-core` |
| 2. `crates/harness` skeleton + MistralChat provider + JSONL buffer — **DONE** | own commit | no — new crate not yet wired |
| 3. Three workspace tools — **DONE** | own commit | no — additive in harness crate |
| 4. Substrate/flavor dispatch + reverse-map + `HarnessLoop` driver | own commit | no — additive in harness crate |
| 5. OpenAIChat + OpenAIResponses providers | own commit | no — additive in harness crate |
| 6. `WakeEntry.instructions` column + `DefaultWakeEntrySeed` constants + onboarding wiring | own commit | additive — column is unread by Goose path |
| 7. Three wake-trace schemas + two new payload traits + `persist_wake_trace` atomic verb | own commit | additive — verb exists but no caller yet |
| 8. **THE CUT** — `InferenceTargetConfig` rewrite, harness wired into `fire_wake_entry`, `persist_wake_trace` called from the new emit path, file deletions, data migration, end-to-end test | one atomic commit | yes — replaces Goose at runtime |

Phases 1–7 are land-anytime; Phase 8 is the single atomic change where Goose, recipe YAML, `LocalCli`, `RemoteModel`, and the recipe rewriter all leave together.

---

## Task index

Each task is self-contained — open the file, follow the steps, commit. Subagent-driven execution: hand one task file at a time to a fresh subagent.

### Phase 1 — `HarnessAdapter` trait + value types + outcome classifier — **DONE**

- [x] [`task-01-harness-trait-and-types.md`](task-01-harness-trait-and-types.md) — Create `crates/core/src/harness/` module (`HarnessAdapter`, `HarnessProgram`, `HarnessContext`, `HarnessOutcome`, `HarnessError`, `ProviderTarget`, `SubstrateToolBinding`)
- [x] [`task-02-outcome-classifier.md`](task-02-outcome-classifier.md) — Exhaustive classifier test (spec outcome rows)

Verification: `rustfmt --check crates/core/src/harness/mod.rs crates/core/src/harness/outcome.rs crates/core/tests/harness_outcome_classifier.rs`; `cargo test -p proxima-core --test harness_outcome_classifier`; `cargo build -p proxima-core`; `cargo test --workspace`.

### Phase 2 — `crates/harness` skeleton + MistralChat provider + JSONL buffer — **DONE**

- [x] [`task-03-harness-crate-skeleton.md`](task-03-harness-crate-skeleton.md) — Workspace member + Cargo.toml + module stubs (stubs created BEFORE first `cargo build`)
- [x] [`task-04-conversation-types.md`](task-04-conversation-types.md) — `Conversation`, `Turn`, `ToolCall`, `ToolSpec`, `AssistantTurn`, `ToolResultTurn`
- [x] [`task-05-provider-trait-and-mistral-chat.md`](task-05-provider-trait-and-mistral-chat.md) — `ProviderClient` trait + private Chat Completions wire helpers + `MistralChatClient`
- [x] [`task-06-jsonl-buffer.md`](task-06-jsonl-buffer.md) — In-memory JSONL transcript with size cap + truncate marker
- [x] [`task-07-mistral-chat-replay-test.md`](task-07-mistral-chat-replay-test.md) — MistralChat replay against recorded HTTP fixtures (no live calls)

Verification: `cargo build -p proxima-harness`; `cargo test -p proxima-harness --test jsonl_buffer`; `cargo test -p proxima-harness --test mistral_chat_replay`; `cargo fmt -p proxima-harness --check`; `cargo clippy -p proxima-harness --all-targets`; `cargo test -p proxima-harness`; `cargo test --workspace`.

### Phase 3 — Three workspace tools — **DONE**

- [x] [`task-08-workspace-tool-trait.md`](task-08-workspace-tool-trait.md) — `WorkspaceTool` trait + `jail_path` cwd-jail helper + registry
- [x] [`task-09-workspace-shell.md`](task-09-workspace-shell.md) — `workspace_shell` with `bash -lc`, env-clear, 32 KB output cap, timeout
- [x] [`task-10-workspace-text-editor.md`](task-10-workspace-text-editor.md) — `workspace_text_editor` with View/Create/StrReplace/Insert ops
- [x] [`task-11-workspace-list-files.md`](task-11-workspace-list-files.md) — `workspace_list_files` with hidden-skip + entry cap

Verification: `cargo test -p proxima-harness --test workspace_shell -- --test-threads=1`; `cargo test -p proxima-harness --test workspace_text_editor`; `cargo test -p proxima-harness --test workspace_list_files`; `cargo fmt -p proxima-harness --check`; `cargo test -p proxima-harness`; `cargo clippy -p proxima-harness --all-targets`; `cargo test --workspace`.

### Phase 4 — Substrate/flavor dispatch + reverse-map + `HarnessLoop` driver

- [`task-12-program-builder-name-map.md`](task-12-program-builder-name-map.md) — `HarnessProgram::resolve` builder with canonical↔provider-safe name maps
- [`task-13-substrate-dispatch.md`](task-13-substrate-dispatch.md) — Substrate dispatch via `HarnessSubstrateBridge` implemented by `McpToolHost` (registry MCP tools + personality substrate pack)
- [`task-14-harness-loop-driver.md`](task-14-harness-loop-driver.md) — Full `HarnessLoop` driver with multi-round dispatch, JSONL logging, outcome classification
- [`task-15-substrate-dispatch-test.md`](task-15-substrate-dispatch-test.md) — Program-builder name-map round-trip tests

### Phase 5 — OpenAIChat + OpenAIResponses providers

- [`task-16-openai-chat.md`](task-16-openai-chat.md) — `OpenAIChatClient` using the private Chat Completions wire helpers
- [`task-17-openai-responses.md`](task-17-openai-responses.md) — `OpenAIResponsesClient` for `/v1/responses` (Codex tier) with `output[].type` switch

### Phase 6 — `WakeEntry.instructions` column + `DefaultWakeEntrySeed` constants + onboarding wiring

- [`task-18-wake-entry-instructions-migration.md`](task-18-wake-entry-instructions-migration.md) — Migration adds `instructions text NOT NULL DEFAULT ''`
- [`task-19-wake-entry-row-rust.md`](task-19-wake-entry-row-rust.md) — `WakeEntryRow.instructions` field
- [`task-20-default-wake-entry-seed-trait.md`](task-20-default-wake-entry-seed-trait.md) — `DefaultWakeEntrySeed` trait + flavor surface
- [`task-21-code-flavor-personality-constants.md`](task-21-code-flavor-personality-constants.md) — Engineer + Execution Worker constants in `flavors/code/src/personalities.rs`
- [`task-22-provisioning-wires-seeds.md`](task-22-provisioning-wires-seeds.md) — Provisioning path copies `instructions` into the wake_entries insert

### Phase 7 — Wake-trace schemas + `persist_wake_trace` atomic verb

- [`task-23-wake-trace-sidecar-migration.md`](task-23-wake-trace-sidecar-migration.md) — Sidecar tables for `wake_trace_v1`, `cited_wake_trace_jsonl_v1`, `citation_wake_trace_v1`
- [`task-24-wake-trace-payload-traits.md`](task-24-wake-trace-payload-traits.md) — Add `CitedObjectPayload` + `CitationMappingPayload` traits (mirror `FactPayload` pattern); register the three wake-trace schemas in the core flavor
- [`task-25-persist-wake-trace-core-types.md`](task-25-persist-wake-trace-core-types.md) — `WakeTracePersistInput` + `WakeTracePersistOutcome` typed surface (rationale: `EventIngest` can't be reused — stamps external/nil authorship, writes no sidecars, writes no edges)
- [`task-26-persist-wake-trace-storage-impl.md`](task-26-persist-wake-trace-storage-impl.md) — Postgres atomic verb writing 13 rows in one transaction (cited_objects + sidecar + event + memory + citation_mapping + sidecar + wake_trace_v1 + change_event + `core/authored` + N×`core/derived-from`)
- [`task-27-persist-wake-trace-integration-test.md`](task-27-persist-wake-trace-integration-test.md) — Postgres-backed test for atomicity + idempotent replay

### Phase 8 — THE CUT (one atomic commit)

> **Atomicity warning:** Tasks 28–37 land in one commit. Intermediate states do not compile. Work on a feature branch; verify with `cargo test --workspace` before committing.

- [`task-28-inference-target-config-rewrite.md`](task-28-inference-target-config-rewrite.md) — Rewrite `InferenceTargetConfig` to `MistralChat | OpenAIChat | OpenAIResponses`; delete `LocalCliConfig`/`RemoteModelConfig`
- [`task-29-inference-targets-data-migration.md`](task-29-inference-targets-data-migration.md) — One-shot data migration `20260512000030_inference_targets_rewrite.sql`; aborts on unmappable rows
- [`task-30-drop-recipe-ref-column.md`](task-30-drop-recipe-ref-column.md) — Drop `personality_wake_entries.recipe_ref`
- [`task-31-delete-goose-and-recipes.md`](task-31-delete-goose-and-recipes.md) — `git rm` Goose adapter + recipe modules + recipe YAML; `target_adapter/mod.rs` becomes alias shim
- [`task-32-fire-wake-entry-rewrite.md`](task-32-fire-wake-entry-rewrite.md) — Build `HarnessProgram` + `HarnessContext`, call `adapter.run`, emit wake-trace via `persist_wake_trace`
- [`task-33-harness-loop-in-binaries.md`](task-33-harness-loop-in-binaries.md) — Construct `HarnessLoop` in all four binaries
- [`task-34-shell-inference-target-record.md`](task-34-shell-inference-target-record.md) — Shell `InferenceTargetRecord` rewrite + TOML round-trip tests
- [`task-35-e2e-harness-wake-test.md`](task-35-e2e-harness-wake-test.md) — End-to-end test asserting `wake-trace-v1` Fact + JSONL CitedObject + `core/authored` edge + personality-attributed authorship + `core/derived-from` edge
- [`task-36-migrate-code-personalities-to-native-provider.md`](task-36-migrate-code-personalities-to-native-provider.md) — Migrate Code's two personalities to a native provider target; prefer MistralChat when `MISTRAL_API_KEY` exists
- [`task-37-atomic-cut-commit.md`](task-37-atomic-cut-commit.md) — Build, test, commit (verifies grep absence of `LocalCli`/`RemoteModel`/`write_effective_recipe`/`recipe_ref`/`engineer.yaml` etc.)

---

## File structure (created or modified across all phases)

**New files:**

- `crates/core/src/harness/mod.rs` — `HarnessAdapter` trait + value types
- `crates/core/src/harness/outcome.rs` — `HarnessOutcome`, `FinishReason`, `ErrorClass`, classifier
- `crates/core/src/verbs/persist_wake_trace.rs` — typed input/outcome
- `crates/core/src/wake/trace/mod.rs` — wake-trace payload structs (Fact + CitedObject + CitationMapping)
- `crates/core/src/wake/trace/emit.rs` — `HarnessOutcome → WakeTracePersistInput → engine.persist_wake_trace`
- `crates/core/src/personality/default_seeds.rs` — `DefaultWakeEntrySeed` + flavor trait
- `crates/harness/Cargo.toml`
- `crates/harness/src/lib.rs` — `HarnessLoop` concrete adapter + re-exports
- `crates/harness/src/program.rs` — `HarnessProgram` builder
- `crates/harness/src/conversation.rs` — `Conversation`, `Turn`, `ToolCall`, `ToolSpec`, `AssistantTurn`, `ToolResultTurn`
- `crates/harness/src/loop_driver.rs` — wake-loop driver (`loop` is a keyword)
- `crates/harness/src/tools/mod.rs` — `ToolBinding`, `ToolName`
- `crates/harness/src/tools/substrate_dispatch.rs` — in-process call into `HarnessSubstrateBridge`
- `crates/harness/src/tools/workspace/{mod.rs,shell.rs,text_editor.rs,list_files.rs}`
- `crates/harness/src/providers/{mod.rs,chat_completions_wire.rs,mistral_chat.rs,openai_chat.rs,openai_responses.rs}`
- `crates/harness/src/trace/{mod.rs,jsonl.rs}` — JSONL buffer with size cap
- `crates/harness/tests/fixtures/{mistral_chat,openai_chat,openai_responses}/*.json`
- `crates/storage-pg/src/verbs/persist_wake_trace.rs` — atomic verb impl
- `crates/storage-pg/migrations/20260512000010_wake_entry_instructions.sql`
- `crates/storage-pg/migrations/20260512000020_wake_trace_sidecars.sql`
- `crates/storage-pg/migrations/20260512000030_inference_targets_rewrite.sql`
- `crates/storage-pg/migrations/20260512000040_drop_wake_entry_recipe_ref.sql`
- `crates/storage-pg/tests/persist_wake_trace.rs`
- `crates/core/tests/{harness_outcome_classifier,persist_wake_trace_types,wake_trace_emission,inference_target_migration,flavor_registry}.rs`
- `crates/harness/tests/{mistral_chat_replay,openai_chat_replay,openai_responses_replay,workspace_shell,workspace_text_editor,workspace_list_files,substrate_dispatch,loop_driver,end_to_end_wake}.rs`
- `flavors/code/src/personalities.rs` — `EngineerSeed`, `ExecutionWorkerSeed` constants
- `flavors/code/instructions/{engineer,execution_worker}.txt`
- `flavors/code/tests/default_seeds.rs`

**Modified files:**

- `Cargo.toml` (workspace `members`)
- `crates/core/src/lib.rs` (re-export `harness` module + payload traits)
- `crates/core/src/payload.rs` (add `CitedObjectPayload` + `CitationMappingPayload`)
- `crates/core/src/flavor.rs` (add `add_cited_object_schema` + `add_citation_mapping_schema`; register three wake-trace schemas)
- `crates/core/src/inference/types.rs` (rewrite `InferenceTargetConfig`)
- `crates/core/src/inference/mod.rs` (drop `recipe_resolve`, `recipe_validate` from `pub mod`)
- `crates/core/src/personality/rows.rs` (`WakeEntryRow` — drop `recipe_ref`, add `instructions`)
- `crates/core/src/personality/mod.rs` (re-export `default_seeds`)
- `crates/core/src/verbs/mod.rs` (add `persist_wake_trace`)
- `crates/core/src/wake/target_adapter/mod.rs` (alias re-export of `HarnessAdapter` at the seam)
- `crates/core/src/wake/fire/fire.rs` (rewire to `HarnessAdapter`; remove `write_effective_recipe`; call `engine.persist_wake_trace`)
- `crates/core/src/wake/fire/mod.rs` (drop `pub mod recipe`)
- `crates/storage-pg/src/verbs/mod.rs` + `lib.rs` (export `persist_wake_trace`)
- `flavors/code/src/lib.rs` (call `register_default_seeds`)
- `apps/proxima-engine/src/main.rs` + `apps/proxima-shell/src-tauri/src/boot.rs` + `apps/proxima-code/src/main.rs` + `apps/proxima-mcp/src/main.rs` (construct `HarnessLoop`)
- `apps/proxima-shell/src-tauri/src/config/types.rs` (`InferenceTargetRecord` variants)

**Deleted files (in Phase 8 only):**

- `crates/core/src/wake/target_adapter/local_cli_goose.rs`
- `crates/core/tests/target_adapter_local_cli.rs`
- `crates/core/src/wake/fire/recipe.rs`
- `crates/core/src/inference/recipe_resolve.rs`
- `crates/core/src/inference/recipe_validate.rs`
- `flavors/code/recipes/engineer.yaml`
- `flavors/code/recipes/execution_worker.yaml`

---

## Self-review

**Spec coverage:** every numbered finding from the second-round spec review maps to one or more tasks above. The dedicated `persist_wake_trace` verb (tasks 25–27) is the load-bearing addition that fixes the previously-broken claim that `EventIngest` would write the `core/authored` edge.

**Reviewer blockers closed in task files:**

- Task 29 updates `inference_targets.kind`, rewrites `config`, and replaces `inference_targets_kind_chk`.
- Task 32 finalizes missing-credential wakes as `credentials_missing:{ENV}` and revokes wake tokens.
- Tasks 13/14 route substrate calls through `HarnessSubstrateBridge`, preserving `McpToolHost::call_tool` and `call_personality_tool`.
- Task 13 sets `caller_self_perspective` from the firing Root Perspective before MCP authoring tools run.
- Task 8 rejects existing symlink leaves and missing leaves under symlinked directories.
- Task 21 uses `proxima-code/code_emit_execution_request`.
- Task 6 preserves JSONL record boundaries before appending the truncation marker.
- Task 11 caps recursive listings at 500 entries and sets `truncated`.
- Task 32's `emit.rs` snippet uses crate-relative `crate::...` imports.

**Known fragilities for the implementing agent:**

- `HarnessSubstrateBridge` is defined in `proxima-core::mcp` and implemented for `McpToolHost` (task 13). `HarnessLoop::new` takes `(Arc<Engine>, Arc<dyn HarnessSubstrateBridge>)`; binary wiring in task 33 passes `Arc<McpToolHost>` cast to `Arc<dyn HarnessSubstrateBridge>`. **Do not** replace it with registry-only `McpToolDescriptor` dispatch: the bridge must preserve `McpToolHost::call_tool` and its `call_personality_tool` fallback for `core/fetch_memory`, `core/emit_perspective`, and the rest of `personality::substrate_pack()`.
- Wake-scoped substrate calls must populate `McpToolCtx.caller_self_perspective` from the firing personality's root perspective. Task 13 sets `McpAuthorContext.caller_self_perspective = Some(ctx.root_perspective_memory_id)` in harness dispatch and keeps a bridge fallback from `WakeTokenContext.current_root_perspective_memory_id`. Without this, authoring registry tools such as `proxima-code/code_emit_execution_request` fail with `caller_self_perspective is required...`.
- `persist_wake_trace` is wired through the `Storage` trait. `proxima-core` does not depend on `proxima-storage-pg`; `Engine::persist_wake_trace` calls `self.storage.persist_wake_trace_atomic(&self.registry, &input)` (task 26 step 1 & 2). `Engine` already holds `registry: FlavorRegistryFrozen` and `storage: StorageHandle` — no new fields.
- `core/derived-from` uses the dash form (`CORE_DERIVED_FROM_RELATION = "core/derived-from"` in `crates/core/src/relation.rs:26`), never the underscore. Same goes for `CORE_AUTHORED_RELATION = "core/authored"`. Use the constants; literal strings are a bug source.
- Goals are entities. `Storage::list_active_goals` returns `Vec<ActiveGoalSummary>` with `goal_id: GoalId`. The `persist_wake_trace` verb takes `active_goal_ids: Vec<GoalId>` and writes edges with `target_kind = "Goal"`, `target_goal_id = Some(_)`, `target_memory_id = None`. **Do not** model these as memories.
- `WorkspacePreparedRun` field names are `work_dir` and `workspace_context` (task 32). `runner.finalize(WorkspaceFinalizeInput { prepared, outcome, .. })` is **load-bearing** — it writes the workspace-mode primary memory + its provenance edges. The wake-trace Fact is a *separate* artifact via `engine.persist_wake_trace_internal`. Do not drop `runner.finalize`.
- After `start_wake_invocation`, no pre-run error may escape with `?` before token revocation and invocation finalization. Missing provider credentials are a failed wake outcome (`credentials_missing:{ENV}`), not a dispatch error. Task 35 includes a regression that checks finalized status, wake-token revocation, and failed wake-trace emission.
- `WakeEntryExecutionMode` variants are `SubstrateOnly | Workspace` (`crates/core/src/personality/types.rs:141`). `ModelTier` variants are `Fast | Standard | Deep` (`crates/core/src/models.rs:55`). The seeds in task 21 use these — there is no `Substrate` / `Strategic` / `Implementation`.
- Code flavor crate names: package `proxima-code`, lib `proxima_code`. `cargo build -p proxima-code` and `use proxima_code::personalities::...` (task 21).
- `FlavorRegistryFrozen` schema accessor is `list(&self) -> Vec<SchemaInfo>` (`crates/core/src/verbs/schema.rs:241`). There is no `schemas()` method — use `list()` in task 24's registration test. `resolve_relation(&self, relation: &str) -> Option<RegisteredRelation<'_>>` is correct as-named (used at `crates/core/src/wake/fire/fire.rs:432`).
- The `CitedObjectPayload` / `CitationMappingPayload` traits in task 24 must include `idempotency_key(&self) -> [u8; 32]` and `cited_object_schema() -> SchemaId` respectively — both are required by `docs/11-citations.md:51,63-67`. `WakeTraceJsonlPayload` carries an inline `content_hash: [u8; 32]` field and returns it from `idempotency_key()`; `WakeTraceCitationPayload::cited_object_schema()` returns the `proxima-core/wake-trace-jsonl-v1` SchemaId. Omitting either method weakens the citation contract.
- **Two-layer idempotency for `persist_wake_trace`** (do not conflate): (1) whole-verb replay keys on `WakeTracePersistInput::event_id()`, which folds in both `content_hash` *and* `invocation_id` — replay returns `idempotent_replay = true`; (2) `cited_objects` row dedup keys on `(owner, schema_id, content_hash)` via the `ON CONFLICT DO UPDATE … RETURNING cited_object_id` clause — distinct wakes producing byte-identical JSONL share the CitedObject row but get *separate* Facts and CitationMappings. Task 27's `distinct_invocations_with_identical_jsonl_do_not_collapse` test is the regression guard.
- Provider `tool_round` impls must NOT fall through to `RoundResult::Final` for unrecognized structural signals — task 5 returns `ProviderError::Deserialize` for Chat Completions `finish_reason` outside `{"stop","tool_calls","length"}`. Same rule for OpenAIChat and OpenAIResponses in tasks 16 and 17.
- `chat_completions_wire` is declared `mod chat_completions_wire;` (not `pub mod`) in `providers/mod.rs` — task 5 step 1. This is the mechanical enforcement of the no-public-compat boundary: Rust visibility makes the entire module unreachable from outside `proxima-harness`, regardless of `pub` markers inside. MistralChat and OpenAIChat access it via `super::chat_completions_wire`. Changing to `pub mod` is the single edit a reviewer must reject; "no public compat surface" is prose without it.
- MistralChat and OpenAIChat replay fixtures must each enumerate all seven `RoundResult` / `ProviderError` outcomes — tasks 7 and 16 explicitly list `{stop, tool_calls, length, auth_401, rate_limit_429, context_length_400, unsupported_finish}.json`. The `Files:` header in each task names all seven; if only 4–5 are recorded, two assertions in the test will hit missing fixtures.
- `InferenceTargetConfig` wire discriminants are stable: `mistral_chat`, `openai_chat`, `openai_responses`. Task 28 uses explicit serde renames for OpenAI variants; task 29's migration and tests must use those exact strings, not acronym-inferred `open_ai_*` or `open_a_i_*` forms.
- `proxima_core.inference_targets.kind` is a second storage discriminator, not decoration. Task 28 updates the Rust kind derivation; task 29 updates the column, rewrites `config`, and replaces `inference_targets_kind_chk`. Every row must satisfy `kind = config->>'kind'` after the cut.
- Default-personality provisioning module path — task 22 instructs the agent to grep for it rather than committing to a hard-coded path that may have moved.

---

## Execution

After committing each phase (1–7) or the atomic cut (phase 8), run `cargo test --workspace` before moving on. For the cut commit, follow the Phase 8 atomicity warning at the top of the index.
