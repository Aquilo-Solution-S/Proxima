# Task 6.5 — Provisioning path wires seeds into `instructions`

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: the owner-default provisioning code path — *located during implementation*. Run `grep -rn "personality_wake_entries\|create_default_personality\|default_personalities\|seed_default" crates/ apps/ flavors/ --include="*.rs"` to find it. Likely candidates: `crates/core/src/personality/`, `apps/proxima-shell/src-tauri/src/boot.rs`, or the existing path that today reads recipe YAML paths into the `recipe_ref` column.

- [ ] **Step 1: Locate the path**

Find the function that today inserts the default Engineer + Execution Worker rows. The `recipe_ref` column is non-NULL today, so the function necessarily references a recipe path or slug. Look for `"engineer"`, `"execution_worker"`, or `recipe_ref:` in literals.

- [ ] **Step 2: Add `instructions` to the insert**

The function currently passes `recipe_ref = "bundled:proxima-code/engineer"` (or similar). Add `instructions = personalities::ENGINEER.wake_entries[0].instructions.to_string()` to the same insert. Both columns coexist until Phase 8 drops `recipe_ref`.

- [ ] **Step 3: Integration test (or augment existing)**

Find the existing default-personality provisioning test (likely in `crates/core/tests/` or `apps/proxima-shell/src-tauri/tests/`). Add an assertion that the inserted row's `instructions` column is non-empty:

```rust
let row = sqlx::query!(
    "SELECT instructions FROM proxima_core.personality_wake_entries \
     WHERE label = $1",
    "Engineer wake on commit",
)
.fetch_one(&pool)
.await
.unwrap();
assert!(!row.instructions.is_empty(), "default Engineer seed should populate instructions");
```

Run the test. Expected: passes.

- [ ] **Step 4: Commit**

```bash
git add <touched files>
git commit -m "core(onboarding): provisioning copies DefaultWakeEntrySeed.instructions into wake_entries"
```
