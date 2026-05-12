# Task 4.4 — Substrate dispatch test

> Part of [Proxima Harness Implementation Plan](README.md). Subagent execution: implement steps in order, commit at the end of the task.

**Files:**
- Create: `crates/harness/tests/substrate_dispatch.rs`

This test ensures the reverse-map and palette enforcement behave correctly. Because direct dispatch needs a wired `Engine` + `McpToolHost` bridge, defer the full e2e to Phase 8 and instead exercise the program builder's name maps here. Task 4.2 adds a separate bridge inventory regression for substrate-pack tools.

- [ ] **Step 1: Write the test**

```rust
use proxima_core::harness::{HarnessProgram, ProviderTarget, SubstrateToolBinding};
use proxima_harness::program::resolve;
use serde_json::json;

fn binding(canonical: &str) -> SubstrateToolBinding {
    SubstrateToolBinding {
        canonical_name: canonical.into(),
        description: "stub".into(),
        args_schema: json!({"type":"object"}),
    }
}

fn empty_program(bindings: Vec<SubstrateToolBinding>, workspace: bool) -> HarnessProgram {
    HarnessProgram {
        system_prompt: "sys".into(),
        instructions: "do".into(),
        context_params: Default::default(),
        substrate_tool_palette: bindings.iter().map(|b| b.canonical_name.clone()).collect(),
        workspace_root: workspace.then(|| std::path::PathBuf::from("/tmp/x")),
        max_rounds: 5,
        provider: ProviderTarget::MistralChat {
            base_url: "http://x".into(),
            model_id: "m".into(),
            api_key: "k".into(),
            temperature: None,
            max_completion_tokens: None,
        },
    }
}

#[test]
fn provider_safe_names_reverse_map_back_to_canonical() {
    let p = empty_program(vec![binding("core/emit_abstraction")], false);
    let bindings = vec![binding("core/emit_abstraction")];
    let r = resolve(p, bindings);
    let safe = r.tools.iter().find(|t| t.canonical == "core/emit_abstraction").unwrap();
    assert_eq!(safe.provider_safe, "core_emit_abstraction");
    assert_eq!(
        r.reverse_map.get("core_emit_abstraction").unwrap(),
        "core/emit_abstraction"
    );
}

#[test]
fn workspace_tools_appear_only_when_workspace_root_is_set() {
    let p_no_ws = empty_program(vec![], false);
    let r_no = resolve(p_no_ws, vec![]);
    assert!(!r_no.tools.iter().any(|t| t.canonical.starts_with("workspace_")));

    let p_ws = empty_program(vec![], true);
    let r_ws = resolve(p_ws, vec![]);
    let names: Vec<&str> = r_ws.tools.iter().map(|t| t.canonical.as_str()).collect();
    assert!(names.contains(&"workspace_shell"));
    assert!(names.contains(&"workspace_text_editor"));
    assert!(names.contains(&"workspace_list_files"));
}
```

Run: `cargo test -p proxima-harness --test substrate_dispatch`
Expected: both tests pass.

- [ ] **Step 2: Commit**

```bash
git add crates/harness/tests/substrate_dispatch.rs
git commit -m "harness: program builder name-map + workspace-only-when-rooted tests"
```
