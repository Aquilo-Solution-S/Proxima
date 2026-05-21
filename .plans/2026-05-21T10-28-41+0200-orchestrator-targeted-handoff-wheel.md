# Orchestrator Targeted Handoff Wheel

Status: Proposed
Created: 2026-05-21
Reviewed:
Implemented:
Implementation:
Verification:
- pending
Notes:
- Supersedes demo-wheel shapes that hardcode `Visionary -> Planner -> Worker -> Verifier`.
- Existing dirty worktree changes must be reconciled, not blindly reverted.

REQUIRED SUB-SKILL: skill-driven-implementation

## Summary

Current failure:
- Demo-wheel topology is encoded in wake config and prompt fixtures.
- Schema triggers wake roles too early, especially verifier-on-every-workspace-run.
- Tests become proof of a scripted shape, not proof of the Spinning Wheel.

Target:
- `GoalActivated -> Orchestrator`.
- Orchestrator reads current state, visible personalities, prior strategy memories, and chooses one next action.
- Non-initial wakes are target-routed by explicit target metadata, not schema broadcast.
- Worker, Verifier, Visionary, Planner emit results back to Orchestrator.
- Orchestrator decides continue, delegate, verify, ask user, blocked, or achieved.

Architecture:
- Add core routing metadata table `proxima_core.memory_handoff_targets`.
- Do not represent handoff as a graph edge: `Fact -> Perspective` would be an upward F/A/P edge and `core/inspires` already has Goal/Self semantics.
- Dispatcher keeps schema wake entries, but if a triggering memory has a target personality, only that personality's matching wake entry is eligible.
- Add wake-visible `core/list_handoff_candidates`.
- Add typed `proxima-core/orchestrator-decision-v1` Fact plus `core/emit_orchestrator_decision`.
- Extend code-flavor request/review tools to optionally target the next personality through the core routing metadata.

## Current Repo Facts

- `docs/08-core-and-flavors.md` says personality instances are runtime substrate rows, and wake entries choose trigger schema, tool palettes, instructions, and execution mode.
- `docs/14-protocol-surface.md` keeps personality lifecycle and wake-entry config outside the six graph verbs.
- `docs/06-goals-and-self.md` defines goal-reactive wakes through `goal_scope = trigger_goal_assigned`.
- `crates/core/src/wake/dispatch.rs` currently scans all active wake entries and matches only by trigger schema/relation plus author filter.
- `flavors/code/src/mcp/emit_execution_request.rs` already has `target_personality` validation for retry requests and checks that the target has an execution wake.
- `flavors/code/src/mcp/emit_execution_request.rs` already has plan item kinds `implementation` and `test`.

## Goals

- [ ] Add explicit target routing for emitted Facts without violating F/A/P layering.
- [ ] Add Orchestrator decision memory and tool.
- [ ] Add handoff candidate discovery visible in wake context/tools.
- [ ] Let Orchestrator choose next personality and action.
- [ ] Convert the demo experiment to start only with `Goal -> Orchestrator`.
- [ ] Prove every non-initial wake in the experiment was target-routed.
- [ ] Prove no verifier wakes by accidental workspace-run schema broadcast.
- [ ] Prove no intervention requests in the successful roundtrip.
- [ ] Persist one strategy memory after success/failure.

## Non-Goals

- No runtime schema/tool/source registration path.
- No new external protocol verb.
- No new flavor inclusion mechanism.
- No Self row or cached Self entity.
- No generic untyped JSON escape hatch for A/P payloads.
- No hardcoded exact role sequence in metrics.
- No live-provider matrix beyond one ignored demo-wheel run.

## Decisions

- Routing store: `proxima_core.memory_handoff_targets`, not an edge.
  Rationale: target routing is dispatcher metadata, not cognitive provenance.

- Target resolution: target by `PersonalityInstanceId`, exposed as `I...` wake handle.
  Rationale: class-aware handle model is already model-visible.

- Dispatcher behavior: target present restricts eligibility; no target falls back to existing schema matching.
  Rationale: preserves current surfaces and lets targeted routing land incrementally.

- Orchestrator is a runtime personality, not a core personality type.
  Rationale: docs keep personality behavior in wake entries and prompts, not flavor-owned runtime traits.

- Strategy memory starts as `proxima-intent/strategy-memory-v1` Abstraction emitted through the existing typed abstraction path.
  Rationale: strategy is learned interpretation, not a raw external observation.

## File Structure

- Create `crates/storage-pg/migrations/20260521000060_core_memory_handoff_targets.sql`
- Modify `crates/core/src/storage.rs`
- Modify `crates/storage-pg/src/lib.rs`
- Modify `crates/storage-pg/src/verbs/consolidate/mod.rs`
- Create `crates/storage-pg/src/verbs/consolidate/handoff_targets.rs`
- Modify `crates/core/src/wake/dispatch.rs`
- Modify `crates/core/src/mcp/core_tools/mod.rs`
- Create `crates/core/src/mcp/core_tools/handoff_candidates.rs`
- Create `crates/core/src/mcp/core_tools/orchestrator_decision.rs`
- Modify `crates/core/src/lib.rs`
- Modify `crates/core/src/orchestrator_decision.rs`
- Modify `flavors/code/src/mcp/emit_execution_request.rs`
- Modify `flavors/code/src/mcp/workspace_review/types.rs`
- Modify `flavors/code/src/mcp/workspace_review/tools.rs`
- Modify `flavors/code/src/workspace_runner/ingest.rs`
- Modify `experiments/demo-wheel/tests/demo_wheel/{mod.rs,prompts.rs,setup.rs,world.rs,world_metrics.rs,outputs.rs}`
- Add tests under `crates/core/tests`, `flavors/code/tests`, and `experiments/demo-wheel/tests`

## Tasks

### Task 1: Add Append-Only Target Routing Metadata

Files:
- Create: `crates/storage-pg/migrations/20260521000060_core_memory_handoff_targets.sql`
- Create: `crates/storage-pg/src/verbs/consolidate/handoff_targets.rs`
- Modify: `crates/storage-pg/src/verbs/consolidate/mod.rs`
- Modify: `crates/storage-pg/src/lib.rs`
- Modify: `crates/core/src/storage.rs`

Migration SQL:

```sql
CREATE TABLE proxima_core.memory_handoff_targets (
    memory_id uuid PRIMARY KEY,
    target_personality_instance_id uuid NOT NULL,
    handoff_reason text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    owner_principal_kind proxima_core.owner_principal_kind NOT NULL,
    owner_principal_id uuid NOT NULL,
    owner_org_id uuid NOT NULL,
    CHECK (length(btrim(handoff_reason)) BETWEEN 1 AND 2000)
);

CREATE INDEX memory_handoff_targets_owner_target_idx
    ON proxima_core.memory_handoff_targets (
        owner_principal_kind,
        owner_principal_id,
        target_personality_instance_id,
        created_at
    );
```

Implementation shape:

```rust
pub struct MemoryHandoffTarget {
    pub memory_id: MemoryId,
    pub target_personality_instance_id: PersonalityInstanceId,
    pub handoff_reason: String,
}

async fn insert_memory_handoff_target(
    tx: &mut Transaction<'_, Postgres>,
    owner: &Owner,
    target: &MemoryHandoffTarget,
) -> Result<(), StorageError>;

async fn memory_handoff_target(
    pool: &PgPool,
    owner: &Owner,
    memory_id: MemoryId,
) -> Result<Option<PersonalityInstanceId>, StorageError>;
```

Test:

```rust
#[sqlx::test(migrations = "../storage-pg/migrations")]
async fn memory_handoff_target_is_owner_scoped_and_idempotent(pool: PgPool) {
    // arrange owner, memory_id, target personality
    // insert target once
    // insert same target again with same memory_id
    // assert one row and same target returned for same owner
    // assert different owner cannot read it
}
```

Run:
- `cargo test -p proxima-storage-pg handoff_target -- --nocapture`

Expected:
- before implementation: unresolved symbols or missing table.
- after implementation: target row is owner-scoped, append-time metadata is stable.

Commit:
- `feat(core): add memory handoff target metadata`

### Task 2: Filter Dispatcher By Explicit Target

Files:
- Modify: `crates/core/src/wake/dispatch.rs`
- Modify: `crates/core/src/storage.rs`
- Test: `crates/core/tests/targeted_wake_dispatch_pg.rs`

Implementation shape:

```rust
fn target_allows_group(
    target: Option<PersonalityInstanceId>,
    group_personality: PersonalityInstanceId,
) -> bool {
    target.is_none_or(|target| target == group_personality)
}
```

Dispatcher algorithm:
- For each `EntityAppend` memory event, resolve `memory_handoff_target(owner, memory_id)`.
- If target exists and `group.personality_instance_id != target`, skip all wake entries for that event.
- If target exists and group matches, apply existing `triggers_match`, `authored_by_matches`, dependency checks, and goal scope.
- Edge events keep current behavior unless a later task adds targetable edge outputs.

Test skeleton:

```rust
#[sqlx::test]
async fn targeted_memory_wakes_only_target_personality() {
    // create two active personalities with same on_memory wake entry for schema S
    // ingest one Fact S
    // insert memory_handoff_targets(memory_id, target = alice)
    // run dispatch_tick
    // assert Alice invocation count == 1
    // assert Bob invocation count == 0
}

#[sqlx::test]
async fn untargeted_memory_keeps_schema_broadcast_behavior() {
    // same setup without memory_handoff_targets row
    // assert both matching personalities wake
}
```

Run:
- `cargo test -p proxima-core targeted_memory_wakes -- --nocapture`
- `cargo test -p proxima-core untargeted_memory_keeps_schema_broadcast_behavior -- --nocapture`

Expected:
- target row gates wake eligibility.
- existing untargeted schema wake behavior remains.

Commit:
- `feat(core): route targeted memory wakes`

### Task 3: Add Handoff Candidate Discovery Tool

Files:
- Create: `crates/core/src/mcp/core_tools/handoff_candidates.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`
- Modify: `crates/core/src/lib.rs` if exports are needed
- Test: `crates/core/tests/handoff_candidates_tool_pg.rs`

Tool:

```rust
pub struct ListHandoffCandidatesArgs {
    pub trigger_schema_id: Option<String>,
    pub execution_mode: Option<WakeExecutionMode>,
}

pub struct HandoffCandidate {
    pub personality: String,       // I...
    pub display_name: String,
    pub purpose: String,
    pub root_perspective: String,  // P...
    pub wake_entries: Vec<HandoffCandidateWakeEntry>,
}

pub struct HandoffCandidateWakeEntry {
    pub wake_entry: String,        // W...
    pub trigger_kind: String,
    pub trigger_id: String,
    pub execution_mode: WakeExecutionMode,
    pub substrate_tool_palette: Vec<String>,
    pub workspace_tool_palette: Vec<String>,
}
```

Filtering:
- same owner only.
- active personalities only.
- tombstoned/disabled wake entries excluded.
- optional trigger and execution filters.
- respect read-scope only for richer detail if needed; do not hide active personality identity from same owner in this first slice.

Test skeleton:

```rust
#[sqlx::test]
async fn list_handoff_candidates_returns_active_personalities_with_matching_wakes() {
    // instantiate Planner, Worker, Verifier
    // set wakes for execution-request and test-request
    // call core/list_handoff_candidates with trigger_schema_id execution-request
    // assert Worker appears and Verifier does not
    // assert handles use I/P/W prefixes
}
```

Run:
- `cargo test -p proxima-core handoff_candidates -- --nocapture`
- `cargo test -p proxima-core handoff_candidates_tool_schema_describes_handles -- --nocapture`

Expected:
- wake-visible schema describes `I...`, `P...`, `W...` handle classes.

Commit:
- `feat(core): expose handoff candidates to wakes`

### Task 4: Add Orchestrator Decision Fact And Tool

Files:
- Create: `crates/core/src/orchestrator_decision.rs`
- Create: `crates/storage-pg/migrations/20260521000070_core_orchestrator_decision.sql`
- Modify: `crates/core/src/lib.rs`
- Modify: `crates/core/src/mcp/core_tools/mod.rs`
- Create: `crates/core/src/mcp/core_tools/orchestrator_decision.rs`
- Test: `crates/core/tests/orchestrator_decision_tool_pg.rs`

Payload:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorDecisionV1 {
    pub goal_id: uuid::Uuid,
    pub triggering_memory_id: Option<uuid::Uuid>,
    pub decision: OrchestratorDecisionKind,
    pub target_personality_id: Option<uuid::Uuid>,
    pub target_schema_id: Option<String>,
    pub rationale: String,
    pub expected_result: Option<String>,
    pub idempotency_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorDecisionKind {
    Delegate,
    Verify,
    AskUser,
    MarkAchieved,
    StopBlocked,
    RecordStrategy,
}
```

Tool:

```rust
core/emit_orchestrator_decision({
  "goal": "G1",
  "triggering_memory": "F1",
  "decision": "delegate",
  "target_personality": "I3",
  "target_schema_id": "proxima-code/execution-request-v1",
  "rationale": "...",
  "expected_result": "...",
  "idempotency_key": "..."
})
```

Behavior:
- writes `proxima-core/orchestrator-decision-v1` Fact.
- if `target_personality` is present, inserts `memory_handoff_targets` for the decision Fact.
- does not directly fire a wake.
- dispatcher sees the committed decision Fact next tick.

Test skeleton:

```rust
#[sqlx::test]
async fn emit_orchestrator_decision_writes_fact_and_target_metadata() {
    // create orchestrator + worker
    // call tool with target_personality I(worker)
    // assert Fact schema id
    // assert sidecar row
    // assert memory_handoff_targets row points at worker
}
```

Run:
- `cargo test -p proxima-core orchestrator_decision -- --nocapture`

Expected:
- tool is recorded-only; no wake invocation happens inside the tool call.

Commit:
- `feat(core): add orchestrator decision facts`

### Task 5: Add Target Fields To Code Handoff Tools

Files:
- Modify: `flavors/code/src/mcp/emit_execution_request.rs`
- Modify: `flavors/code/src/mcp/workspace_review/types.rs`
- Modify: `flavors/code/src/mcp/workspace_review/tools.rs`
- Modify: `flavors/code/src/workspace_runner/ingest.rs`
- Modify: `flavors/code/src/workspace_runner/loaders.rs`
- Test: `flavors/code/tests/targeted_handoff_pg.rs`

API changes:

```rust
pub struct CodeEmitExecutionRequestArgs {
    // existing fields...
    pub target_personality: Option<String>, // I... Worker
    pub return_to_personality: Option<String>, // I... Orchestrator
    pub handoff_reason: Option<String>,
}

pub struct ExecutionPlanItemArgs {
    // existing fields...
    pub target_personality: Option<String>,
    pub return_to_personality: Option<String>,
    pub handoff_reason: Option<String>,
}

pub struct CodeEmitVerificationEvidenceArgs {
    // existing fields...
    pub target_personality: Option<String>, // I... Orchestrator
    pub handoff_reason: Option<String>,
}

pub struct CodeEmitWorkspaceReviewArgs {
    // existing fields...
    pub target_personality: Option<String>, // I... Orchestrator
    pub handoff_reason: Option<String>,
}
```

Validation:
- `target_personality` must be active.
- for execution requests, target must have enabled workspace wake for `proxima-code/execution-request-v1`.
- for test requests, target must have enabled workspace wake for `proxima-code/test-request-v1`.
- for evidence/review result routing, target must have enabled substrate wake for the emitted result schema unless omitted.

Workspace run propagation:
- When a Worker handles a targeted execution request with `return_to_personality`, the emitted `proxima-core/workspace-run-v1` gets a handoff target back to that personality.
- Reason: Worker cannot set the target on the harness-emitted workspace run manually.

Test skeleton:

```rust
#[sqlx::test]
async fn execution_request_target_routes_worker_and_workspace_run_returns_to_orchestrator() {
    // Orchestrator emits execution request target=Worker return_to=Orchestrator
    // assert execution request memory_handoff_targets -> Worker
    // fire Worker wake and persist workspace run
    // assert workspace run memory_handoff_targets -> Orchestrator
}

#[sqlx::test]
async fn workspace_review_target_routes_back_to_orchestrator() {
    // Verifier emits approved workspace review target=Orchestrator
    // assert memory_handoff_targets for review -> Orchestrator
}
```

Run:
- `cargo test -p proxima-code targeted_handoff -- --nocapture`
- `cargo test -p proxima-code --test payload_contracts -- --nocapture`

Expected:
- existing non-targeted tool calls still work.
- targeted tool calls produce target metadata.

Commit:
- `feat(flavors-code): target code handoff facts`

### Task 6: Add Orchestrator Demo Experiment

Files:
- Modify: `experiments/demo-wheel/tests/demo_wheel/mod.rs`
- Modify: `experiments/demo-wheel/tests/demo_wheel/setup.rs`
- Modify: `experiments/demo-wheel/tests/demo_wheel/world.rs`
- Modify: `experiments/demo-wheel/tests/demo_wheel/prompts.rs`
- Modify: `experiments/demo-wheel/tests/demo_wheel/world_metrics.rs`
- Modify: `experiments/demo-wheel/tests/demo_wheel/outputs.rs`

Experiment mode:

```rust
enum DemoPlannerMode {
    Scripted,
    Real,
    VisionDocument,
    Orchestrated,
}
```

Wake setup:
- Initial assignment: Goal assigned to Orchestrator only.
- Orchestrator wake entries:
  - `proxima-goal/goal-activated-v1`
  - `proxima-intent/vision-brief-v1`
  - `proxima-code/execution-request-v1` only if Orchestrator wants to inspect emitted requests
  - `proxima-core/workspace-run-v1`
  - `proxima-code/verification-evidence-v1`
  - `proxima-code/workspace-review-v1`
  - `proxima-core/orchestrator-decision-v1`
- Visionary/Planner/Worker/Verifier wake entries remain capability declarations only; they should not receive untargeted events in the orchestrated test except the initial goal path if explicitly targeted.

Orchestrator tools:
- `core/fetch_memory`
- `core/search_memories`
- `core/list_active_goals`
- `core/list_handoff_candidates`
- `core/emit_orchestrator_decision`
- `core/emit_abstraction` for `proxima-intent/strategy-memory-v1`
- `proxima-code/code_emit_execution_request`
- `proxima-code/code_emit_execution_plan`
- `proxima-goal/goal_mark_achieved`

Prompt contract:
- Inspect current state.
- Choose exactly one next decision.
- If delegating, use a target returned by `core/list_handoff_candidates`.
- Do not do Worker work or Verifier work directly.
- Mark achieved only from an approved workspace review plus passing evidence.
- Record a strategy memory before terminal close.

Test skeleton:

```rust
#[ignore]
#[tokio::test]
async fn orchestrated_signal_match_spinning_wheel() {
    let result = demo_wheel::run_orchestrated_signal_match_from_env().await;
    assert!(result.metrics.overall_pass);
    assert_eq!(result.metrics.intervention_request_count, 0);
    assert!(result.metrics.orchestrator_decision_count >= 3);
    assert!(result.metrics.targeted_wake_ratio == 1.0);
    assert_eq!(result.metrics.accidental_verifier_workspace_run_wakes, 0);
    assert!(result.metrics.strategy_memory_count >= 1);
}
```

Run:
- `cargo test -p proxima-demo-wheel --test demo_wheel_pg --no-run`
- live only after focused tests pass:
  `PROXIMA_LIVE_MISTRAL=1 cargo test -p proxima-demo-wheel --test demo_wheel_pg orchestrated_signal_match_spinning_wheel -- --ignored --nocapture --test-threads=1`

Expected:
- no fixed exact sequence assertion.
- every non-initial wake has a target row.
- verifier appears only after Orchestrator-targeted verification request or review request.

Commit:
- `feat(demo-wheel): add orchestrated handoff experiment`

### Task 7: Strategy Memory

Files:
- Modify: `flavors/intent/src/lib.rs`
- Create: `flavors/intent/src/payloads/strategy_memory.rs`
- Modify: `flavors/intent/src/payloads/mod.rs`
- Add migration: `flavors/intent/migrations/20260521000080_strategy_memory.sql`
- Modify: `experiments/demo-wheel/tests/demo_wheel/setup.rs`
- Modify: `experiments/demo-wheel/tests/demo_wheel/prompts.rs`
- Modify: demo Orchestrator tools to allow strategy emit.

Payload:

```rust
pub struct StrategyMemoryV1 {
    pub goal_id: uuid::Uuid,
    pub goal_class: String,
    pub strategy_used: Vec<String>,
    pub worked: bool,
    pub failure_modes: Vec<String>,
    pub next_time_hint: String,
    pub evidence: Vec<uuid::Uuid>,
}
```

Test skeleton:

```rust
#[sqlx::test]
async fn orchestrator_strategy_memory_links_to_goal_and_evidence() {
    // emit strategy memory from orchestrator
    // assert typed sidecar row
    // assert derived-from edges to evidence facts only
    // assert future search_memories can retrieve by goal class text
}
```

Run:
- `cargo test -p proxima-flavor-intent strategy_memory -- --nocapture`
- `cargo test -p proxima-demo-wheel --test demo_wheel_pg --no-run`

Commit:
- `feat(flavors-intent): add strategy memory payload`

## Metrics For Orchestrated Demo

Add to `Metrics`:

```rust
orchestrator_decision_count: u32,
targeted_wake_count: u32,
untargeted_non_initial_wake_count: u32,
targeted_wake_ratio: f64,
accidental_verifier_workspace_run_wakes: u32,
strategy_memory_count: u32,
final_closer_role: String,
```

Pass conditions:
- `overall_pass == true`
- `final_goal_state == Achieved`
- `intervention_request_count == 0`
- `correction_loop_count == 0`
- `untargeted_non_initial_wake_count == 0`
- `accidental_verifier_workspace_run_wakes == 0`
- `orchestrator_decision_count >= 3`
- `strategy_memory_count >= 1`
- `final_closer_role == "Orchestrator"`

## Docs Alignment

- `docs/02-memory.md`: no Fact-to-Perspective handoff edge; routing metadata is not cognitive provenance.
- `docs/06-goals-and-self.md`: initial goal assignment still uses Goal -> Self-Perspective inspiration and `goal_scope`.
- `docs/08-core-and-flavors.md`: Orchestrator is runtime personality config plus wake entries, not a flavor-owned personality class.
- `docs/14-protocol-surface.md`: no new external graph verb; personality lifecycle remains operational config.
- `docs/12-tool-manifest.md`: new tools are build-time registered core tools, not runtime tool registration.

## Verification Sequence

Focused:
- `cargo test -p proxima-storage-pg handoff_target -- --nocapture`
- `cargo test -p proxima-core targeted_memory_wakes -- --nocapture`
- `cargo test -p proxima-core handoff_candidates -- --nocapture`
- `cargo test -p proxima-core orchestrator_decision -- --nocapture`
- `cargo test -p proxima-code targeted_handoff -- --nocapture`
- `cargo test -p proxima-demo-wheel --test demo_wheel_pg --no-run`

Workspace:
- `cargo check --workspace`
- `cargo clippy --workspace --all-targets -- -D warnings`

Live:
- `PROXIMA_LIVE_MISTRAL=1 cargo test -p proxima-demo-wheel --test demo_wheel_pg orchestrated_signal_match_spinning_wheel -- --ignored --nocapture --test-threads=1`

## Review Checks

- Strategy memory lives in `flavors/intent` as an Abstraction.
- Targeted routing applies to memory events only in v1.
- Handoff candidates expose active same-owner personality names, purposes, and wake entries; richer memory context still obeys read-scope.

## Out Of Scope

- Multi-owner or group-shared orchestration semantics.
- UI for Orchestrator configuration.
- Automatic long-term strategy retrieval beyond the demo prompt/tool path.
- Deleting or rewriting existing demo-wheel plan history.
- Replacing existing scripted demo modes.
