# Task 6.4 — Code flavor `personalities.rs` constants

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `flavors/code/src/personalities.rs`
- Modify: `flavors/code/src/lib.rs`

- [ ] **Step 1: Migrate the two YAML bodies to Rust constants**

The instruction body in `flavors/code/recipes/execution_worker.yaml` lines 43–75 becomes a single `&'static str` constant. Same for `flavors/code/recipes/engineer.yaml` (read it first — it follows the same template). Verbatim transcription; do not summarise.

```rust
//! Default personalities shipped by the Code flavor.
//!
//! These constants replace the recipe YAML files (deleted in Phase 8).
//! On a fresh owner the provisioning path copies these into
//! `personality_wake_entries.instructions`.

use proxima_core::ModelTier;
use proxima_core::personality::{
    DefaultPersonalitySeed, DefaultWakeEntrySeed, TriggerKind, WakeEntryExecutionMode,
};

pub const ENGINEER_INSTRUCTIONS: &str = include_str!("../instructions/engineer.txt");
pub const EXECUTION_WORKER_INSTRUCTIONS: &str =
    include_str!("../instructions/execution_worker.txt");

pub const ENGINEER: DefaultPersonalitySeed = DefaultPersonalitySeed {
    display_name: "Code Engineer",
    purpose: "Reviews and orients on Code repo events; proposes execution requests.",
    system_prompt:
        "You are the Code engineer Personality inside Proxima. Read Reality, \
        decide what to do, and emit either an execution-request Fact or a no-op.",
    wake_entries: &[DefaultWakeEntrySeed {
        trigger_kind: TriggerKind::ChangeEventSchema,
        trigger_id: "proxima-code/commit-v1",
        label: "Engineer wake on commit",
        // Real variants live in crates/core/src/personality/types.rs:141:
        //     pub enum WakeEntryExecutionMode { SubstrateOnly, Workspace }
        execution_mode: WakeEntryExecutionMode::SubstrateOnly,
        substrate_tool_palette: &[
            "core/emit_abstraction",
            "core/emit_perspective",
            // Must match `CodeEmitExecutionRequestTool::NAME` in
            // `flavors/code/src/mcp/emit_execution_request.rs:73`. The
            // harness palette resolver compares strings — a mismatch
            // silently drops the tool from the Engineer's palette.
            "proxima-code/code_emit_execution_request",
        ],
        workspace_tool_palette: &[],
        max_rounds: 8,
        // Real variants live in crates/core/src/models.rs:55:
        //     pub enum ModelTier { Fast, Standard, Deep }
        // Engineer wants the strongest model — pick Deep.
        model_tier: ModelTier::Deep,
        probability_promille: 1000,
        instructions: ENGINEER_INSTRUCTIONS,
    }],
};

pub const EXECUTION_WORKER: DefaultPersonalitySeed = DefaultPersonalitySeed {
    display_name: "Code Execution Worker",
    purpose: "Implements an execution-request Fact inside a prepared worktree.",
    system_prompt:
        "You are an unattended software engineer inside a prepared Proxima \
        worktree. Optimize for completing one concrete change.",
    wake_entries: &[DefaultWakeEntrySeed {
        trigger_kind: TriggerKind::ChangeEventSchema,
        trigger_id: "proxima-code/execution-request-v1",
        label: "Execution worker wake",
        execution_mode: WakeEntryExecutionMode::Workspace,
        substrate_tool_palette: &[],
        workspace_tool_palette: &[
            "workspace_shell",
            "workspace_text_editor",
            "workspace_list_files",
        ],
        max_rounds: 30,
        // Standard tier is the right balance for the implementer — fast
        // enough for the 30-round budget, capable enough for code edits.
        model_tier: ModelTier::Standard,
        probability_promille: 1000,
        instructions: EXECUTION_WORKER_INSTRUCTIONS,
    }],
};

pub const ALL: &[DefaultPersonalitySeed] = &[ENGINEER, EXECUTION_WORKER];
```

Create `flavors/code/instructions/engineer.txt` — paste the `instructions:` body from `flavors/code/recipes/engineer.yaml` verbatim (preserve line endings, indentation, paragraph breaks). Do **not** wrap it in YAML pipes or quotes.

Create `flavors/code/instructions/execution_worker.txt` — paste lines 43–75 of `flavors/code/recipes/execution_worker.yaml` verbatim. (Lines starting with two spaces in the YAML body are *not* indented in the .txt — the YAML's `instructions: |` block-scalar marker stripped the two leading spaces. Match what `goose run` saw at the prompt: dedented.)

- [ ] **Step 2: Wire into `flavors/code/src/lib.rs`**

Add `pub mod personalities;` near the existing module declarations. Export `personalities::ALL` so the onboarding path can iterate it (Task 6.5).

- [ ] **Step 3: Verify constants compile and `include_str!` resolves**

Run: `cargo build -p proxima-code`
Expected: clean. (Crate name is verified at `flavors/code/Cargo.toml:1` — `name = "proxima-code"`, `lib.name = "proxima_code"`.)

- [ ] **Step 4: Add a smoke test**

Create `flavors/code/tests/default_seeds.rs`:

```rust
use proxima_code::mcp::CodeEmitExecutionRequestTool;
use proxima_code::personalities::{ALL, ENGINEER, EXECUTION_WORKER, ENGINEER_INSTRUCTIONS, EXECUTION_WORKER_INSTRUCTIONS};
use proxima_core::mcp::McpTool;

#[test]
fn instructions_are_non_empty() {
    assert!(!ENGINEER_INSTRUCTIONS.is_empty());
    assert!(!EXECUTION_WORKER_INSTRUCTIONS.is_empty());
}

#[test]
fn execution_worker_instructions_contain_phase_order_marker() {
    // Sanity that the YAML→txt migration preserved the phase numbering.
    assert!(EXECUTION_WORKER_INSTRUCTIONS.contains("phase order"));
}

#[test]
fn each_seed_has_at_least_one_wake_entry() {
    for s in ALL {
        assert!(!s.wake_entries.is_empty(), "{} missing wake entries", s.display_name);
    }
}

#[test]
fn engineer_wake_entry_triggers_on_commit_schema() {
    assert_eq!(ENGINEER.wake_entries[0].trigger_id, "proxima-code/commit-v1");
}

/// Regression: the Engineer's palette string must match the actual
/// registered MCP tool name. Hardcoded strings drift; cross-check
/// against the trait's associated const so a rename fires this test.
#[test]
fn engineer_palette_references_real_execution_request_tool() {
    let palette = ENGINEER.wake_entries[0].substrate_tool_palette;
    assert!(
        palette.contains(&<CodeEmitExecutionRequestTool as McpTool>::NAME),
        "Engineer palette {palette:?} missing {}",
        <CodeEmitExecutionRequestTool as McpTool>::NAME
    );
}
```

Verify the `McpTool` trait path before running — adjust the `use` if the trait lives at a different module path in `proxima-core` (search: `grep -rn "pub trait McpTool" crates/core/src`).

Run: `cargo test -p proxima-code --test default_seeds`
Expected: 5 tests pass.

- [ ] **Step 5: Commit**

```bash
git add flavors/code/src/personalities.rs flavors/code/instructions flavors/code/src/lib.rs flavors/code/tests/default_seeds.rs
git commit -m "code(flavor): DefaultPersonalitySeed constants replacing recipe YAML bodies"
```

