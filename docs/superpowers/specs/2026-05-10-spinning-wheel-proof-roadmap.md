# Spinning Wheel — v0.1.0 Closed-Loop Proof Roadmap

**Status:** Roadmap, 2026-05-10. Sequencing document for the v0.1.0
end-to-end demo. Composes existing specs; introduces no new architecture.

**Related:**
- `2026-05-10-personality-vocabulary-and-archetype-discipline.md`
  (canonical vocabulary; chains-of-personalities = composition pattern,
  not a fixed engine architecture)
- `2026-05-07-personality-as-composed-behaviors.md` (wake/decide/write
  loop, Goose owns the LLM loop, ContextBuilder + recipe_ref +
  tool_palette)
- `2026-05-09-workspace-mode-design.md` (Workspace runner, worktree
  lifecycle, decision UI, run+decision Facts)
- `2026-05-10-personality-mcp-crud-design.md` (handle layer, audit
  Fact, scopes — used by personalities that mutate themselves)
- `2026-05-10-personality-topology-canvas-design.md` (canvas for editing
  the chain visually)
- `docs/02-memory.md` (four pillars F/A/P/G; directionality rule)

## Goal

Define the smallest closed loop of personalities, memories, and human
gates that proves Proxima can autonomously develop code from a
conversation-seeded goal. The loop must close end-to-end before
v0.1.0 merges to `main`.

This is a **composition** of pieces that are individually specced
elsewhere — not a redesign. The contribution here is the topology, the
v0.1.0 acceptance test, and the milestone sequence that lets us land
each link without breaking the chain.

## v0.1.0 Acceptance Test

The demo script that gates the merge to `main`:

1. Heinrich and the assistant discuss a small concrete change in the
   working repo (e.g. "rename the `unwrap` helper in `tauri-client.ts`
   to `unwrapTauri` and update callers").
2. The assistant calls `proxima-goal/goal_propose` via MCP, attaching
   the conversation Perspective(s) as motivating evidence. A Goal row
   lands with `state = Proposed`.
3. Heinrich reviews the proposal in the UI and calls
   `proxima-goal/goal_accept`. The Goal transitions to `Active`. A
   `proxima-goal/goal-activated-v1` Fact lands (substrate gap — see
   §Required substrate work).
4. The configured planning personality assigned by `core/inspires`
   wakes on `proxima-goal/goal-activated-v1` and emits one or more
   `proxima-code/execution-request-v1` Facts, one per target repo.
5. The configured workspace worker wakes on each execution-request
   Fact, runs Goose in workspace mode, and emits
   `proxima-code/workspace-run-v1` with a non-empty diff.
6. Heinrich opens the Workspace Runs panel, reviews the diff, clicks
   `Merge → road-to-v1`. The branch fast-forwards. A
   `proxima-code/workspace-decision-v1{decision: merged}` Fact lands.

Pass = each step succeeds with the typed memory writes intact and
visible in Atlas/Surface, fully audited, no hand-holding between
steps. Fail = any link requires manual intervention or invented
substrate that isn't already specced.

## Non-Goals

- **Open-ended autonomy.** v0.1.0 has two human gates (goal accept,
  workspace decision) and they stay. Removing either is a post-v0.1.0
  conversation.
- **Goal-side repo scope.** Planner handoff is a Code Fact. Goals stay
  core entities; repo targeting belongs to
  `proxima-code/execution-request-v1`.
- **Self-mutation in the loop.** The personality CRUD via MCP
  (`2026-05-10-personality-mcp-crud-design.md`) is the substrate that
  makes self-mutation *possible*, but no personality in this roadmap
  rewrites its own WakeConfig during the demo. Self-evolution rides on
  this loop in a later spec.
- **Required personality instances.** Planner and worker are
  user-configured personality instances. Code ships bundled Goose
  recipes; it does not ship required personality instances. No engine
  archetype enums.
- **A "Reviewer" personality in v0.1.0.** Tempting to add Abstraction →
  Perspective → Goal as three personalities, but the loop closes with
  fewer hops. M5 (post-v0.1.0) adds the reviewer if and when we want
  the chain to spin without conversation seeding.
- **Cross-personality messaging beyond memory writes.** Communication
  is by typed memory + wake filters. No side channels, no pub/sub.

## The closed loop (v0.1.0 topology)

```
                     ┌── Heinrich ⇄ assistant conversation ──┐
                     │  (Claude Code or any MCP-speaking LLM) │
                     └────────────────┬──────────────────────┘
                                      │ goal_propose (MCP)
                                      ▼
                          ┌────────────────────────┐
                          │ Goal{state=Proposed}   │
                          └────────────┬───────────┘
                                       │
                              [HUMAN GATE — goal_accept]
                                       │
                                       ▼
                          ┌────────────────────────┐
                          │ Goal{state=Active}     │
                          │ + goal-activated-v1 F  │
                          └────────────┬───────────┘
                                       │ wake filter:
                                       │  {schema_id: proxima-goal/goal-activated-v1}
                                       ▼
                  ┌────────────────────────────────────┐
                  │ configured planning personality    │
                  │   recipe = plan_execution_requests │
                  │   emits Code execution requests    │
                  └────────────────┬───────────────────┘
                                   │ execution-request-v1 F
                                   ▼
                  ┌────────────────────────────────────┐
                  │ configured workspace worker         │
                  │   Workspace mode, recipe = worker   │
                  │   palette = developer__*            │
                  └────────────────┬───────────────────┘
                                   │ goose run in worktree
                                   ▼
                  ┌────────────────────────────────────┐
                  │ workspace-run-v1 F (diff, head_sha)│
                  └────────────────┬───────────────────┘
                                   │
                          [HUMAN GATE — accept / merge / reject]
                                   │
                                   ▼
                  ┌────────────────────────────────────┐
                  │ workspace-decision-v1 F            │
                  │ + (on merge) commit on target_branch│
                  └────────────────┬───────────────────┘
                                   │ next git ingest tick
                                   ▼
                       configured Engineer-like personality
                          (commit → commit-summary-v1 A
                           → development-perspective-v1 P)
                                   │
                                   ▼
                       (chain idle until next conversation)
```

The loop is **seeded by conversation** and **drained by merge**.
Between gates, every hop is a typed memory write that the next hop
filters on. Audit by construction across the whole loop.

## Configured participants

Planner, worker, and Engineer are configured personality instances.
Labels are user vocabulary; `PersonalityInstanceId` is the only engine
identity. Code ships bundled recipes only.

| display_name | Wake filter | Tool palette | Output | Status |
|---|---|---|---|---|
| Engineer | `OnMemory{schema_id: proxima-code/commit-summary-v1}` | substrate read + `core/emit_perspective` | `development-perspective-v1` Perspective | **Already running.** Substrate-only mode. The upstream `commit-summary-v1` Abstraction is produced from raw commits by Code's git ingest path (separate concern). |
| (assistant, external) | n/a — runs as MCP client, not a personality | `proxima-goal/goal_propose` | `Goal{state=Proposed}` | **Already running.** Heinrich's Claude Code session is the v0.1.0 stand-in. Becomes a personality post-v0.1.0. |
| Planner | `OnMemory{schema_id: proxima-goal/goal-activated-v1}` | Code graph/search tools + `proxima-code/code_emit_execution_request` | `execution-request-v1` Fact(s) | **M4.** Configured by user; no default instance. |
| Worker | `OnMemory{schema_id: proxima-code/execution-request-v1}` | Workspace mode; `developer__text_editor`, `developer__shell`, `developer__list_files` | `workspace-run-v1` Fact (via WorkspaceRunnerSource) | **M5.** Configured by user; no default instance. |
| (Heinrich) | n/a — human-in-the-loop | `goal_accept` MCP + Workspace Runs panel | `Goal{state=Active}` + `goal-activated-v1` F + `workspace-decision-v1` F | **Partially implemented.** `goal_accept` works; goal-activated Fact emit + decision Fact land in M2 / M3. |

The "(assistant, external)" row is deliberate: in v0.1.0 the goal
proposer is not yet a personality — it is whoever Heinrich is
conversing with via MCP (Claude Code, the Shell, future flavors). The
loop closes without an in-engine proposer. M6 adds an
in-engine Goal Proposer personality that wakes on Perspectives written
by Engineer and proposes Goals autonomously, removing the conversation
seed.

## Human gates

### Gate 1 — Goal acceptance

- **Surface:** `proxima-goal/goal_accept` MCP tool (already
  implemented). v0.1.0 leans on the MCP path — Heinrich calls
  `goal_accept` from his Claude Code session referencing the proposal
  handle returned by `goal_propose`. A dedicated proposed-goals review
  panel in the Shell is desirable but not required for the acceptance
  test; it can land alongside or after M4.
- **Effect:** new Goal row with `state = Active`, supersedes the
  proposal. Today: nothing else. After M2: also emits a typed
  `proxima-goal/goal-activated-v1` Fact so wake triggers can fire.
- **Why a gate:** prevents a runaway proposer from triggering
  workspace runs the user didn't sanction. Cheap to upgrade later
  (auto-accept rules, trusted proposer scopes).

### Gate 2 — Workspace decision

- **Surface:** Workspace Runs panel (Code flavor frontend). Three
  buttons: `Reject`, `Accept`, `Merge → <target_branch>`. Already
  specced in `2026-05-09-workspace-mode-design.md` §Decision flow.
- **Effect:** emits `workspace-decision-v1` Fact + on-disk side
  effects (worktree remove for reject/merge, no-op for accept; git
  fast-forward merge for merge).
- **Why a gate:** workspace runs may be wrong, disruptive, or
  unwanted. Merging is when changes leave the disposable worktree and
  hit the working repo's branch. v0.1.0 keeps this firmly under
  human control.

## Required substrate work (gaps blocking v0.1.0)

These are calls on adjacent specs the roadmap depends on. None
require new architecture — they're closing concrete gaps.

### S0 — Per-master-token shell-author identity

Today: `Storage::ensure_shell_author_personality(owner)` mints **one**
shell-author personality per owner (singleton), lazily on the first
personality-CRUD audit emit. `ctx.caller_self_perspective` stays `None`
for master-token tool calls unless the caller passes an explicit
`_proxima_caller_self_perspective` arg, which assistants don't.

Decision: lift the master-token UUID into the identity. The MCP server
already keys auth by token UUID
(`crates/mcp-server/src/auth.rs:37`: `master_tokens: HashMap<Uuid,
Owner>`). We extend this to a stable `(master_token_uuid, owner) →
personality_instance_id` mapping, eager-minted on first connect, and
default `ctx.caller_self_perspective` to that personality's
Self-Perspective for every master-token tool call.

Effect:

- Each MCP client (Heinrich's Claude Code session, future marketplace
  automation, future hosted shell) gets its own stable Self-Perspective.
  Audit and provenance attribute to *this client*, not to a generic
  per-owner shell-author bucket.
- `goal_propose`'s existing `core/inspires` edge logic
  (`flavors/goal/src/tools/propose.rs:61-92`) starts working from
  master-token calls without any tool-side change — the edge points at
  the calling client's Self-Perspective.
- Master-token Fact emits (M1's lifecycle Facts among them) carry
  proper authorship.

Out of scope for S0:

- Goals targeting *specific other personalities* from a master-token
  call. That's an explicit `target_personality: P` arg on
  `goal_propose`, separable. The default behavior with S0 is "this
  goal originated from this client"; addressing other personalities
  is a separable extension.
- Per-token scopes, rate limits, or auth tiers beyond what the MCP
  server already enforces.

Lands in M0 (a small substrate milestone before M1).

### S1 — Goal lifecycle Facts

Today: `goal_propose` and `goal_accept` insert rows into
`proxima_core.goals` and (for accept) write `motivated_by` edges, but
**no Fact** lands in the wake stream. Personalities cannot wake on
"goal moved to Active."

Decision: emit a typed Fact on goal state transitions. Two new schemas
in the goal flavor:

```
proxima-goal/goal-proposed-v1
  payload: { goal_id, schema_id, title, proposer: {kind, ref} }

proxima-goal/goal-activated-v1
  payload: { goal_id, schema_id, title, accepted_at, evidence_count }
```

Emitted from the `goal_propose` and `goal_accept` paths after the
goals-table insert, in the same transaction. Authorship:
`EventSource("proxima-goal/lifecycle")`.

This is a small follow-on spec (~200 lines) that lands in M1.

### S2 — `core/emit_goal` substrate tool

Today: `core/emit_goal` is a stub returning `"not implemented in v1"`.

Decision: leave it stubbed for v0.1.0. The conversation-seeded
proposer uses `proxima-goal/goal_propose` (already wired). Implementing
`core/emit_goal` is part of M5 (the in-engine Goal Proposer
personality), not v0.1.0.

### S3 — Workspace runner Phase 3 + 4

`2026-05-09-workspace-mode-design.md` Phasing already covers this:
Phase 3 = Code workspace runner; Phase 4 = decision UX. M3 covers the
runner. M5 covers the worker recipe + decision UX.

### S4 — Planner recipe + execution-request Facts

The Code flavor must ship `plan_execution_requests.yaml` alongside
existing `engineer.yaml` and `commit_summary.yaml`.

The recipe wakes on accepted goals assigned to the configured planner
through `core/inspires`, explores graph/code MCP tools, and emits
`proxima-code/execution-request-v1` Facts. No default planner instance
is minted.

## Milestones

Each milestone is reviewable independently and lands as its own
implementation plan in `docs/superpowers/plans/`.

### M0 — Per-master-token shell-author identity (S0)

**Status:** Landed 2026-05-10. Plan:
[`docs/superpowers/plans/2026-05-10-m0-per-token-shell-author.md`](../plans/2026-05-10-m0-per-token-shell-author.md).
Acceptance verified end-to-end:
[`flavors/goal/tests/goal_propose_master_token_pg.rs`](../../../flavors/goal/tests/goal_propose_master_token_pg.rs)
(per-token inspires edge) and
[`crates/mcp-server/tests/master_token_identity.rs`](../../../crates/mcp-server/tests/master_token_identity.rs)
(call_tool ensure-on-call wiring).

- Promote `ensure_shell_author_personality(owner)` to
  `ensure_master_token_personality(owner, master_token_uuid)` (or
  equivalent — the storage trait shape is an implementation detail).
  Per-token UUID becomes part of the identity key; mapping persists
  across reconnects.
- MCP server (`crates/mcp-server/src/handler.rs`) calls the ensure
  path at the start of every master-token-authenticated tool
  invocation and threads the resulting Self-Perspective
  `MemoryId` into `ctx.caller_self_perspective`.
- Audit Fact `caller` enum gains the per-token instance_id;
  `PersonalityConfigChangedV1` `MasterToken` variant updates
  accordingly.
- **Acceptance:** `goal_propose` from a fresh master-token MCP
  connection writes a Goal **with** a `core/inspires` edge to the
  per-token Self-Perspective, with no explicit
  `_proxima_caller_self_perspective` arg. Reconnecting under the same
  token resolves to the same Self-Perspective. Two distinct master
  tokens against the same owner resolve to two distinct
  Self-Perspectives.
- **Dependencies:** none. Pure substrate.

### M1 — Goal lifecycle Facts (S1)

**Status:** Landed 2026-05-10.
Acceptance verified end-to-end:
[`flavors/goal/tests/goal_propose_master_token_pg.rs`](../../../flavors/goal/tests/goal_propose_master_token_pg.rs)
(master-token MCP `goal_propose` + `goal_accept` emit lifecycle
Facts with `core/authored` edges to the per-token Self-Perspective)
and
[`flavors/goal/tests/accept_decline_pg.rs`](../../../flavors/goal/tests/accept_decline_pg.rs)
(query returns both lifecycle Facts and authored edges).
Verification:
`cargo fmt --check`;
`cargo clippy -p proxima-flavor-goal --all-targets`;
`cargo test -p proxima-flavor-goal`;
`cargo check --workspace`.

- Add `proxima-goal/goal-proposed-v1` and `goal-activated-v1` payload
  modules + sidecar tables in goal flavor.
- Wire emits into `goal_propose` and `goal_accept` transactions.
- Surface in Atlas/Surface (free — they project the memory graph).
- **Acceptance:** call `goal_propose` then `goal_accept` via MCP; both
  Facts appear in `query` results with the right authorship (per-token
  Self-Perspective from M0 for master-token-driven calls).
- **Dependencies:** M0 (so authorship attributes correctly).

### M2 — Wake on goal-activated (substrate-only smoke test)

**Status:** Landed 2026-05-10.
Acceptance verified:
[`flavors/code/tests/goal_activated_wake_pg.rs`](../../../flavors/code/tests/goal_activated_wake_pg.rs)
(MCP `goal_propose` + `goal_accept` emits `goal-activated-v1`, the
SubstrateOnly smoke wake fires, and `core/emit_perspective` lands a
Perspective with `core/authored` from the smoke Root Perspective).
Verification:
`cargo fmt --check`;
`cargo test -p proxima-code --test goal_activated_wake_pg`;
`cargo clippy -p proxima-code --all-targets`;
`cargo check --workspace`.

- Author a temporary Code-flavor smoke personality with
  `execution_mode = SubstrateOnly` (the workspace runner is still the
  `Unimplemented` stub from workspace Phase 1). Recipe: a no-op Goose
  recipe that emits a single Perspective like "I would do X."
- Wake filter: `OnMemory{schema_id: proxima-goal/goal-activated-v1}`.
- **Acceptance:** propose + accept a goal → smoke personality wakes →
  Perspective lands authored by its Root Perspective.
- **Dependencies:** M1.
- **Why substrate-only first:** isolates the wake-trigger plumbing
  from the workspace-runner unknowns. Proves the chain *fires* before
  we plug in worktree machinery.

### M3 — Code workspace runner (S3, workspace Phase 3)

**Status:** Landed 2026-05-10.
Acceptance verified:
[`flavors/code/tests/workspace_run_pg.rs`](../../../flavors/code/tests/workspace_run_pg.rs)
(workspace wake fires on a Code Fact trigger, the runner creates a
worktree, the Goose mock commits a diff, and `workspace-run-v1` plus
`core/authored` / `core/derived-from` edges land).
Boundary note: core owns only generic workspace trigger eligibility and
opaque runner handoff; Code owns `repo_id`, `target_branch`, worktrees,
and workspace-run sidecars.
Verification:
`cargo fmt --check`;
`cargo test -p proxima-core`;
`cargo test -p proxima-code --test mcp_tools_pg`;
`cargo test -p proxima-code --test workspace_run_pg`;
`cargo test -p proxima-code --test goal_activated_wake_pg`;
`cargo clippy -p proxima-core --all-targets`;
`cargo clippy -p proxima-code --all-targets`;
`pnpm --filter @proxima/core typecheck`;
`cargo check --workspace`.

- Implement `CodeWorkspaceRunner` + `WorkspaceRunnerSource` per
  `2026-05-09-workspace-mode-design.md` Phase 3.
- E2E integration test: workspace wake fires, goose mock runs, Fact +
  edges land.
- **Acceptance:** workspace Phase 3 acceptance, unchanged.
- **Dependencies:** M2 (so the wake actually arrives) plus workspace
  Phases 1 and 2 (already in flight per workspace spec).

### M4 — Planner handoff + Code execution-request Facts

- Add Goal-to-planner assignment through `core/inspires` from Goal to
  planner Root Perspective.
- `goal_propose` / `goal_accept` accept optional
  `target_personality` handles. If omitted, current per-token caller
  Self behavior remains unchanged.
- Add Code Fact schema `proxima-code/execution-request-v1`:
  `{ repo_id, title, instructions, request_key }`.
- Add Code MCP tool `proxima-code/code_emit_execution_request`.
  Validates repo handle, Active Goal context, and idempotency key.
- Author `flavors/code/recipes/plan_execution_requests.yaml` and
  register `proxima-code/plan_execution_requests`.
- **Acceptance:** propose + accept a Goal assigned to a configured
  planner; planner wakes; one or more execution-request Facts land
  with repo scope and `core/authored` / `core/derived-from`
  provenance.
- **Dependencies:** M3.

### M5 — Workspace worker recipe + decision UX

- Worker wakes on `proxima-code/execution-request-v1`.
- Code workspace runner extracts `repo_id` from that Code Fact.
- Worker emits `workspace-run-v1`.
- User reviews via Workspace Runs panel and writes
  `workspace-decision-v1`.
- **Acceptance:** v0.1.0 acceptance test §1–6. Full closed loop.
- **Dependencies:** M4.

### M6 — Autonomous goal proposer

- Add an autonomous proposer personality that wakes on Engineer's
  Perspectives and calls `proxima-goal/goal_propose` via the
  goal-flavor MCP tool from inside its wake.
- Implement `core/emit_goal` substrate tool (S2).
- The chain becomes self-spinning: any commit can flow up to a
  proposed Goal without Heinrich talking to the assistant first.
- Goal acceptance gate stays.
- **Not required for v0.1.0.**

## Boundedness & runaway prevention

The loop has four natural circuit-breakers, in increasing severity:

1. **`max_rounds` per WakeEntry.** Goose stops at the bound; the
   personality emits whatever it has and returns.
2. **`probability_promille` per WakeEntry.** Optional probabilistic
   damping; off by default for the four personalities above (always
   fire) but available if the chain churns.
3. **Wake filter `authored_by`.** A personality whose wake filter
   excludes its own outputs cannot self-trigger. Engineer's wake
   filter (`{schema_id: commit-v1}`) implicitly excludes Engineer's
   own writes since Engineer doesn't emit `commit-v1`. Same for
   planner on `goal-activated-v1` and worker on `execution-request-v1`.
4. **Human gates.** Both gates (goal accept, workspace decide) are
   synchronous: the chain *cannot* continue without a human
   acknowledgment. v0.1.0's safety story rests on this.

If a runaway is detected post-v0.1.0 (e.g. M6's proposer flooding
proposals), the response is a probability damper on Goal Proposer's
WakeEntry — a config change, not an architecture change.

## What we are NOT proving in v0.1.0

Listing this explicitly to keep ambition bounded:

- Cross-flavor chains (e.g. Code → Learning).
- Multi-instance personality scale-out.
- Goal trees / sub-goals / plan decomposition.
- Self-mutation of WakeConfig from inside a wake.
- Failure recovery beyond "human rejects the workspace run."
- Continuous-mode autonomy (no human gates).
- Cross-owner reasoning.

Each is a downstream spec.

## References

- `crates/core/src/personality/mod.rs` — `WakeEntry`,
  `WakeEntryTriggerKind {OnMemory, OnEdge}`, `PersonalityInstanceId`.
- `crates/core/src/wake/fire.rs` — current substrate wake path;
  workspace branch short-circuit (lines 158-184) is the surface
  workspace Phase 3 replaces.
- `flavors/goal/src/tools/{propose,accept,decline,modify}.rs` — Goal
  MCP tools.
- `flavors/code/src/payloads/{commit_summary,development_perspective}.rs`
  — current Engineer outputs.
- `flavors/code/recipes/{engineer,commit_summary,plan_execution_requests}.yaml`
  — current Code-flavor recipes.

## Rollback

Each milestone is independently revertable:

- M0 — revert the storage trait extension + handler wiring + audit
  payload variant. The singleton-per-owner shell-author keeps
  working as fallback.
- M1 — revert the two payload modules + sidecar migrations + the two
  emit-call sites. No code consumes the new Facts yet.
- M2 — delete the temporary smoke-test personality (tombstone). No
  schema or migration changes.
- M3 — workspace Phase 3 rollback (already specced).
- M4 — remove the planner recipe, execution-request schema/tool, and
  Goal target-personality args.
- M5 — workspace Phase 4 rollback + remove the worker recipe.

The chain can sit at any milestone boundary indefinitely; later
milestones don't change earlier ones.
