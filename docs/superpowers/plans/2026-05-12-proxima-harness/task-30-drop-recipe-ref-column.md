# Task 8.3 — Drop `recipe_ref` column

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/storage-pg/migrations/20260512000040_drop_wake_entry_recipe_ref.sql`

- [ ] **Step 1: Write the migration**

```sql
ALTER TABLE proxima_core.personality_wake_entries
    DROP COLUMN recipe_ref;
```

Remove `recipe_ref: String` from `WakeEntryRow` in `crates/core/src/personality/rows.rs`. Remove from `WakeEntryDraft` too. Audit every construction site (`grep -rn "recipe_ref"` in the workspace) and delete the field — including in the SQL row mappers in `crates/storage-pg/src/`.

