//! Phase 1d Task 12: full pipeline. Real PG, real engine MCP listener, real
//! Goose subprocess, real recipe that emits an Abstraction.
//!
//! **Currently `#[ignore]`'d.** This test is a contract test for the runtime
//! spine — it documents what end-to-end success looks like once the
//! Phase 1d.5 follow-up lands the MCP→PersonalityTool dispatch bridge.
//! Today, when Goose calls `core/emit_abstraction` over MCP, the call
//! lands at the `McpToolDescriptor` path which does NOT thread through
//! `PersonalityToolContext.wake_invocation` — so memory provenance
//! stamping (Task 10) won't apply to Goose-driven writes until the bridge
//! exists.
//!
//! **Re-enable this test by removing `#[ignore]` once:**
//! 1. The MCP server's `tools/call` dispatch threads `WakeTokenContext`
//!    through to whatever path actually writes memories on behalf of
//!    Goose (likely via constructing a `PersonalityToolContext` with
//!    `with_wake_invocation(...)`).
//! 2. The substrate `core/emit_*` MCP descriptors delegate to the
//!    `PersonalityTool` impls (or share the same provenance-stamping
//!    write path).
//!
//! **Skipped at runtime if `goose` is not on PATH.**
//!
//! ## Contract this test will assert (once enabled)
//!
//! 1. Boot engine with a real `EngineHostedMcpListener` attached so
//!    `Engine::start` binds a loopback HTTP MCP server.
//! 2. Provision Owner + InferenceTarget + tier binding for `standard`.
//!    Recipe at `~/.proxima/recipes/<owner>/emit_one.yaml` that calls
//!    `core/emit_abstraction` once with a fixed `schema_id`.
//!    `WakeEntry` triggered by an external memory schema; substrate
//!    palette = `["core/emit_abstraction"]`.
//! 3. Start engine — spawns MCP listener + dispatcher tick loop.
//! 4. Append the triggering memory.
//! 5. Wait up to 60s for the wake to land an `Abstraction` authored by
//!    the personality.
//! 6. Assert provenance: `mem.author == fixture.instance_id` and
//!    `mem.model_id.is_some()` (stamped from `WakeTokenContext`).
//! 7. Assert invocation row: `status == "succeeded"`,
//!    `wake_token.is_some()`, `recipe_sha256.is_some()`,
//!    `resolved_inference_target_ref.is_some()`.

fn goose_on_path() -> bool {
    which::which("goose").is_ok()
}

#[tokio::test]
#[ignore = "requires Phase 1d.5 MCP→PersonalityTool bridge + configured Goose provider"]
async fn wake_pipeline_writes_memory_through_real_goose() {
    // See module-level docstring for what this test verifies and what's
    // needed to enable it. Body is intentionally a placeholder — the
    // contract is documented above so the test can be filled in once the
    // bridge lands.
    let _ = goose_on_path();
}
