# Task 6.3 — `DefaultWakeEntrySeed` trait + flavor surface

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/core/src/personality/default_seeds.rs`
- Modify: `crates/core/src/personality/mod.rs` (`pub mod default_seeds;`)

- [ ] **Step 1: Implement**

```rust
//! Build-time source of the default `instructions:` body each flavor
//! ships for its bundled personalities. Replaces the recipe YAML
//! that today lives in `flavors/*/recipes/`.

use crate::ModelTier;
use crate::personality::WakeEntryExecutionMode;

#[derive(Debug, Clone)]
pub struct DefaultWakeEntrySeed {
    pub trigger_kind: TriggerKind,
    pub trigger_id: &'static str,
    pub label: &'static str,
    pub execution_mode: WakeEntryExecutionMode,
    pub substrate_tool_palette: &'static [&'static str],
    pub workspace_tool_palette: &'static [&'static str],
    pub max_rounds: u16,
    pub model_tier: ModelTier,
    pub probability_promille: u16,
    pub instructions: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub enum TriggerKind {
    ChangeEventSchema,
    GoalKind,
    SelfPerspectivePulse,
}

#[derive(Debug, Clone)]
pub struct DefaultPersonalitySeed {
    pub display_name: &'static str,
    pub purpose: &'static str,
    pub system_prompt: &'static str,
    pub wake_entries: &'static [DefaultWakeEntrySeed],
}
```

- [ ] **Step 2: Re-export from `personality/mod.rs`**

```rust
pub mod default_seeds;
pub use default_seeds::{DefaultPersonalitySeed, DefaultWakeEntrySeed, TriggerKind};
```

- [ ] **Step 3: Build**

Run: `cargo build -p proxima-core`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/personality
git commit -m "core(personality): DefaultWakeEntrySeed surface for flavor-shipped instructions"
```

