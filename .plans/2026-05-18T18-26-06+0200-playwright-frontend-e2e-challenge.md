# Flexible Frontend E2E Challenge

Goal: add a harder live demo challenge that tests whether the wheel can use shell-native repo tooling for frontend verification.
Architecture: keep Proxima tools for graph/state transitions; use workspace shell for git inspection, tests, and frontend toolchains. No Playwright MCP or Proxima-owned browser runner in this iteration.
Tech Stack: Rust `proxima-code`, existing demo wheel test, package-free static frontend challenge, Node built-in tests.
REQUIRED SUB-SKILL: superpowers-executing-plans

Status: Implemented, live E2E still under iteration
Created: 2026-05-18
Reviewed:
Implemented: 2026-05-18
Implementation:
Verification:
- `git diff --check` passed
- `cargo test -p proxima-code --test demo_wheel_pg --no-run` passed
- `cargo test -p proxima-code --test workspace_run_pg workspace_run_review_finalize -- --test-threads=1` passed
- `cargo test -p proxima-code --test workspace_run_pg workspace_run_trigger_prepares_verifier_context_from_worker_branch -- --test-threads=1` passed
- Live Kanban run after inspect-only finalization: failed at max dispatcher ticks, but workspace-run loop is gone (`workspace_run_count = 3`, `workspace_review_count = 0`).
- Live Kanban run after verifier read-only palette and `8` verifier rounds: failed at max dispatcher ticks after real progress (`workspace_run_count = 4`, `workspace_review_count = 3`, `goal_achieved_count = 1`).
- Live Kanban run after `18` ticks and verifier `10` rounds: failed at max dispatcher ticks after correction progress (`workspace_run_count = 4`, `workspace_review_count = 4`, `goal_achieved_count = 2`).
- Live Kanban run after `24` ticks: failed at max dispatcher ticks with one unreviewed child run (`workspace_run_count = 3`, `workspace_review_count = 2`, `goal_achieved_count = 2`).
Notes:
- Current decision: explicit browser tooling is deferred until experiments show shell-driven verification is too weak.
- Current gap: dispatcher scheduling/termination for the harder decomposed flow is still nondeterministic. The old verifier self-loop is fixed; remaining failures are missed final verifier/goal-reviewer turns before tick cap.

## Summary

Current implemented state:
- `flavors/code/tests/demo_wheel_pg.rs` supports `signal_match`, `todo_cli`, and `kanban_board`.
- Verifier workspace context includes `diff_inspection_commands`.
- Verifier prompts tell the personality to inspect git status/diff through `workspace_shell` and not edit files.
- Existing-run workspace preparations finalize as inspect-only and do not emit another `WorkspaceRunV1`.
- Inspect-only finalization rejects verifier-caused worktree mutations.
- `kanban_board` seeds a static frontend repo with `README.md`, `data/tasks.json`, and `docs/acceptance.md`.
- Planner is encouraged, not forced, to decompose the frontend task into three child goals.
- Worker can edit with `workspace_text_editor` and execute repo-native checks with `workspace_shell`.
- Verifier checks the resulting app through shell commands, including `node test_kanban.mjs`.

## File Structure

- `flavors/code/src/workspace_runner/prepare.rs` - adds review-time `diff_inspection_commands`.
- `flavors/code/tests/demo_wheel_pg.rs` - adds `kanban_board` challenge, prompts, seed repo, metrics gates, and verifier shell checks.

## Goals / Acceptance

- [x] Verifier can explore worktree diff through shell-provided git commands.
- [x] `PROXIMA_DEMO_CHALLENGE=kanban_board` is accepted by the demo test.
- [x] Kanban seed repo includes static frontend input data and acceptance docs.
- [x] Planner prompt advertises three decomposable frontend work streams.
- [x] Worker prompt asks for package-free `index.html` plus executable `test_kanban.mjs`.
- [x] Verifier prompt runs shell-based git inspection and `node test_kanban.mjs`.
- [x] Verifier is configured without `workspace_text_editor`.
- [x] Existing-run verifier finalization does not emit a new `WorkspaceRunV1`.
- [x] Kanban deterministic gates require three child goals/runs/reviews.
- [x] Compile check passes.

## Decisions

- Browser tooling: do not add Playwright MCP, `BrowserSmoke`, or a side container now. Rationale: shell already exposes repo-native test runners and avoids reinventing each domain tool.
- Frontend proof: require executable repo-native tests through `workspace_shell`. Rationale: Planner/Worker can choose the test strategy; Verifier observes by running commands.
- Challenge shape: `kanban_board`. Rationale: harder than the CLI because it requires UI contract, state, filtering, movement, persistence, and tests.
- Write access: Worker remains the repo writer. Verifier uses shell/listing for inspection and review evidence, not file edits.
- Existing-run finalization: no new event type. Runner state carries a private finalize policy; only fresh execution requests emit `WorkspaceRunV1`.
- Future BrowserSmoke: only add if repeated experiments show skipped/faked browser verification or need stable screenshots for scoring.

## Implemented Slice

### Diff Inspection Context

Files:
- Modified: `flavors/code/src/workspace_runner/prepare.rs`

Behavior:
```json
{
  "diff_inspection_commands": [
    "git status --short",
    "git diff --stat <parent>..HEAD",
    "git diff --name-only <parent>..HEAD",
    "git diff --unified=80 <parent>..HEAD"
  ]
}
```

### Kanban Challenge

Files:
- Modified: `flavors/code/tests/demo_wheel_pg.rs`

Runtime:
```sh
PROXIMA_LIVE_MISTRAL=1 \
PROXIMA_DEMO_CHALLENGE=kanban_board \
PROXIMA_DEMO_REPO=/private/tmp/proxima-kanban-e2e \
cargo test -p proxima-code --test demo_wheel_pg -- --ignored --nocapture --test-threads=1
```

Expected signals:
- `overall_pass: true`
- `functional_pass: true`
- `deterministic_pass: true`
- `final_goal_state: Achieved`
- `goal_graph.child_goal_count >= 3`
- `goal_graph.child_workspace_run_count >= 3`
- `goal_graph.verification_evidence_count >= 1`
- changed files include `index.html` and `test_kanban.mjs`
- no app-local package files are required

## Inspect-Only Finalization

Files:
- Modified: `flavors/code/src/workspace_runner/mod.rs`
- Modified: `flavors/code/src/workspace_runner/prepare.rs`
- Modified: `flavors/code/tests/workspace_run_pg.rs`

Contract:
- Fresh `ExecutionRequestV1` workspace runs emit `WorkspaceRunV1`.
- Existing `WorkspaceRunV1` / review / decision preparations are inspect-only.
- Inspect-only finalize returns `primary_memory_id = None`.
- Inspect-only finalize fails if `HEAD` or `git status --porcelain` changes during inspection.

## Out of Scope

- Playwright MCP tools.
- Proxima-owned browser smoke runner.
- Browser side container.
- Automatic repo ingestion after merge.
- Read-only shell allowlisting.
