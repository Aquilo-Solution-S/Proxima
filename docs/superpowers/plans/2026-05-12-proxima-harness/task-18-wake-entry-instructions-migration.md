# Task 6.1 — Migration: add `instructions` column

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/storage-pg/migrations/20260512000010_wake_entry_instructions.sql`

- [ ] **Step 1: Write the migration**

```sql
-- Spec: docs/superpowers/specs/2026-05-12-proxima-harness-design.md
--       §"Recipe lifecycle: kill the YAML".
--
-- Adds the per-trigger instruction body that today's recipe YAML's
-- `instructions:` field carries. The Goose path ignores this column;
-- the harness path (Phase 8) reads it as the user-seed prefix.
ALTER TABLE proxima_core.personality_wake_entries
    ADD COLUMN IF NOT EXISTS instructions text NOT NULL DEFAULT '';
```

- [ ] **Step 2: Apply migrations locally**

Run: `cargo test -p proxima-storage-pg --test migrations` (or whatever the existing migration test is — check `crates/storage-pg/tests/`).
Expected: migration applies cleanly. Find the existing migration smoke test and confirm it still passes.

- [ ] **Step 3: Commit**

```bash
git add crates/storage-pg/migrations/20260512000010_wake_entry_instructions.sql
git commit -m "storage(wake_entries): add instructions column"
```

