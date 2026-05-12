# Task 6.3 — User-authored `WakeEntry.instructions` surfaces

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/core/src/mcp/core_tools/{wake_entry_input.rs,get_personality.rs,list_wake_entries.rs,update_wake_entry.rs}`
- Modify: `apps/proxima-shell/src-tauri/src/commands/engine.rs`
- Modify: `packages/frontend-core/src/bindings.ts` via Specta regeneration
- Modify: `packages/frontend-core/src/views/personalities/{types.ts,index.tsx,inspector.tsx}` and tests

- [ ] **Step 1: MCP surfaces**

Add `instructions: String` to `WakeEntryDraftInput` with `#[serde(default)]`.
Map it into `WakeEntryDraft`. Add `instructions` to `core/get_personality`
and `core/list_wake_entries` outputs. Add `instructions: Option<String>` to
`UpdateWakeEntryPatch` and apply it when present.

- [ ] **Step 2: Shell command types**

Add `instructions: String` to `WakeEntryTs` and `WakeEntryDraftTs`.
Map row → TS and TS → `WakeEntryDraft`.

Regenerate bindings:

```bash
cargo test -p proxima-shell export_ts_bindings
```

- [ ] **Step 3: Personalities UI**

Add `instructions: ""` to `emptyDraft`, preserve it in `entryToDraft`, and add
a textarea in the wake-entry Behavior section. Keep the existing recipe picker
until Phase 8 because Goose still consumes `recipe_ref`.

- [ ] **Step 4: Tests**

Update existing personality tests and add one regression that edits the
instructions textarea and asserts `setWakeEntries(...).entries[0].instructions`
contains the edited text.

Run:

```bash
pnpm --filter @proxima/core test -- personalities
pnpm --filter @proxima/core typecheck
```

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/mcp/core_tools apps/proxima-shell/src-tauri/src/commands/engine.rs packages/frontend-core/src
git commit -m "frontend-core(personalities): edit wake entry instructions"
```
