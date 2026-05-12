# Task 8.4 — Delete recipe rewriter, recipe resolve/validate, recipe YAML

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files (delete):**
- `crates/core/src/wake/fire/recipe.rs`
- `crates/core/src/inference/recipe_resolve.rs`
- `crates/core/src/inference/recipe_validate.rs`
- `flavors/code/recipes/engineer.yaml`
- `flavors/code/recipes/execution_worker.yaml`
- `crates/core/src/wake/target_adapter/local_cli_goose.rs`
- `crates/core/tests/target_adapter_local_cli.rs`

**Files (modify):**
- `crates/core/src/wake/fire/mod.rs` — drop `pub mod recipe;`
- `crates/core/src/wake/target_adapter/mod.rs` — drop `pub mod local_cli_goose;`, re-export `HarnessAdapter`/`HarnessProgram`/`HarnessOutcome`/`HarnessContext` for the seam name compat, then plan to delete the file entirely.

- [ ] **Step 1: Run `git rm` on the deletes**

```bash
git rm crates/core/src/wake/fire/recipe.rs \
       crates/core/src/inference/recipe_resolve.rs \
       crates/core/src/inference/recipe_validate.rs \
       flavors/code/recipes/engineer.yaml \
       flavors/code/recipes/execution_worker.yaml \
       crates/core/src/wake/target_adapter/local_cli_goose.rs \
       crates/core/tests/target_adapter_local_cli.rs
```

- [ ] **Step 2: Remove `target_adapter` module re-exports**

Simplest path: keep `crates/core/src/wake/target_adapter/mod.rs` as a thin shim that re-exports the harness seam from `proxima_core::harness::*`, then in a *follow-up* delete the module entirely. For now, replace `mod.rs` contents with:

```rust
//! Wake target-adapter seam.
//!
//! v1: `TargetAdapter` is replaced by `proxima_core::harness::HarnessAdapter`.
//! This module re-exports the new types under the old path so a small
//! follow-up commit can rename call sites then delete the module.

pub use crate::harness::{
    HarnessAdapter as TargetAdapter,
    HarnessContext as TargetContext,
    HarnessError as TargetAdapterError,
    HarnessOutcome as TargetOutcome,
    HarnessOutcomeKind as TargetOutcomeKind,
    HarnessProgram as TargetInvocation,
};
```

The aliases preserve `fire_wake_entry`'s `&dyn TargetAdapter` parameter name for the rewrite below.

