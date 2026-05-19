use super::*;

#[derive(Clone)]
pub(super) struct WakeOptions {
    pub(super) authored_by: WakeEntryAuthoredBy,
    pub(super) goal_scope: WakeEntryGoalScope,
    pub(super) max_rounds: u16,
    pub(super) intervention_policy: Option<InterventionPolicy>,
}

impl WakeOptions {
    pub(super) fn default_with_rounds(max_rounds: u16) -> Self {
        Self {
            authored_by: WakeEntryAuthoredBy::Other,
            goal_scope: WakeEntryGoalScope::None,
            max_rounds,
            intervention_policy: None,
        }
    }
}

pub(super) fn demo_intervention_policy(
    wake_supervisor: PersonalityInstanceId,
    mode: DemoInterventionMode,
) -> InterventionPolicy {
    InterventionPolicy {
        intervention_personality_instance_id: wake_supervisor.into_inner(),
        intervention_extension_rounds: 4,
        intervention_hard_cap_rounds: 8,
        intervention_progress_contract: match mode {
            DemoInterventionMode::Normal => "Decide from the intervention request Fact and wake lineage whether the truncated wake made concrete progress toward the active demo Goal. Loops with repeated tool errors should stop. Truncations after useful work or after the larger goal has enough downstream evidence may be accepted as terminal for v1; automatic continuation is allowed when the truncated wake has useful unfinished work.".into(),
            DemoInterventionMode::ForceContinue => "Forced continuation demo. Emit continue for the first useful max-round truncation so the next dispatcher tick starts a continuation from the InterventionDecisionV1 event.".into(),
        },
    }
}

pub(super) async fn prepare_demo_repo(
    path: &Path,
    challenge: DemoChallenge,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    if path.exists() {
        let marker = path.join(".proxima-demo-repo");
        if marker.is_file() {
            std::fs::remove_dir_all(path)?;
        } else if path.read_dir()?.next().is_some() {
            return Err(format!(
                "PROXIMA_DEMO_REPO exists and is not marked as a Proxima demo repo: {}",
                path.display()
            )
            .into());
        }
    }
    std::fs::create_dir_all(path)?;
    std::fs::write(path.join(".proxima-demo-repo"), challenge.marker())?;
    match challenge {
        DemoChallenge::SignalMatch => {
            std::fs::write(
                path.join("README.md"),
                "# Signal Match\n\nThe demo wheel should create `index.html`.\n",
            )?;
        }
        DemoChallenge::TodoCli => {
            std::fs::create_dir_all(path.join("examples"))?;
            std::fs::write(
                path.join("README.md"),
                "# Todo Audit\n\nBuild `todo_audit.mjs` and `test_todo_audit.mjs`. Use only Node built-ins.\n",
            )?;
            std::fs::write(
                path.join("examples/tasks.md"),
                "- [ ] Ship parser @ana #cli !high due:2026-05-17\n- [x] Draft README @bo #docs !low due:2026-05-10\n- [ ] Add JSON output @ana #cli #report !medium due:2026-05-20\n- [ ] Triage backlog @cy #ops !high\n",
            )?;
        }
        DemoChallenge::KanbanBoard => {
            std::fs::create_dir_all(path.join("data"))?;
            std::fs::create_dir_all(path.join("docs"))?;
            std::fs::write(
                path.join("README.md"),
                "# Kanban Board\n\nBuild a package-free static frontend in `index.html` and executable tests in `test_kanban.mjs`. Use only browser APIs and Node built-ins. The app must run by opening `index.html` directly.\n",
            )?;
            std::fs::write(
                path.join("data/tasks.json"),
                r#"[
  {"id":"api-contract","title":"Review API contract","status":"backlog","owner":"Ana","priority":"high","tag":"API"},
  {"id":"wire-ui","title":"Wire board UI","status":"active","owner":"Bo","priority":"medium","tag":"UI"},
  {"id":"blocked-auth","title":"Unblock auth mock","status":"blocked","owner":"Cy","priority":"high","tag":"Auth"},
  {"id":"docs-done","title":"Publish usage notes","status":"done","owner":"Dee","priority":"low","tag":"Docs"}
]
"#,
            )?;
            std::fs::write(
                path.join("docs/acceptance.md"),
                "# Acceptance\n\nRequired test ids: `app-title`, `search-input`, `status-filter`, `task-card`, `move-next`, `move-prev`, `task-count`, `done-count`, `reset-board`.\n\nRequired behavior: render seeded tasks, filter by text/status, move tasks between columns, update counters, persist board state with localStorage key `proxima-kanban-demo-v1`, and reset to seed data.\n",
            )?;
        }
    }
    git(path, &["init", "-b", "main"])?;
    git(path, &["add", "."])?;
    git(
        path,
        &[
            "-c",
            "user.name=Proxima Demo",
            "-c",
            "user.email=demo@example.test",
            "commit",
            "-m",
            match challenge {
                DemoChallenge::SignalMatch => "chore: seed signal match demo",
                DemoChallenge::TodoCli => "chore: seed todo audit demo",
                DemoChallenge::KanbanBoard => "chore: seed kanban board demo",
            },
        ],
    )?;
    Ok(Uuid::now_v7())
}

pub(super) fn visionary_instruction(
    challenge: DemoChallenge,
    mode: DemoInterventionMode,
) -> String {
    let normal = format!(
        "You are the Visionary for the triggering active Goal in N1. Do not create files, child Goals, or execution requests. Interpret the user's real expectation before planning. Use the `triggering_memory` context JSON handles: typed_payload.goal_id is the goal handle and memory is the goal_activated_memory handle. Call the available VisionBrief emit tool listed in Wake Contract exactly once. Pass top-level JSON fields matching schema \"{}\" v1: goal_id from triggering_memory.typed_payload.goal_id; goal_activated_memory_id from triggering_memory.memory; original_goal_text {}; interpreted_outcome as the intended product outcome, not an HTML implementation detail; target_user; use_case; artifact_shape; ambition_level \"Production\"; quality_bar; constraints as an array of strings; assumptions as an array of strings; open_questions as an array of strings; acceptance_rubric as a flat JSON array of strings, not an object; demo_proof; planner_directive; optional text. Do not pass schema_id, schema_version, or payload. The planner_directive must tell Planner to walk lineage from the VisionBrief to the goal_activated Fact, decompose the parent Goal, and preserve the quality bar. Then stop.",
        VisionBriefV1::SCHEMA_ID,
        serde_json::to_string(challenge.goal_text()).expect("goal text serializes")
    );
    match mode {
        DemoInterventionMode::Normal => normal,
        DemoInterventionMode::ForceContinue => format!(
            "Forced continuation branch: if the Continuation context is present, do not call a VisionBrief emit tool and do not emit a second VisionBrief. Instead call core_fetch_memory exactly once for each Continuation handle: continuation.intervention_decision.handle, continuation.intervention_request.handle, continuation.prior_wake_trace.handle, and continuation.original_triggering_memory.handle. After those four fetches, stop. If no Continuation context is present, follow this first-wake instruction exactly:\n\n{normal}"
        ),
    }
}

pub(super) fn planner_instruction(
    planner: PersonalityInstanceId,
    challenge: DemoChallenge,
    mode: DemoPlannerMode,
) -> String {
    match mode {
        DemoPlannerMode::Scripted => match challenge {
            DemoChallenge::SignalMatch => signal_match_planner_instruction(planner),
            DemoChallenge::TodoCli => todo_cli_planner_instruction(planner),
            DemoChallenge::KanbanBoard => kanban_board_planner_instruction(planner),
        },
        DemoPlannerMode::Real => real_planner_instruction(planner, challenge),
    }
}

pub(super) fn worker_instruction(challenge: DemoChallenge) -> String {
    match challenge {
        DemoChallenge::SignalMatch => {
            let app = signal_match_index_html();
            format!(
                "Use workspace_text_editor to create `index.html` with exactly this file_text, then run workspace_shell with command `test -f index.html && grep -E \"Signal Match|data-pad|keydown|restart|level|score\" index.html` and stop. file_text JSON string: {}",
                serde_json::to_string(&app).expect("serialize app")
            )
        }
        DemoChallenge::TodoCli => {
            "Implement the requested Todo Audit CLI using only Node.js built-ins. Create or update `todo_audit.mjs`, `test_todo_audit.mjs`, and `examples/tasks.md` as needed. The CLI must support `node todo_audit.mjs <markdown-file> --today 2026-05-18 --json`, parse Markdown task-list items, extract done/open state, @owner, #tags, !priority, due:YYYY-MM-DD, compute totals, open/done/overdue counts, byOwner, byTag, highPriorityOpen, and nextDue sorted by due date. Write meaningful tests in `test_todo_audit.mjs` using node:assert/child_process only, run `node test_todo_audit.mjs`, then stop.".into()
        }
        DemoChallenge::KanbanBoard => {
            "Implement the requested Kanban frontend using only `index.html`, browser APIs, and Node built-ins for tests. Create or update `index.html` and `test_kanban.mjs`; keep `data/tasks.json` and `docs/acceptance.md` useful if needed. Required test ids: app-title, search-input, status-filter, task-card, move-next, move-prev, task-count, done-count, reset-board. The app must render seeded tasks, filter by search and status, move tasks between backlog/active/blocked/done with accessible buttons, update task and done counters, persist state under localStorage key `proxima-kanban-demo-v1`, reset to seed data, and run by opening index.html directly. Write meaningful package-free tests in `test_kanban.mjs` using node:assert/fs only; the tests should inspect index.html for the selector contract, seeded-data contract, localStorage key, and movement/filtering logic markers. Run `node test_kanban.mjs`, then stop.".into()
        }
    }
}

pub(super) fn verifier_instruction(
    challenge: DemoChallenge,
    planner_mode: DemoPlannerMode,
) -> String {
    match (challenge, planner_mode) {
        (DemoChallenge::SignalMatch, DemoPlannerMode::Real) => {
            signal_match_real_planner_verifier_instruction()
        }
        (DemoChallenge::SignalMatch, _) => signal_match_verifier_instruction(),
        (DemoChallenge::TodoCli, _) => todo_cli_verifier_instruction(),
        (DemoChallenge::KanbanBoard, _) => kanban_board_verifier_instruction(),
    }
}

pub(super) fn real_planner_instruction(
    _planner: PersonalityInstanceId,
    challenge: DemoChallenge,
) -> String {
    let repo_handle = challenge.repo_handle();
    let goal_text = challenge.goal_text();
    let child_bounds = match challenge {
        DemoChallenge::SignalMatch => "one or two",
        DemoChallenge::TodoCli | DemoChallenge::KanbanBoard => "two or three",
    };
    format!(
        "You are the Planner. This is real-planner demo mode: plan from the Goal, VisionBrief, Triggering Memory, Wake Contract, Coordination Context, and tool descriptors instead of replaying fixture child goals. Do not use scripted child titles or request keys. Use only handles from context for graph/runtime references. If N1 is a proxima-intent VisionBrief, walk lineage from N1 to the active parent goal, decompose that parent exactly once with target_personality \"P1\", activate the children, and author {child_bounds} original child goals that together cover this target outcome: {}. Then stop. If N1 is a child proxima-goal/goal-activated-v1 Fact, emit exactly one execution request for repo_handle \"{repo_handle}\" using N1 as the activated goal, with a title, implementation instructions, idempotency key, and acceptance criteria derived from that child goal. Acceptance criteria must be repo-native, include the primary output, and be deterministic enough for a verifier or reviewer. Then stop.",
        serde_json::to_string(goal_text).expect("goal text serializes"),
    )
}

pub(super) fn scripted_child_titles() -> Vec<&'static str> {
    vec![
        "Signal Match static shell and responsive pads",
        "Signal Match gameplay controls and restart loop",
        "Todo Audit parser and data model",
        "Todo Audit JSON summary CLI",
        "Todo Audit fixtures and tests",
        "Kanban board shell and seeded task rendering",
        "Kanban filtering counters and accessible movement",
        "Kanban persistence reset and executable tests",
    ]
}

pub(super) fn scripted_request_keys() -> Vec<&'static str> {
    vec![
        "demo-signal-match-shell",
        "demo-signal-match-gameplay",
        "demo-todo-audit-parser",
        "demo-todo-audit-summary",
        "demo-todo-audit-tests",
        "demo-kanban-board-shell",
        "demo-kanban-board-interactions",
        "demo-kanban-board-tests",
    ]
}

pub(super) fn signal_match_planner_instruction(planner: PersonalityInstanceId) -> String {
    let _ = planner;
    format!(
        "You are the Planner. If N1 is a proxima-intent VisionBrief, call core_walk_lineage with memory \"N1\", direction \"ancestors\", depth 2, limit 10; find the returned proxima-goal/goal-activated-v1 memory handle; call proxima_goal_goal_decompose with parent_goal set to that handle, activate_children true, target_personality \"P1\", idempotency_key \"demo-signal-match-decompose\", and these suggested children: {}. Then stop. If N1 is already one of those child goal_activated Facts, call proxima_code_code_emit_execution_request for that child with repo_handle \"{}\", goal_activated_memory \"N1\", evidence [], a child-specific title/instructions/idempotency_key, and these required acceptance_criteria: {}. Use idempotency_key \"demo-signal-match-shell\" for the shell/pads child and \"demo-signal-match-gameplay\" for the gameplay/restart child. Then stop.",
        json!([
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Signal Match static shell and responsive pads",
                        "text": "Create index.html with a package-free responsive Signal Match shell, title, four colored pads, and direct browser entrypoint."
                    }
                },
                "evidence": []
            },
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Signal Match gameplay controls and restart loop",
                        "text": "Create index.html gameplay behavior for sequence playback, click input, Q W A S keyboard input, score and level display, failure state, and restart control."
                    }
                },
                "evidence": []
            }
        ]),
        SIGNAL_MATCH_REPO_HANDLE,
        json!([
            {
                "key": "static_entrypoint",
                "description": "index.html exists and runs without package installation",
                "required": true,
                "verifier_kind": "file_exists",
                "verifier_spec": { "path": "index.html" }
            },
            {
                "key": "gameplay_controls",
                "description": "Signal Match includes pads, keyboard input, score, level, failure state, and restart",
                "required": true,
                "verifier_kind": "command",
                "verifier_spec": {
                    "command": ["grep", "-E", "Signal Match|data-pad|keydown|restart|level|score|game-over", "index.html"]
                }
            }
        ])
    )
}

pub(super) fn todo_cli_planner_instruction(planner: PersonalityInstanceId) -> String {
    let _ = planner;
    format!(
        "You are the Planner. If N1 is a proxima-intent VisionBrief, call core_walk_lineage with memory \"N1\", direction \"ancestors\", depth 2, limit 10; find the returned proxima-goal/goal-activated-v1 memory handle; call proxima_goal_goal_decompose with parent_goal set to that handle, activate_children true, target_personality \"P1\", idempotency_key \"demo-todo-audit-decompose\", and these suggested children: {}. Then stop. If N1 is already one of those child goal_activated Facts, call proxima_code_code_emit_execution_request for that child with repo_handle \"{}\", goal_activated_memory \"N1\", evidence [], a child-specific title/instructions/idempotency_key, and these required acceptance_criteria: {}. Each child request must still produce a complete runnable CLI and test suite because workspace runs are evaluated independently. Then stop.",
        json!([
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Todo Audit parser and data model",
                        "text": "Implement Markdown task-list parsing for done/open state, @owner, #tags, !priority, due date tokens, and stable task records."
                    }
                },
                "evidence": []
            },
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Todo Audit JSON summary CLI",
                        "text": "Implement a package-free Node CLI that reads a Markdown file and prints deterministic JSON summary counts, byOwner, byTag, highPriorityOpen, and nextDue."
                    }
                },
                "evidence": []
            },
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Todo Audit fixtures and tests",
                        "text": "Add sample Markdown tasks and Node built-in tests that verify parser and CLI JSON behavior."
                    }
                },
                "evidence": []
            }
        ]),
        TODO_CLI_REPO_HANDLE,
        json!([
            {
                "key": "cli_entrypoint",
                "description": "todo_audit.mjs exists and can be executed with Node without package installation",
                "required": true,
                "verifier_kind": "file_exists",
                "verifier_spec": { "path": "todo_audit.mjs" }
            },
            {
                "key": "parser_tests",
                "description": "Node built-in test script passes",
                "required": true,
                "verifier_kind": "command",
                "verifier_spec": { "command": ["node", "test_todo_audit.mjs"] }
            },
            {
                "key": "json_summary",
                "description": "CLI emits deterministic JSON summary for examples/tasks.md",
                "required": true,
                "verifier_kind": "command",
                "verifier_spec": { "command": ["sh", "-c", "node todo_audit.mjs examples/tasks.md --today 2026-05-18 --json | grep -E '\"total\"|\"open\"|\"byOwner\"|\"nextDue\"'"] }
            }
        ])
    )
}

pub(super) fn kanban_board_planner_instruction(planner: PersonalityInstanceId) -> String {
    let _ = planner;
    format!(
        "You are the Planner. If N1 is a proxima-intent VisionBrief, call core_walk_lineage with memory \"N1\", direction \"ancestors\", depth 2, limit 10; find the returned proxima-goal/goal-activated-v1 memory handle; call proxima_goal_goal_decompose with parent_goal set to that handle, activate_children true, target_personality \"P1\", idempotency_key \"demo-kanban-board-decompose\", and these suggested children: {}. Then stop. If N1 is already one of those child goal_activated Facts, call proxima_code_code_emit_execution_request for that child with repo_handle \"{}\", goal_activated_memory \"N1\", evidence [], a child-specific title/instructions/idempotency_key, and these required acceptance_criteria: {}. Each child request must still produce a complete package-free index.html and test_kanban.mjs because workspace runs are evaluated independently. The planner may ask for browser-style or DOM tests, but verification is executed through shell and repo-native commands, not a special browser tool. Then stop.",
        json!([
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Kanban board shell and seeded task rendering",
                        "text": "Create a package-free index.html Kanban shell that renders seeded tasks into responsive backlog, active, blocked, and done columns with the required data-testid selector contract."
                    }
                },
                "evidence": []
            },
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Kanban filtering counters and accessible movement",
                        "text": "Implement search, status filtering, task and done counters, and accessible move-next and move-prev controls for tasks."
                    }
                },
                "evidence": []
            },
            {
                "payload": {
                    "schema_id": "proxima-goal/simple-text-v1",
                    "body": {
                        "title": "Kanban persistence reset and executable tests",
                        "text": "Persist board state with localStorage key proxima-kanban-demo-v1, add reset behavior, and write package-free Node tests that verify the frontend contract."
                    }
                },
                "evidence": []
            }
        ]),
        KANBAN_REPO_HANDLE,
        json!([
            {
                "key": "static_entrypoint",
                "description": "index.html exists and runs without package installation",
                "required": true,
                "verifier_kind": "file_exists",
                "verifier_spec": { "path": "index.html" }
            },
            {
                "key": "frontend_tests",
                "description": "Package-free frontend contract tests pass",
                "required": true,
                "verifier_kind": "command",
                "verifier_spec": { "command": ["node", "test_kanban.mjs"] }
            },
            {
                "key": "ui_contract",
                "description": "Kanban app exposes required selectors and persistence key",
                "required": true,
                "verifier_kind": "command",
                "verifier_spec": {
                    "command": ["sh", "-c", "grep -E 'data-testid=\"(app-title|search-input|status-filter|task-card|move-next|move-prev|task-count|done-count|reset-board)\"|proxima-kanban-demo-v1|localStorage' index.html"]
                }
            }
        ])
    )
}

pub(super) fn signal_match_verifier_instruction() -> String {
    "Inspect the prepared workspace and its diff before judging. Do not edit files. The workspace context contains diff_inspection_commands; if the embedded diff is insufficient, run workspace_shell with those git status/diff commands. Then run workspace_shell with command `test -f index.html && grep -E \"Signal Match|data-pad|keydown|restart|level|score|game-over\" index.html`. If it exits 0, first call proxima_code_code_emit_verification_evidence twice: {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"static_entrypoint\",\"status\":\"passed\",\"summary\":\"index.html exists\",\"artifact_refs\":{\"path\":\"index.html\"},\"idempotency_key\":\"demo-signal-match-evidence-static\"} and {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"gameplay_controls\",\"status\":\"passed\",\"summary\":\"index.html contains Signal Match controls and states\",\"artifact_refs\":{\"path\":\"index.html\"},\"idempotency_key\":\"demo-signal-match-evidence-gameplay\"}. Then call proxima_code_code_emit_workspace_review with {\"workspace_run_memory\":\"N1\",\"verdict\":\"approved\",\"summary\":\"Signal Match requirements satisfied\",\"findings\":[],\"verification_summary\":\"index.html exists and contains direct-run Signal Match gameplay controls\",\"idempotency_key\":\"demo-signal-match-review-approved\"}. If the shell check fails, call proxima_code_code_emit_verification_evidence for both keys with status \"failed\", then call the review tool with verdict rejected, summary \"Signal Match requirements missing\", one finding for index.html, correction_instructions \"Create a complete direct-run Signal Match index.html. Failed criteria: static_entrypoint, gameplay_controls\", and idempotency_key \"demo-signal-match-review-rejected\". Then stop.".into()
}

pub(super) fn signal_match_real_planner_verifier_instruction() -> String {
    "Inspect the prepared workspace and its diff before judging. Do not edit files. Use Wake Contract, Triggering Memory, Workspace Context, and tool descriptors for handles, available tools, and argument shapes. Treat N1 as the workspace_run memory when emitting verification evidence or workspace review. Run workspace_shell with command `test -f index.html && grep -E \"Signal Match|data-pad|keydown|restart|level|score|game-over\" index.html`. If it exits 0, emit passed evidence for every deterministic acceptance criterion in Workspace Context; if no deterministic criteria exist, emit fallback passed evidence for static_entrypoint and gameplay_controls. Then emit an approved workspace review. If the shell check fails, emit failed evidence for every deterministic acceptance criterion present, then emit a rejected workspace review with correction instructions for a complete direct-run Signal Match index.html with pads, keyboard input, score, level, failure state, and restart. Then stop.".into()
}

pub(super) fn todo_cli_verifier_instruction() -> String {
    "Inspect the prepared workspace and its diff before judging. Do not edit files. The workspace context contains diff_inspection_commands; if the embedded diff is insufficient, run workspace_shell with those git status/diff commands. Then run workspace_shell with command `test -f todo_audit.mjs && test -f test_todo_audit.mjs && test -f examples/tasks.md && node test_todo_audit.mjs && node todo_audit.mjs examples/tasks.md --today 2026-05-18 --json | grep -E '\"total\"|\"open\"|\"byOwner\"|\"nextDue\"'`. If it exits 0, first call proxima_code_code_emit_verification_evidence exactly three times with these JSON objects: {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"cli_entrypoint\",\"status\":\"passed\",\"summary\":\"todo_audit.mjs exists and runs with Node\",\"artifact_refs\":{\"paths\":[\"todo_audit.mjs\"]},\"idempotency_key\":\"demo-todo-audit-evidence-entrypoint\"}, {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"parser_tests\",\"status\":\"passed\",\"summary\":\"node test_todo_audit.mjs passed\",\"artifact_refs\":{\"paths\":[\"test_todo_audit.mjs\",\"todo_audit.mjs\"]},\"idempotency_key\":\"demo-todo-audit-evidence-tests\"}, and {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"json_summary\",\"status\":\"passed\",\"summary\":\"CLI emitted expected JSON summary fields\",\"artifact_refs\":{\"paths\":[\"examples/tasks.md\",\"todo_audit.mjs\"]},\"idempotency_key\":\"demo-todo-audit-evidence-json\"}. Then call proxima_code_code_emit_workspace_review with {\"workspace_run_memory\":\"N1\",\"verdict\":\"approved\",\"summary\":\"Todo Audit CLI requirements satisfied\",\"findings\":[],\"verification_summary\":\"entrypoint, tests, and JSON summary passed\",\"idempotency_key\":\"demo-todo-audit-review-approved\"}. If the shell check fails, first call proxima_code_code_emit_verification_evidence exactly three times with status \"failed\" for cli_entrypoint, parser_tests, and json_summary, using artifact_refs objects like {\"paths\":[\"todo_audit.mjs\"]}. Then call the review tool with verdict rejected, summary \"Todo Audit CLI requirements missing\", one finding for todo_audit.mjs, correction_instructions \"Create a complete package-free Node Todo Audit CLI with parser tests and deterministic JSON output. Failed criteria: cli_entrypoint, parser_tests, json_summary\", and idempotency_key \"demo-todo-audit-review-rejected\". Then stop.".into()
}

pub(super) fn kanban_board_verifier_instruction() -> String {
    "Inspect the prepared workspace and its diff before judging. Do not edit files. The workspace context contains diff_inspection_commands; if the embedded diff is insufficient, run workspace_shell with those git status/diff commands. Then run workspace_shell with command `test -f index.html && test -f test_kanban.mjs && node test_kanban.mjs && grep -E 'data-testid=\"(app-title|search-input|status-filter|task-card|move-next|move-prev|task-count|done-count|reset-board)\"|proxima-kanban-demo-v1|localStorage' index.html`. If it exits 0, first call proxima_code_code_emit_verification_evidence exactly three times with these JSON objects: {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"static_entrypoint\",\"status\":\"passed\",\"summary\":\"index.html exists and runs directly\",\"artifact_refs\":{\"paths\":[\"index.html\"]},\"idempotency_key\":\"demo-kanban-evidence-entrypoint\"}, {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"frontend_tests\",\"status\":\"passed\",\"summary\":\"node test_kanban.mjs passed\",\"artifact_refs\":{\"paths\":[\"test_kanban.mjs\",\"index.html\"]},\"idempotency_key\":\"demo-kanban-evidence-tests\"}, and {\"workspace_run_memory\":\"N1\",\"criterion_key\":\"ui_contract\",\"status\":\"passed\",\"summary\":\"Kanban selector and localStorage contract present\",\"artifact_refs\":{\"paths\":[\"index.html\",\"docs/acceptance.md\"]},\"idempotency_key\":\"demo-kanban-evidence-ui-contract\"}. Then call proxima_code_code_emit_workspace_review with {\"workspace_run_memory\":\"N1\",\"verdict\":\"approved\",\"summary\":\"Kanban frontend requirements satisfied\",\"findings\":[],\"verification_summary\":\"entrypoint, package-free tests, selector contract, and persistence contract passed through shell\",\"idempotency_key\":\"demo-kanban-review-approved\"}. If the shell check fails, first call proxima_code_code_emit_verification_evidence exactly three times with status \"failed\" for static_entrypoint, frontend_tests, and ui_contract, using artifact_refs objects like {\"paths\":[\"index.html\",\"test_kanban.mjs\"]}. Then call the review tool with verdict rejected, summary \"Kanban frontend requirements missing\", one finding for index.html, correction_instructions \"Create a complete package-free static Kanban index.html with test_kanban.mjs. Failed criteria: static_entrypoint, frontend_tests, ui_contract\", and idempotency_key \"demo-kanban-review-rejected\". Then stop.".into()
}

pub(super) fn goal_reviewer_instruction() -> String {
    "Read the workspace review payload in Triggering Memory. If verdict is approved, first call proxima_code_code_goal_completion_status with {\"workspace_review_memory\":\"N1\"}. If its child_close is present, call proxima_goal_goal_mark_achieved using exactly child_close.goal, child_close.evidence, and child_close.idempotency_key. If its parent.parent_close is present, call proxima_goal_goal_mark_achieved after the child call using exactly parent.parent_close.goal, parent.parent_close.evidence, and parent.parent_close.idempotency_key. If verdict is rejected, call proxima_code_code_emit_correction_execution_request with {\"workspace_review_memory\":\"N1\",\"target_personality\":\"P1\",\"idempotency_key\":\"demo-signal-match-correction-1\"}. Then stop.".into()
}

pub(super) fn wake_supervisor_instruction(mode: DemoInterventionMode) -> String {
    match mode {
        DemoInterventionMode::Normal => "You are the Wake Supervisor for this E2E demo. Triggering Memory N1 is a core/intervention-requested-v1 Fact. First inspect N1. You may call core_walk_lineage with {\"memory\":\"N1\"} if you need the wake trace and triggering Fact context. If the truncated wake made concrete progress but needs a few more rounds, call core_emit_intervention_decision with {\"intervention_request\":\"N1\",\"decision\":\"continue\",\"grant_rounds\":4,\"rationale\":\"<short evidence-based reason>\",\"idempotency_key\":\"demo-wake_supervisor-continue-N1\"}. If the larger goal already has enough downstream evidence or the wake is likely terminal-but-truncated, call core_emit_intervention_decision with {\"intervention_request\":\"N1\",\"decision\":\"accept_terminal\",\"rationale\":\"<short evidence-based reason>\",\"idempotency_key\":\"demo-wake_supervisor-accept-N1\"}. If the wake appears to be looping, blocked, or making no useful progress, call core_emit_intervention_decision with decision \"stop\" and idempotency_key \"demo-wake_supervisor-stop-N1\". Then stop.".into(),
        DemoInterventionMode::ForceContinue => "You are the Wake Supervisor for the forced continuation demo. Triggering Memory N1 is a core/intervention-requested-v1 Fact for a useful max-round truncation. Always call core_emit_intervention_decision exactly once with {\"intervention_request\":\"N1\",\"decision\":\"continue\",\"grant_rounds\":4,\"rationale\":\"Forced demo: validate persisted graph continuation from the intervention decision event.\",\"idempotency_key\":\"demo-forced-wake-supervisor-continue-N1\"}. Then stop.".into(),
    }
}

pub(super) fn deterministic_checks(
    challenge: DemoChallenge,
    required_child_goal_count: i64,
    achieved: bool,
    goal_graph: &GoalGraphMetrics,
    vision_brief_count: i64,
    diff: &GitDiffStats,
    changed_files: &[String],
) -> BTreeMap<String, bool> {
    let mut checks = BTreeMap::new();
    checks.insert(
        "required_files_exist".into(),
        challenge
            .required_files()
            .iter()
            .all(|file| changed_files.iter().any(|changed| changed == file)),
    );
    checks.insert(
        "no_package_install_required".into(),
        !changed_files.iter().any(|f| {
            matches!(
                f.as_str(),
                "package.json" | "pnpm-lock.yaml" | "package-lock.json" | "yarn.lock"
            )
        }),
    );
    checks.insert("goal_achieved_fact_exists".into(), achieved);
    checks.insert("vision_brief_emitted".into(), vision_brief_count >= 1);
    checks.insert(
        "planner_decomposed_parent_goal".into(),
        goal_graph.child_goal_count >= required_child_goal_count,
    );
    checks.insert(
        "all_child_goals_achieved_before_parent_completion".into(),
        goal_graph.child_goal_count >= required_child_goal_count
            && goal_graph.achieved_child_goal_count == goal_graph.child_goal_count,
    );
    checks.insert(
        "child_execution_requests_observed".into(),
        goal_graph.child_execution_request_count >= required_child_goal_count,
    );
    checks.insert(
        "child_workspace_runs_observed".into(),
        goal_graph.child_workspace_run_count >= required_child_goal_count,
    );
    checks.insert(
        "child_workspace_reviews_observed".into(),
        goal_graph.child_workspace_review_count >= required_child_goal_count,
    );
    checks.insert(
        "deterministic_verifier_evidence_observed".into(),
        goal_graph.verification_evidence_count >= 1,
    );
    checks.insert(
        "final_diff_modifies_only_demo_repo_files".into(),
        changed_files
            .iter()
            .all(|f| !f.starts_with('/') && !f.contains("..")),
    );
    checks.insert(
        "primary_entrypoint_exists".into(),
        changed_files
            .iter()
            .any(|f| f == challenge.required_files()[0]),
    );
    checks.insert(
        "nonempty_diff".into(),
        diff.files_changed > 0 && diff.insertions > 0,
    );
    checks
}

pub(super) fn signal_match_index_html() -> String {
    r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>Signal Match</title>
  <style>
    :root { color-scheme: dark; font-family: Inter, ui-sans-serif, system-ui, sans-serif; background: #101317; color: #f4f7fb; }
    * { box-sizing: border-box; }
    body { min-height: 100vh; margin: 0; display: grid; place-items: center; padding: 20px; }
    main { width: min(720px, 100%); display: grid; gap: 18px; }
    header { display: flex; align-items: end; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
    h1 { margin: 0; font-size: clamp(2rem, 7vw, 4.5rem); line-height: .9; }
    .stats { display: flex; gap: 10px; flex-wrap: wrap; }
    .stat { border: 1px solid #2b3340; border-radius: 8px; padding: 10px 12px; min-width: 92px; background: #171c23; }
    .stat b { display: block; font-size: 1.35rem; }
    .board { display: grid; grid-template-columns: repeat(2, minmax(120px, 1fr)); gap: 12px; aspect-ratio: 1; }
    button { font: inherit; }
    .pad { border: 0; border-radius: 8px; color: white; font-size: clamp(1.8rem, 8vw, 4rem); font-weight: 800; box-shadow: inset 0 -10px rgba(0,0,0,.22); cursor: pointer; transition: transform .08s, filter .12s; }
    .pad:active, .pad.lit { transform: translateY(3px); filter: brightness(1.55) saturate(1.2); }
    [data-pad="0"] { background: #e23d46; }
    [data-pad="1"] { background: #2f9e58; }
    [data-pad="2"] { background: #2774d8; }
    [data-pad="3"] { background: #d89a24; }
    .controls { display: flex; gap: 10px; flex-wrap: wrap; align-items: center; }
    .primary { border: 0; border-radius: 8px; background: #f4f7fb; color: #11151b; padding: 12px 16px; font-weight: 800; cursor: pointer; }
    .status { min-height: 1.5rem; color: #b8c3d2; }
    .game-over { color: #ffb4b4; }
    @media (max-width: 520px) { body { padding: 12px; } .board { gap: 8px; } .stat { flex: 1; } }
  </style>
</head>
<body>
  <main>
    <header>
      <h1>Signal Match</h1>
      <section class="stats" aria-label="Game stats">
        <div class="stat">Level <b id="level">1</b></div>
        <div class="stat">Score <b id="score">0</b></div>
        <div class="stat">Best <b id="best">0</b></div>
      </section>
    </header>
    <section class="board" aria-label="Signal pads">
      <button class="pad" data-pad="0" aria-label="Red pad">Q</button>
      <button class="pad" data-pad="1" aria-label="Green pad">W</button>
      <button class="pad" data-pad="2" aria-label="Blue pad">A</button>
      <button class="pad" data-pad="3" aria-label="Yellow pad">S</button>
    </section>
    <section class="controls">
      <button class="primary" id="restart">Restart</button>
      <div id="status" class="status">Repeat the signal.</div>
    </section>
  </main>
  <script>
    const pads = [...document.querySelectorAll('[data-pad]')];
    const levelEl = document.querySelector('#level');
    const scoreEl = document.querySelector('#score');
    const bestEl = document.querySelector('#best');
    const statusEl = document.querySelector('#status');
    const restart = document.querySelector('#restart');
    const keys = { q: 0, w: 1, a: 2, s: 3 };
    let sequence = [];
    let cursor = 0;
    let accepting = false;
    let score = 0;
    let best = Number(localStorage.getItem('signal-match-best') || 0);
    bestEl.textContent = best;
    const wait = ms => new Promise(resolve => setTimeout(resolve, ms));
    function setStatus(text, over = false) {
      statusEl.textContent = text;
      statusEl.classList.toggle('game-over', over);
    }
    async function flash(index) {
      const pad = pads[index];
      pad.classList.add('lit');
      await wait(260);
      pad.classList.remove('lit');
      await wait(120);
    }
    async function playSequence() {
      accepting = false;
      setStatus('Watch the signal.');
      await wait(350);
      for (const item of sequence) await flash(item);
      cursor = 0;
      accepting = true;
      setStatus('Repeat the signal.');
    }
    function addStep() {
      sequence.push(Math.floor(Math.random() * 4));
      levelEl.textContent = sequence.length;
    }
    async function start() {
      sequence = [];
      cursor = 0;
      score = 0;
      scoreEl.textContent = score;
      addStep();
      await playSequence();
    }
    async function choose(index) {
      if (!accepting) return;
      await flash(index);
      if (sequence[cursor] !== index) {
        accepting = false;
        setStatus('Signal lost. Restart to try again.', true);
        return;
      }
      cursor += 1;
      score += 10;
      scoreEl.textContent = score;
      if (score > best) {
        best = score;
        bestEl.textContent = best;
        localStorage.setItem('signal-match-best', String(best));
      }
      if (cursor === sequence.length) {
        addStep();
        await playSequence();
      }
    }
    pads.forEach(pad => pad.addEventListener('click', () => choose(Number(pad.dataset.pad))));
    window.addEventListener('keydown', event => {
      const pad = keys[event.key.toLowerCase()];
      if (pad !== undefined) choose(pad);
    });
    restart.addEventListener('click', start);
    start();
  </script>
</body>
</html>
"#
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn planner_prompts_use_handle_target_personality_not_uuid() {
        let planner = PersonalityInstanceId::new(Uuid::now_v7());
        let raw = planner.into_inner().to_string();

        for prompt in [
            real_planner_instruction(planner, DemoChallenge::SignalMatch),
            signal_match_planner_instruction(planner),
            todo_cli_planner_instruction(planner),
            kanban_board_planner_instruction(planner),
        ] {
            assert!(!prompt.contains(&raw));
            assert!(prompt.contains("target_personality \"P1\""));
        }
    }
}
