# Task 8.6 — Construct `HarnessLoop` in every binary

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `apps/proxima-engine/src/main.rs`
- Modify: `apps/proxima-shell/src-tauri/src/boot.rs`
- Modify: `apps/proxima-code/src/main.rs`
- Modify: `apps/proxima-mcp/src/main.rs`

- [ ] **Step 1: Find each adapter construction site**

`grep -rn "LocalCliGooseAdapter::new\|target_adapter" apps/` shows where Goose is wired today. Replace each.

`HarnessLoop::new` takes two args: `Arc<Engine>` and `Arc<dyn proxima_core::mcp::HarnessSubstrateBridge>`. The bridge is implemented by `McpToolHost` (Task 4.2). Every binary that boots `Engine` also constructs a `McpToolHost` (search for `McpToolHost::from_pool` or `McpToolHost::from_database_url` to find the construction site — it lives next to `Engine::start`).

```rust
let mcp_tool_host = std::sync::Arc::new(mcp_tool_host); // already wired today; ensure it's Arc-owned
let adapter = std::sync::Arc::new(
    proxima_harness::HarnessLoop::new(
        engine.clone(),
        mcp_tool_host.clone() as std::sync::Arc<dyn proxima_core::mcp::HarnessSubstrateBridge>,
    ),
);
```

If a binary today owns `McpToolHost` by value (not `Arc`), wrap the existing instance in `Arc::new(...)` at the boot site and update other references — the `Clone` impl on `McpToolHost` already takes `&self` so existing uses keep compiling.

`Engine::set_target_adapter(adapter)` or the equivalent setter — match the existing call shape. The adapter trait alias from Task 8.4 keeps the type name working.

Add `proxima-harness = { path = "../../crates/harness" }` to each binary's `Cargo.toml`.

- [ ] **Step 2: Build all four binaries**

Run: `cargo build -p proxima-engine -p proxima-shell -p proxima-code -p proxima-mcp`
Expected: clean.

- [ ] **Step 3: Commit**

```bash
git add apps/proxima-engine apps/proxima-shell apps/proxima-code apps/proxima-mcp
git commit -m "apps: wire HarnessLoop into every binary with McpToolHost bridge"
```
