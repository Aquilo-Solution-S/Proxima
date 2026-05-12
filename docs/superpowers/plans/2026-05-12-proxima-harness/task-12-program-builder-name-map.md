# Task 4.1 — `HarnessProgram` builder + name-map helper

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Modify: `crates/harness/src/program.rs`

- [ ] **Step 1: Implement**

```rust
//! HarnessProgram → Conversation + tools list, plus the
//! canonical ↔ provider-safe name map the loop driver uses to
//! reverse-resolve `function.name` from the provider.

use std::collections::HashMap;

use proxima_core::harness::{HarnessProgram, SubstrateToolBinding};
use proxima_core::mcp::provider_safe_tool_name;

use crate::conversation::{Conversation, ToolSpec};
use crate::tools::{ToolBinding, workspace::WorkspaceToolName};

#[derive(Debug)]
pub struct ResolvedProgram {
    pub conversation: Conversation,
    pub tools: Vec<ToolSpec>,
    /// provider-safe name → canonical name. Lookup direction the
    /// loop driver uses when reading `function.name` back from the
    /// provider response.
    pub reverse_map: HashMap<String, String>,
    /// canonical name → binding. Lookup direction the dispatch path
    /// uses after reverse-resolving the name.
    pub bindings: HashMap<String, ToolBinding>,
}

#[must_use]
pub fn resolve(program: HarnessProgram, substrate_tools: Vec<SubstrateToolBinding>) -> ResolvedProgram {
    let user_seed = build_user_seed(&program);
    let mut tools = Vec::with_capacity(substrate_tools.len() + 3);
    let mut reverse_map = HashMap::new();
    let mut bindings = HashMap::new();

    for s in &substrate_tools {
        let provider_safe = provider_safe_tool_name(&s.canonical_name);
        tools.push(ToolSpec {
            canonical: s.canonical_name.clone(),
            provider_safe: provider_safe.clone(),
            description: s.description.clone(),
            input_schema: s.args_schema.clone(),
        });
        reverse_map.insert(provider_safe, s.canonical_name.clone());
        bindings.insert(
            s.canonical_name.clone(),
            ToolBinding::Substrate(s.clone()),
        );
    }

    if program.workspace_root.is_some() {
        for name in [
            WorkspaceToolName::Shell,
            WorkspaceToolName::TextEditor,
            WorkspaceToolName::ListFiles,
        ] {
            let canonical = name.canonical().to_string();
            let provider_safe = provider_safe_tool_name(&canonical);
            tools.push(ToolSpec {
                canonical: canonical.clone(),
                provider_safe: provider_safe.clone(),
                description: workspace_description(name).into(),
                input_schema: workspace_args_schema(name),
            });
            reverse_map.insert(provider_safe, canonical.clone());
            bindings.insert(canonical, ToolBinding::Workspace(name));
        }
    }

    ResolvedProgram {
        conversation: Conversation {
            system_prompt: program.system_prompt,
            user_seed,
            turns: vec![],
        },
        tools,
        reverse_map,
        bindings,
    }
}

fn build_user_seed(program: &HarnessProgram) -> String {
    let mut s = String::new();
    if !program.instructions.is_empty() {
        s.push_str(&program.instructions);
        s.push_str("\n\n");
    }
    for key in [
        "root_perspective",
        "active_goals",
        "trigger_event",
        "triggering_memory",
        "workspace_context",
    ] {
        if let Some(v) = program.context_params.get(key) {
            s.push_str(&format!(
                "{}:\n{}\n\n",
                snake_to_title(key),
                serde_json::to_string_pretty(v).unwrap_or_default()
            ));
        }
    }
    s.trim_end().to_string()
}

fn snake_to_title(s: &str) -> String {
    s.split('_')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(first) => first.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn workspace_description(name: WorkspaceToolName) -> &'static str {
    match name {
        WorkspaceToolName::Shell => "Run a shell command in the prepared worktree.",
        WorkspaceToolName::TextEditor => {
            "Create or edit files in the prepared worktree (view | create | str_replace | insert)."
        }
        WorkspaceToolName::ListFiles => "List files under a path in the prepared worktree.",
    }
}

fn workspace_args_schema(name: WorkspaceToolName) -> serde_json::Value {
    match name {
        WorkspaceToolName::Shell => crate::tools::workspace::shell::args_schema(),
        WorkspaceToolName::TextEditor => crate::tools::workspace::text_editor::args_schema(),
        WorkspaceToolName::ListFiles => crate::tools::workspace::list_files::args_schema(),
    }
}

// Re-export to satisfy `pub mod program;` even though this module's
// public API is `resolve` + `ResolvedProgram`.
#[allow(unused_imports)]
use SubstrateToolBinding as _;
```

- [ ] **Step 2: Verify build**

Run: `cargo build -p proxima-harness`
Expected: builds clean.

- [ ] **Step 3: Commit**

```bash
git add crates/harness/src/program.rs
git commit -m "harness: program builder with canonical/provider-safe name maps"
```
