# Task 6.2 — `WakeEntryRow` Rust shape

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/core/src/personality/rows.rs`

- [ ] **Step 1: Add the field**

In `WakeEntryRow` (around line 47–64), add `instructions: String` after `recipe_ref`:

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

If `WakeEntryDraft` (likely nearby in the same file) carries the same fields, add `instructions: String` there as well. Use `String::new()` as the default for any test fixture / builder that materialises a row.

- [ ] **Step 2: Update storage SQL**

Find the `SELECT` / `INSERT` for `personality_wake_entries` in `crates/storage-pg/src/`. Add `instructions` to both the column list and the `RETURNING`/`SELECT` shape. The query macro will fail at `cargo check` if the row no longer matches.

Run: `cargo check -p proxima-storage-pg`
Expected: green.

- [ ] **Step 3: Update test fixtures**

`grep -rn "WakeEntryRow {" crates/ flavors/ apps/ --include="*.rs"` to find every construction site. Add `instructions: String::new()` (or `instructions: String::from("…")` where the test cares about the value).

Run: `cargo build --workspace`
Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/personality/rows.rs crates/storage-pg/src
git commit -m "core(wake_entries): add instructions field; storage round-trips it"
```

