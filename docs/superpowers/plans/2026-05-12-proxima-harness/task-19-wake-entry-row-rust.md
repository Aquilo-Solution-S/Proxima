# Task 6.2 — `WakeEntry.instructions` core/storage round-trip

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/core/src/personality/drafts.rs`
- Modify: `crates/core/src/personality/rows.rs`
- Modify: `crates/core/src/wake/dispatch.rs`
- Modify: `crates/storage-pg/src/verbs/consolidate/{wake_entries.rs,instances.rs}`

- [ ] **Step 1: Add the field**

In `WakeEntryDraft` and `WakeEntryRow`, add `instructions: String` after `recipe_ref`:

```rust
pub struct WakeEntryRow {
    pub wake_entry_id: Uuid,
    pub trigger_kind: WakeEntryTriggerKind,
    pub trigger_id: String,
    pub label: String,
    pub enabled: bool,
    pub execution_mode: WakeEntryExecutionMode,
    pub authored_by: WakeEntryAuthoredBy,
    pub probability_promille: u16,
    pub goal_scope: WakeEntryGoalScope,
    pub recipe_ref: String,
    pub instructions: String,
    pub model_tier: crate::ModelTier,
    pub inference_target_ref: Option<String>,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
    pub max_rounds: u16,
    pub disabled_reason: Option<String>,
}
```

Keep `WakeEntryDraft::new(...)`'s signature stable and initialise
`instructions: String::new()`. Use `String::new()` as the default for
any test fixture / builder that materialises a row.

- [ ] **Step 2: Update storage SQL**

Find the `SELECT` / `INSERT` / `UPSERT` paths for
`personality_wake_entries` in `crates/storage-pg/src/`. Add
`instructions` to the column lists, row structs, row mappers, and update
sets. Ensure `set_wake_entries_within` preserves carried instructions
because it reads current entries before applying the mutation.

Run: `cargo check -p proxima-core -p proxima-storage-pg`
Expected: green.

- [ ] **Step 3: Update test fixtures**

`rg -n "WakeEntryDraft \\{|WakeEntryRow \\{" crates/ flavors/ apps/ -g "*.rs"` to find every construction site. Add `instructions: String::new()` or a meaningful value where the test asserts round-trip behavior.

Run: `cargo build --workspace`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/personality crates/core/src/wake/dispatch.rs crates/storage-pg/src
git commit -m "core(wake_entries): add user-authored instructions field"
```
