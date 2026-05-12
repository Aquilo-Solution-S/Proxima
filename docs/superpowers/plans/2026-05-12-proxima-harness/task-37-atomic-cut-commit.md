# Task 8.10 — Build, test, commit

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

- [ ] **Step 1: Full workspace build**

Run: `cargo build --workspace`
Expected: clean (warnings denied; this MUST compile with zero warnings).

- [ ] **Step 2: Full workspace test**

Run: `cargo test --workspace`
Expected: all tests pass, including:
- `harness_outcome_classifier` (14 tests)
- `mistral_chat_replay`, `openai_chat_replay`, `openai_responses_replay`
- `workspace_shell`, `workspace_text_editor`, `workspace_list_files`
- `jsonl_buffer`, `substrate_dispatch`, `loop_driver`
- `default_seeds` (code flavor)
- `inference_target_migration`
- the e2e wake test
- every pre-existing test in the workspace

- [ ] **Step 3: Verify no stale references**

```bash
! grep -rn "LocalCliGooseAdapter\|LocalCli\b\|RemoteModel\b\|write_effective_recipe\|recipe_ref\|recipe_resolve\|recipe_validate\|GOOSE_PROFILE\|engineer\.yaml\|execution_worker\.yaml" --include="*.rs" --include="*.sql" --include="*.toml" --include="*.md" crates/ apps/ flavors/ \
    | grep -v "docs/superpowers/specs/" \
    | grep -v "docs/superpowers/plans/" \
    | grep -v "/.git/"
```

Expected: empty output. Any hit other than spec/plan files needs to be cleaned up in the same commit.

- [ ] **Step 4: Single atomic commit**

```bash
git add -A
git status   # eyeball the file list; should match Tasks 8.1–8.9 exactly
git commit -m "$(cat <<'EOF'
harness(cut): replace Goose subprocess with in-process Proxima Harness

Atomic greenfield cut, per spec
docs/superpowers/specs/2026-05-12-proxima-harness-design.md:

- Rewrite InferenceTargetConfig to MistralChat | OpenAIChat | OpenAIResponses
  (LocalCli + RemoteModel variants dropped).
- One-shot data migration translates existing inference_targets rows
  by (vendor, dialect); unmappable rows abort the migration.
- Wire HarnessLoop into fire_wake_entry; delete write_effective_recipe
  call and recipe-rewrite middle layer.
- Emit wake-trace-v1 Fact + wake-trace-jsonl-v1 CitedObject +
  wake-trace-citation-v1 CitationMapping after every wake.
- Drop personality_wake_entries.recipe_ref column.
- Delete crates/core/src/wake/target_adapter/local_cli_goose.rs,
  crates/core/src/wake/fire/recipe.rs,
  crates/core/src/inference/recipe_resolve.rs,
  crates/core/src/inference/recipe_validate.rs,
  flavors/code/recipes/engineer.yaml,
  flavors/code/recipes/execution_worker.yaml.
- Construct HarnessLoop in every binary (engine, shell, code, mcp).
- End-to-end test: Engineer wake → MistralChat mock → wake-trace Fact
  persisted with non-empty JSONL CitedObject.
- Migrate Code's two default personalities to a native MistralChat or
  OpenAIChat target; provisioning errors loudly if no API key env var
  is set.

This is the single commit Heinrich approved as the greenfield cut —
no transition window, no deprecation lane, no coexistence variants.
EOF
)"
```

---

## Self-Review Notes

**Spec coverage:**

| Spec section | Plan task(s) |
|---|---|
| Six principles | All — woven through Phases 1–8 |
| Crate layout (core defines trait, harness depends on core) | Task 1.1, 2.1 |
| Core traits (HarnessAdapter, ProviderClient, Conversation, RoundResult) | Tasks 1.1, 2.2, 2.3 |
| Outcome classification table | Task 1.1 + 1.2 (exhaustive tests) |
| InferenceTargetConfig migration | Tasks 8.1 + 8.2 |
| Workspace tools (shell, text_editor, list_files) | Tasks 3.1–3.4 |
| Substrate/flavor in-process dispatch + reverse-map | Tasks 4.1 + 4.2 + 4.4 |
| Recipe lifecycle: kill the YAML | Tasks 6.1–6.5 + 8.4 |
| Provisioning defaults (DefaultWakeEntrySeed) | Tasks 6.3–6.5 |
| Three observability layers | Layer 1 in Task 2.4 + 4.3 (JSONL); Layer 2 in 4.3 (`wake_invocation_log` rows already written by existing code, harness adds `harness_round` phase rows — Task 4.3 sketch covers it); Layer 3 in Tasks 7.1 + 7.2 + 8.5 |
| Changes in fire_wake_entry | Task 8.5 |
| Provider scope (MistralChat + OpenAIChat + OpenAIResponses) | Phases 2 + 5 |
| Single-cut Goose removal | Phase 8 |
| What stays valuable | Tasks 6.1 (WakeEntry shape preserved), 4.2 (McpToolHost bridge preserves registry MCP + substrate-pack tools), e2e test (WorkspaceRunner.prepare still called) |

**Type consistency:** `HarnessAdapter` / `HarnessProgram` / `HarnessOutcome` / `HarnessContext` / `ProviderTarget` / `SubstrateToolBinding` names are stable across Tasks 1.1, 4.1, 4.3, 8.5. `HarnessSubstrateBridge` is the Task 4.2 dispatch seam; do not replace it with registry-only `McpToolDescriptor` dispatch. `WorkspaceToolName::{Shell,TextEditor,ListFiles}` are stable across Tasks 3.1, 4.1, 4.3. `ToolSpec { canonical, provider_safe, description, input_schema }` consistent in Tasks 2.2, 4.1, 4.4.

**Placeholder scan:** No "TBD" / "implement later" markers. Each task either includes the code or points at the exact spec section to mirror.

**Known fragilities (called out in tasks):**

- `CitationMappingPayload::cited_object_schema()` return type in Task 7.2 — verify against `crates/core/src/payload.rs`.
- Provisioning module path in Tasks 6.5 + 8.9 — located during implementation.
- `FlavorRegistryFrozen` iterator names (`fact_schemas`, `cited_object_schemas`, `citation_mapping_schemas`) in Task 7.2 — verify before writing the assertion.
- serde-rename of `MistralChat`/`OpenAIChat`/`OpenAIResponses` in Task 8.2 — the test in Task 8.2 verifies the actual string; SQL migration must match.
