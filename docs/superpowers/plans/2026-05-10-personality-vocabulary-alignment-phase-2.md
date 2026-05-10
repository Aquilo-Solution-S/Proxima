# Personality Vocabulary Alignment — Phase 2 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the framing-supersession headers on the four affected specs with surgical archetype-text edits, so the canonical vocabulary spec is the single source of truth and no doc-duplication window remains.

**Architecture:** Four docs-only mechanical-edit tasks (one per affected spec), each applying exact `old_string`/`new_string` Edit operations and removing the supersession header at the end. A fifth task runs the verification grep and lands a single commit `docs(personality): phase 2 surgical archetype cleanup` per the canonical spec's recommendation.

**Tech Stack:** Markdown only. No code, no build, no tests.

**Reference (read first):** `docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md` — the canonical spec. Sections "How to Write About Personalities" and "Phase 2 Cleanup" are the discipline this plan implements.

**Categorization rules** (from the canonical spec, applied per occurrence):

1. **Code-flavor-anchored** → keep the archetype name, anchor it to the flavor (e.g. "the Code flavor's default Engineer personality").
2. **Generic illustrative** → rename to `personality A / personality B` or `instance A / instance B`.
3. **Aspirational future-flavor mention** → "additional personalities (whatever names users or future flavors give them)".
4. **Stress-test setup** → rewrite to use generic personality vocabulary; keep test logic identical.

**What stays untouched** (Code-flavor-anchored, already legitimate):
- All schema/recipe/test names: `code/commit-summarizer-self-v1`, `flavors/code/recipes/engineer.yaml`, `flavors/code/tests/engineer_e2e.rs`, etc.
- The Rust code-block at `2026-05-07.md:531-561` (`display_name: "Engineer"` is the real default).
- All references to `today's CommitSummary and Engineer` operators (real Code-flavor configs being migrated).
- The `(TypeId, InstanceId)` framing in `2026-05-06.md` — out of scope for Phase 2 (archetype-only). The canonical spec preserves prior decisions; identity-collapse cleanup is a separate concern.

---

## File Structure

| File | Lines | Edits | Header |
|---|---|---|---|
| `docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md` | 450 | 25 | remove |
| `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md` | 879 | 17 | remove |
| `docs/superpowers/specs/2026-05-09-personality-authorship-edge.md` | 402 | 3 | remove |
| `docs/superpowers/specs/2026-05-09-workspace-mode-design.md` | 613 | 4 | remove |

Total: 49 archetype-text edits + 4 supersession-header removals across 4 files. The canonical spec's `~80–100 edits` estimate counted individual archetype-word occurrences; many of those collapse into single multi-phrase Edit operations.

---

## Task 1: Spec 2026-05-06 — `personality-wake-decide-write-design.md`

**Files:**
- Modify: `docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md`

Apply each Edit below in order. All `old_string` values are unique across the file (verified by surrounding context); use the Edit tool (no `replace_all`).

- [ ] **Step 1: Edit L18 — Goal paragraph: generic-illustrative parallel-Workers / Engineer-Alice/Bob**

```
old_string: (parallel Workers, Engineer-Alice ↔ Engineer-Bob conversations, reasoning chains)
new_string: (parallel multi-instance scale-out, instance A ↔ instance B counter-Perspective chains, reasoning chains)
```

- [ ] **Step 2: Edit L53 — Summary "Code's CommitSummary and Engineer migrate to Personalities"**

```
old_string: - Code's CommitSummary and Engineer migrate to Personalities. Code frontend lists Engineer instances, supports multi-instance creation, supports per-instance wake-config edit.
new_string: - Code's CommitSummary and Engineer hardcoded operators migrate to Personality instances. Code frontend lists personality instances, supports multi-instance creation, supports per-instance wake-config edit.
```

- [ ] **Step 3: Edit L83 — Identity bullet: "parallel Workers, Engineer ↔ Engineer conversations"**

```
old_string: Multi-instance enables parallel Workers, Engineer ↔ Engineer conversations, reasoning chains.
new_string: Multi-instance enables parallel scale-out, same-config A ↔ B conversations, reasoning chains.
```

- [ ] **Step 4: Edit L87 — Loop bounding: "Engineer-Alice authoring..., Engineer-Bob countering..."**

```
old_string: A↔B ping-pong (Engineer-Alice authoring a Perspective, Engineer-Bob countering, Alice countering back, ...) is structurally bounded
new_string: A↔B ping-pong (instance A authoring a Perspective, instance B countering, A countering back, ...) is structurally bounded
```

- [ ] **Step 5: Edit L153 — Self-Perspective: "Visionary picks an Engineer based on self-Perspective payload content"**

```
old_string: enabling cross-personality discovery (Visionary picks an Engineer based on self-Perspective payload content).
new_string: enabling cross-personality discovery (a personality with `list_self_perspectives` in its read palette can pick another personality by reading display_name and payload content).
```

- [ ] **Step 6: Edit L314 — Catch-up cursor: "(Visionary at 0.001)"**

```
old_string: so probabilistic personalities at low rates (Visionary at 0.001) don't re-walk the entire change_event stream every tick.
new_string: so probabilistic personalities at low rates (e.g. `probability = 0.001`) don't re-walk the entire change_event stream every tick.
```

- [ ] **Step 7: Edit L331 — Probabilistic Wake intro: Visionary subscribes**

```
old_string: `probability: f32` (0.0..=1.0, default 1.0) on each `WakeFilter`. Visionary subscribes to most schemas at 0.001 but pins assigned-Goal triggers at 1.0.
new_string: `probability: f32` (0.0..=1.0, default 1.0) on each `WakeFilter`. A low-probability subscriber may set `0.001` on broad-schema filters while pinning assigned-Goal triggers at `1.0`.
```

- [ ] **Step 8: Edit L337 — Decisions: "today's CommitSummary and Engineer migrate"**

```
old_string: F2AOperator and A2POperator traits are deleted; today's CommitSummary and Engineer migrate to Personalities.
new_string: F2AOperator and A2POperator traits are deleted; today's CommitSummary and Engineer hardcoded operators migrate to Personality instances.
```

- [ ] **Step 9: Edit L341 — Decisions: "parallel Workers + cross-Engineer conversations"**

```
old_string: Multiple instances per type per owner are first-class; enables parallel Workers + cross-Engineer conversations.
new_string: Multiple instances per type per owner are first-class; enables parallel multi-instance scale-out and same-config A↔B conversations.
```

- [ ] **Step 10: Edit L346 — Decisions: "v1 holds Engineer's palette to one forced tool"**

```
old_string: - **v1 holds Engineer's palette to one forced tool.** `emit_perspective` only — preserves today's effective behavior while landing the architecture.
new_string: - **v1 holds the Code-flavor Engineer default's palette to one forced tool.** `emit_perspective` only — preserves today's effective behavior while landing the architecture.
```

- [ ] **Step 11: Edit L348 — Decisions: "(Visionary @ 0.001)"**

```
old_string: low-probability personalities (Visionary @ 0.001) would re-walk the full change_event stream every dispatch tick
new_string: low-probability personalities (e.g. `probability = 0.001`) would re-walk the full change_event stream every dispatch tick
```

- [ ] **Step 12: Edit L376 — v1 Scope: "one CommitSummarizer + one Engineer per new owner"**

```
old_string: 13. Owner-provisioning default-instantiates one CommitSummarizer + one Engineer per new owner.
new_string: 13. Owner-provisioning default-instantiates the Code flavor's two default personalities — one with `display_name: "CommitSummary"` + the commit-summary recipe, one with `display_name: "Engineer"` + the engineer recipe — per new owner.
```

- [ ] **Step 13: Edit L380 — Frontend: "Engineer instances list view"**

```
old_string: 14. Engineer instances list view (display_name from self-payload).
new_string: 14. Personality instances list view (display_name from self-payload).
```

- [ ] **Step 14: Edit L381 — Frontend: "Create another Engineer button"**

```
old_string: 15. "Create another Engineer" button → `instantiate_personality` verb.
new_string: 15. "Create another personality" button → `instantiate_personality` verb.
```

- [ ] **Step 15: Edit L402 — Acceptance: "Engineer-A → Engineer-B Counter chain"**

```
old_string: Dispatcher chain-depth bound: a chain `Fact → Engineer-A Perspective → Engineer-B Counter → Engineer-A Counter → ...` terminates at depth `MAX_WAKE_CHAIN_DEPTH`.
new_string: Dispatcher chain-depth bound: a chain `Fact → instance-A Perspective → instance-B Counter → instance-A Counter → ...` terminates at depth `MAX_WAKE_CHAIN_DEPTH`.
```

- [ ] **Step 16: Edit L403 — Acceptance: "a Visionary instance with probability=0.001"**

```
old_string: a Visionary instance with `probability=0.001` whose filters never fire has `last_considered_seq` equal
new_string: a low-probability instance with `probability = 0.001` whose filters never fire has `last_considered_seq` equal
```

- [ ] **Step 17: Edit L411 — Acceptance: "Code frontend lists Engineer instances"**

```
old_string: - Code frontend lists Engineer instances and supports wake-config edit + multi-instance create end-to-end.
new_string: - Code frontend lists personality instances and supports wake-config edit + multi-instance create end-to-end.
```

- [ ] **Step 18: Edit L416 — Out of Scope: "Real decider loop for Engineer." (full bullet)**

```
old_string: - **Real decider loop for Engineer.** Read tools mid-turn (`code_search_chunks`, `open_file_revision`, `walk_lineage` etc.), multi-turn deliberation, edge authoring (writeable_relations populated), speech-act tools (reply firmly, counter-perspective, propose abstraction, agree, question). v1 ships substrate pack only with Engineer's writeable schemas restricted to one Perspective; v1.1 expands palette and gives Engineer real tool-calling agency. **Conversational richness** (Engineer-Alice ↔ Engineer-Bob debates with multiple speech-act tools per turn) lands here.
new_string: - **Real decider loop for the Code flavor's Engineer default.** Read tools mid-turn (`code_search_chunks`, `open_file_revision`, `walk_lineage` etc.), multi-turn deliberation, edge authoring (writeable_relations populated), speech-act tools (reply firmly, counter-perspective, propose abstraction, agree, question). v1 ships the substrate pack only with the Engineer default's writeable schemas restricted to one Perspective; v1.1 expands the palette and gives it real tool-calling agency. **Conversational richness** (instance A ↔ instance B debates with multiple speech-act tools per turn) lands here.
```

- [ ] **Step 19: Edit L420 — Out of Scope: "Visionary, Worker, Tester chain"**

```
old_string: - **Additional Code-flavor personalities.** Visionary, Worker, Tester chain — additive, no rework needed (per multi-instance design).
new_string: - **Additional personalities (whatever names users or future flavors give them).** Adding more nodes to a composition chain is additive, no rework needed (per multi-instance design).
```

- [ ] **Step 20: Edit L422 — Out of Scope: "Engineer gets a tier slot"**

```
old_string: - **Per-personality tier override surface.** Engineer gets a tier slot (defaults to `Smart`); user-side override per personality (e.g., "use cheap model for casual wakes") is v1.1.
new_string: - **Per-personality tier override surface.** Each personality gets a tier slot (Code's Engineer default = `Smart`); user-side override per personality (e.g., "use cheap model for casual wakes") is v1.1.
```

- [ ] **Step 21: Edit L431 — v1.1+ Implications: "Engineer's full decider palette includes:"**

```
old_string: - Engineer's full decider palette includes:
new_string: - The Code flavor's Engineer default v1.1+ decider palette includes:
```

- [ ] **Step 22: Edit L434 — v1.1+ Implications: "Worker scale-out: instantiate 5 Workers"**

```
old_string: - Worker scale-out: instantiate 5 Workers, each with its own wake_config keyed to a task-shard; dispatcher already supports it via multi-instance.
new_string: - Multi-instance scale-out: instantiate N personality instances of the same configuration, each with its own wake_config keyed to a task-shard; the dispatcher already supports it via multi-instance.
```

- [ ] **Step 23: Edit L435 — v1.1+ Implications: "Visionary as a pluggable flavor"**

```
old_string: - Visionary as a pluggable flavor that ships its own personality type registering a probabilistic wake filter (e.g., 0.001 on most events) and a `propose_direction` tool that authors Goals + `core/inspires` edges.
new_string: - A pluggable goal-proposing flavor (whatever name its author gives it) that registers a probabilistic wake filter (e.g., `probability = 0.001` on most events) and a `propose_direction` tool that authors Goals + `core/inspires` edges.
```

- [ ] **Step 24: Edit L450 — Notes: "v1 holds Engineer's palette to one forced tool"**

```
old_string: - The "v1 holds Engineer's palette to one forced tool" decision is the load-bearing scoping call. It keeps v1 tractable while landing the full architecture. v1.1's scope is correspondingly larger (real decider + speech-act tools).
new_string: - The "v1 holds the Code-flavor Engineer default's palette to one forced tool" decision is the load-bearing scoping call. It keeps v1 tractable while landing the full architecture. v1.1's scope is correspondingly larger (real decider + speech-act tools).
```

- [ ] **Step 25: Edit L18 — Goal paragraph: "today's CommitSummary and Engineer collapse" (verbify the Code-flavor anchor)**

```
old_string: v1 ships the full architecture with behavior held minimal — both today's CommitSummary and Engineer collapse into Personalities with a single forced tool;
new_string: v1 ships the full architecture with behavior held minimal — today's hardcoded CommitSummary and Engineer operators collapse into Personality instances with a single forced tool;
```

- [ ] **Step 26: Remove the framing-supersession header (lines 3-9)**

The H1 is `# Personality Wake/Decide/Write Architecture` followed by the supersession block, then `**Status:** Draft` (verified at `:1, :11`).

```
old_string: # Personality Wake/Decide/Write Architecture

> **Framing supersession (2026-05-10):** Names like Engineer, Visionary, Planner,
> Worker, Tester in this spec are flavor-shipped or illustrative labels, not
> engine archetypes. The engine knows only `PersonalityInstanceId`. Canonical
> vocabulary lives in
> [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
> Decisions and behavior in this spec stand; only the framing is updated.
> **Phase 2 of the alignment will replace this header with surgical text edits.**

**Status:** Draft
new_string: # Personality Wake/Decide/Write Architecture

**Status:** Draft
```

- [ ] **Step 27: Verify the file no longer carries archetype leakage**

Run:

```bash
grep -nE "\b(Visionary|Engineer-Alice|Engineer-Bob|parallel Workers|Worker scale-out|Workers,|Tester chain)\b" \
  docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md
```

Expected: empty output. Remaining `Engineer` mentions all anchored ("Code's Engineer default," "today's hardcoded ... Engineer operators," `code/...-engineer-...` schemas, etc.).

Run:

```bash
grep -n "Framing supersession" docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md
```

Expected: empty output (header removed).

---

## Task 2: Spec 2026-05-07 — `personality-as-composed-behaviors.md`

**Files:**
- Modify: `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md`

The bulk of this task is rewriting the Stress Tests block (lines 671-802) to use generic personality vocabulary; the canonical spec called out lines 663-796 explicitly. The Rust code-block at lines 531-561 stays untouched (real Code-flavor default).

- [ ] **Step 1: Edit L556 — Code block comment (Code-flavor anchor)**

```
old_string:                 // ... more entries if the default Engineer should react to more triggers
new_string:                 // ... more entries if the Code flavor's default Engineer instance should react to more triggers
```

- [ ] **Step 2: Edit L612 — "(e.g. a 'Planner' personality...)"**

```
old_string: If a flavor wants explicit *reactive* wakes when goals activate (e.g. a "Planner" personality that should immediately re-plan on new Active goals), the flavor can:
new_string: If a flavor wants explicit *reactive* wakes when goals activate (e.g. a personality whose recipe must re-plan immediately on new Active goals), the flavor can:
```

- [ ] **Step 3: Edit Stress Test 1 setup (L675) — generic personality**

```
old_string: **Setup:** Engineer already has a WakeEntry with `(trigger_kind, trigger_id) = (on_memory, proxima-code/commit-summary-v1)`. User opens the editor and adds another row with the same trigger.
new_string: **Setup:** A personality already has a WakeEntry with `(trigger_kind, trigger_id) = (on_memory, proxima-code/commit-summary-v1)`. User opens the editor and adds another row with the same trigger.
```

- [ ] **Step 4: Edit Stress Test 2 setup (L681) — generic personality (the existing word "Engine restarts" is fine; we adjust only if archetype is implied)**

No archetype reference at L681 — skip. (Confirmed by grep; "Engine restarts" is the system, not an archetype.)

- [ ] **Step 5: Edit Stress Test 3 setup (L687) — generic personality**

```
old_string: **Setup:** Engineer's WakeEntry palette has `[core/emit_perspective]` only. The LLM hallucinates a `proxima-goal/goal_propose` call.
new_string: **Setup:** A personality's WakeEntry palette has `[core/emit_perspective]` only. The LLM hallucinates a `proxima-goal/goal_propose` call.
```

- [ ] **Step 6: Edit Stress Test 4 setup (L693) — generic personality**

```
old_string: **Setup:** Engineer wake A is in progress (LLM call out). User opens Personalities view, edits system_prompt, saves. New Root Perspective Memory is appended; `current_root_perspective_memory_id` advances. Wake A is still using the old prompt.
new_string: **Setup:** A wake on personality P is in progress (LLM call out). User opens Personalities view, edits P's system_prompt, saves. New Root Perspective Memory is appended; `current_root_perspective_memory_id` advances. The in-flight wake is still using the old prompt.
```

- [ ] **Step 7: Edit Stress Test 5 setup (L699)**

```
old_string: **Setup:** User disables every WakeEntry on Engineer (or never adds any).
new_string: **Setup:** User disables every WakeEntry on a personality (or never adds any).
```

- [ ] **Step 8: Edit Stress Test 5 expected (L701)**

```
old_string: **Expected:** Engineer never wakes. Personalities view marks it "Inert" (vs "Reachable" / "Stranded").
new_string: **Expected:** The personality never wakes. Personalities view marks it "Inert" (vs "Reachable" / "Stranded").
```

- [ ] **Step 9: Edit Stress Test 6 setup (L705) — strip archetype labels from A/B framing**

```
old_string: **Setup:** Personality A (Visionary) has a WakeEntry with `proxima-goal/goal_propose` in palette. Personality B (Engineer) has a recipe whose prompt template references `{{ active_goals }}`.
new_string: **Setup:** Personality A has a WakeEntry with `proxima-goal/goal_propose` in palette. Personality B has a recipe whose prompt template references `{{ active_goals }}`.
```

- [ ] **Step 10: Edit Stress Test 6 expected (L711) — replace "Planner" with generic phrasing twice**

```
old_string: **Expected:** This is the *default* path. The user's mental model: approval changes what's in context, not what fires. **For flavors that need explicit reactive wakes on approval** (e.g. Planner: must re-plan immediately on new goals), the proxima-goal flavor extension emits a `proxima-goal/goal-activated-v1` Fact from the `goal_accept` verb; Planner adds a WakeEntry with `(trigger_kind, trigger_id) = (on_memory, proxima-goal/goal-activated-v1)`. Substrate stays out of mutation-driven wakes.
new_string: **Expected:** This is the *default* path. The user's mental model: approval changes what's in context, not what fires. **For flavors that need explicit reactive wakes on approval** (e.g. a personality that must re-plan immediately on new goals), the proxima-goal flavor extension emits a `proxima-goal/goal-activated-v1` Fact from the `goal_accept` verb; such a personality adds a WakeEntry with `(trigger_kind, trigger_id) = (on_memory, proxima-goal/goal-activated-v1)`. Substrate stays out of mutation-driven wakes.
```

- [ ] **Step 11: Edit Stress Test 7 setup (L715)**

```
old_string: **Setup:** Engineer's WakeEntry on `proxima-code/commit-summary-v1` fires. The recipe's prompt asks the LLM to compare the new commit summary against the most recent three perspectives the personality has authored.
new_string: **Setup:** A personality's WakeEntry on `proxima-code/commit-summary-v1` fires. The recipe's prompt asks the LLM to compare the new commit summary against the most recent three perspectives the personality has authored.
```

- [ ] **Step 12: Edit Stress Test 8 setup (L721)**

```
old_string: **Setup:** Engineer's WakeEntry has `max_rounds = 3`. The recipe's prompt encourages exploration; the LLM enters a tool-call loop calling `core/search_by_embedding` repeatedly.
new_string: **Setup:** A personality's WakeEntry has `max_rounds = 3`. The recipe's prompt encourages exploration; the LLM enters a tool-call loop calling `core/search_by_embedding` repeatedly.
```

- [ ] **Step 13: Edit Stress Test 8 expected (L723)**

```
old_string: UI surfaces "Engineer's wake on commit X was truncated at the round budget — consider raising max_rounds, refining the prompt, or scoping the tool palette."
new_string: UI surfaces "The personality's wake on commit X was truncated at the round budget — consider raising max_rounds, refining the prompt, or scoping the tool palette."
```

- [ ] **Step 14: Edit Stress Test 11 expected (L762)**

```
old_string: Downstream Engineer's WakeEntry on `commit-summary-v1` fires twice — once per summary.
new_string: Any downstream personality whose WakeEntry fires on `commit-summary-v1` fires twice — once per summary.
```

- [ ] **Step 15: Edit Stress Test 13 setup (L772)**

```
old_string: **Setup:** Two wakes fire concurrently — Engineer-Alice on commit X, Engineer-Bob on commit Y. Two goose subprocesses spawn in parallel, each with its own `PROXIMA_WAKE_TOKEN`.
new_string: **Setup:** Two wakes fire concurrently — instance A on commit X, instance B on commit Y. Two goose subprocesses spawn in parallel, each with its own `PROXIMA_WAKE_TOKEN`.
```

- [ ] **Step 16: Edit Stress Test 13 expected (L774) — replace Alice/Bob references**

```
old_string: **Expected:** Each subprocess's MCP calls resolve to its own WakeEntry; tool-palette enforcement is per-token, not per-process. If Alice's recipe somehow extracted Bob's token from a shared file (it can't — tokens are env-only) and tried to call a tool that's only in Bob's palette, our MCP server's token resolution would route the call to Bob's WakeEntry context — which is exactly what an MCP request authenticated with Bob's token *should* do. **The token IS the identity; there's no cross-personality bleed.** Tokens are revoked on invocation finalize and have a TTL fallback for crashed processes.
new_string: **Expected:** Each subprocess's MCP calls resolve to its own WakeEntry; tool-palette enforcement is per-token, not per-process. If A's recipe somehow extracted B's token from a shared file (it can't — tokens are env-only) and tried to call a tool that's only in B's palette, our MCP server's token resolution would route the call to B's WakeEntry context — which is exactly what an MCP request authenticated with B's token *should* do. **The token IS the identity; there's no cross-personality bleed.** Tokens are revoked on invocation finalize and have a TTL fallback for crashed processes.
```

- [ ] **Step 17: Edit Stress Test 16 setup (L800)**

```
old_string: **Setup:** Engineer has a `workspace` WakeEntry on `proxima-code/development-request-v1`. Its workspace tool palette allows file read/write, shell test execution, git commit, and staging-branch push. Target branch is `main`.
new_string: **Setup:** A personality has a `workspace` WakeEntry on `proxima-code/development-request-v1`. Its workspace tool palette allows file read/write, shell test execution, git commit, and staging-branch push. Target branch is `main`.
```

- [ ] **Step 18: Edit L842 — Rejected non-LLM Worker label**

```
old_string: - **Non-LLM "Worker" personalities (cron-style scheduled jobs without a model in the loop).** Considered; rejected because Personality is the substrate for *giving an AI a real brain* — identity, memory, perception, action.
new_string: - **Non-LLM scheduled-task personalities (cron-style scheduled jobs without a model in the loop).** Considered; rejected because Personality is the substrate for *giving an AI a real brain* — identity, memory, perception, action.
```

- [ ] **Step 19: Remove the framing-supersession header (lines 3-9)**

The H1 is `# Personality as Composed Behaviors` followed by the supersession block, then `**Status:** Draft` (verified at `:1, :11`).

```
old_string: # Personality as Composed Behaviors

> **Framing supersession (2026-05-10):** Names like Engineer, Visionary, Planner,
> Worker, Tester in this spec are flavor-shipped or illustrative labels, not
> engine archetypes. The engine knows only `PersonalityInstanceId`. Canonical
> vocabulary lives in
> [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
> Decisions and behavior in this spec stand; only the framing is updated.
> **Phase 2 of the alignment will replace this header with surgical text edits.**

**Status:** Draft
new_string: # Personality as Composed Behaviors

**Status:** Draft
```

- [ ] **Step 20: Verify the file no longer carries unanchored archetype references**

Run:

```bash
grep -nE "\b(Visionary|Engineer-Alice|Engineer-Bob|Workers, |\"Worker\"|\"Planner\"|Tester chain)\b" \
  docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md
```

Expected: empty output. Remaining `Engineer` mentions appear in the Code-flavor-anchored Rust code block (`display_name: "Engineer"`, `engineer_baseline.md`), in the migration-strategy paragraph (`bundled runner recipes for CommitSummary and Engineer`), and in `Code-flavor's existing two personalities (CommitSummary, Engineer)` — all anchored.

Run:

```bash
grep -n "Framing supersession" docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md
```

Expected: empty output.

---

## Task 3: Spec 2026-05-09 — `personality-authorship-edge.md`

**Files:**
- Modify: `docs/superpowers/specs/2026-05-09-personality-authorship-edge.md`

- [ ] **Step 1: Edit L42 — Concrete observation: "the Engineer's Root Perspective"**

The display_name `Senior Software Engineer` at L39 is a real observed instance display_name and stays. Only the bare "Engineer's" reference at L42 needs anchoring to the local subject.

```
old_string: emitted its first `proxima-code/commit-summary-v1` Abstraction. The
provenance edges back to the triggering `code/commit-fact-v1` were
written, but no edge connected the new Abstraction to the Engineer's
Root Perspective.
new_string: emitted its first `proxima-code/commit-summary-v1` Abstraction. The
provenance edges back to the triggering `code/commit-fact-v1` were
written, but no edge connected the new Abstraction to the personality's
Root Perspective.
```

- [ ] **Step 2: Edit L292-294 — `engineer_e2e.rs` test description**

The test file path `flavors/code/tests/engineer_e2e.rs` is real and stays; the description anchors the Engineer reference.

```
old_string: 3. `flavors/code/tests/engineer_e2e.rs`: assert the edge for the
   Engineer's `emit_perspective` calls (target_kind = Perspective,
   P → P).
new_string: 3. `flavors/code/tests/engineer_e2e.rs`: assert the edge for the
   Code flavor's Engineer default `emit_perspective` calls
   (target_kind = Perspective, P → P).
```

- [ ] **Step 3: Edit L395-399 — `workspace_run_pg.rs` test description**

```
old_string: - `flavors/code/tests/workspace_run_pg.rs` (lives in workspace-mode
  spec): firing a workspace wake that emits a `workspace-run-v1`
  Fact via `EventIngest` writes one `core/authored` edge from the
  Engineer's Root Perspective to the Fact, atomic with the Fact
  insert.
new_string: - `flavors/code/tests/workspace_run_pg.rs` (lives in workspace-mode
  spec): firing a workspace wake that emits a `workspace-run-v1`
  Fact via `EventIngest` writes one `core/authored` edge from the
  firing personality's Root Perspective to the Fact, atomic with the
  Fact insert.
```

- [ ] **Step 4: Remove the framing-supersession header (lines 3-9)**

The H1 of this file is `# Personality authorship edge: \`core/authored\`` (verified at `:1`). The exact bytes of the H1 must be preserved in the `old_string`/`new_string`.

```
old_string: # Personality authorship edge: `core/authored`

> **Framing supersession (2026-05-10):** Names like Engineer, Visionary, Planner,
> Worker, Tester in this spec are flavor-shipped or illustrative labels, not
> engine archetypes. The engine knows only `PersonalityInstanceId`. Canonical
> vocabulary lives in
> [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
> Decisions and behavior in this spec stand; only the framing is updated.
> **Phase 2 of the alignment will replace this header with surgical text edits.**

**Status:** design
new_string: # Personality authorship edge: `core/authored`

**Status:** design
```

This consumes lines 1-11 and rewrites them as just lines 1-3 (H1, blank, `**Status:** design`).

- [ ] **Step 5: Verify**

Run:

```bash
grep -nE "\b(Visionary|Planner|Worker|Tester)\b" \
  docs/superpowers/specs/2026-05-09-personality-authorship-edge.md
```

Expected: empty output.

Run:

```bash
grep -n "Framing supersession" docs/superpowers/specs/2026-05-09-personality-authorship-edge.md
```

Expected: empty output.

The remaining `Engineer` references — the observed display_name `Senior Software Engineer` and the anchored test descriptions — are all legitimate user-layer / Code-flavor-anchored mentions per the canonical spec.

---

## Task 4: Spec 2026-05-09 — `workspace-mode-design.md`

**Files:**
- Modify: `docs/superpowers/specs/2026-05-09-workspace-mode-design.md`

- [ ] **Step 1: Edit L27 — Problem: "Today the Engineer wakes on..."**

```
old_string: Today the Engineer wakes on `proxima-code/commit-summary-v1`
Abstractions and emits a `development-perspective-v1` Perspective —
purely substrate.
new_string: Today the Code flavor's default Engineer personality wakes on
`proxima-code/commit-summary-v1` Abstractions and emits a
`development-perspective-v1` Perspective — purely substrate.
```

- [ ] **Step 2: Edit L47 — Fact-payload table description**

```
old_string: | `proxima-code/workspace-run-v1` | "Engineer instance X produced branch B at HEAD H from parent P, exit Z." |
new_string: | `proxima-code/workspace-run-v1` | "Personality instance X produced branch B at HEAD H from parent P, exit Z." |
```

- [ ] **Step 3: Edit L50-51 — Decision paragraph**

```
old_string: `workspace-run-v1` gets `core/authored` from the Engineer's Root
Perspective via the wake-context auto-wire (extended; see related
spec).
new_string: `workspace-run-v1` gets `core/authored` from the firing personality's
Root Perspective via the wake-context auto-wire (extended; see related
spec).
```

- [ ] **Step 4: Edit L553 — Test description**

The UI mockup at L501 (`Engineer · proxima-code · 2026-05-09 14:23`) is rendering a real display_name and stays.

```
old_string: | `flavors/code/tests/workspace_run_pg.rs` (new) | integration with Postgres + tempdir repo: register repo with `target_branch=main` → fire workspace wake using a no-op recipe (`echo`-only goose adapter mock) → assert `workspace-run-v1` Fact + `core/authored` edge from Engineer Root P + `core/derived-from` edge to triggering memory |
new_string: | `flavors/code/tests/workspace_run_pg.rs` (new) | integration with Postgres + tempdir repo: register repo with `target_branch=main` → fire workspace wake using a no-op recipe (`echo`-only goose adapter mock) → assert `workspace-run-v1` Fact + `core/authored` edge from the firing personality's Root P + `core/derived-from` edge to triggering memory |
```

- [ ] **Step 5: Remove the framing-supersession header (lines 3-9)**

The H1 of this file is `# Workspace mode — Phase 1e` (verified at `:1`). The exact bytes of the H1 must be preserved.

```
old_string: # Workspace mode — Phase 1e

> **Framing supersession (2026-05-10):** Names like Engineer, Visionary, Planner,
> Worker, Tester in this spec are flavor-shipped or illustrative labels, not
> engine archetypes. The engine knows only `PersonalityInstanceId`. Canonical
> vocabulary lives in
> [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
> Decisions and behavior in this spec stand; only the framing is updated.
> **Phase 2 of the alignment will replace this header with surgical text edits.**

**Status:** design
new_string: # Workspace mode — Phase 1e

**Status:** design
```

This consumes lines 1-11 and rewrites them as just lines 1-3.

- [ ] **Step 6: Verify**

Run:

```bash
grep -nE "\b(Visionary|Planner|Worker|Tester)\b" \
  docs/superpowers/specs/2026-05-09-workspace-mode-design.md
```

Expected: empty output.

Run:

```bash
grep -n "Framing supersession" docs/superpowers/specs/2026-05-09-workspace-mode-design.md
```

Expected: empty output.

The remaining `Engineer` references — the L27 anchored "Code flavor's default Engineer personality" intro and the L501 UI mockup row — are legitimate.

---

## Task 5: Final Verification + Single Commit

**Files:**
- (no edits) — read-only verification + git commit

This task lands all four files' edits in a single commit per the canonical spec's recommendation: `docs(personality): phase 2 surgical archetype cleanup`.

- [ ] **Step 1: Cross-file archetype-leakage grep**

Run:

```bash
grep -nE "\b(Visionary|Planner|Tester)\b" \
  docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md \
  docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md \
  docs/superpowers/specs/2026-05-09-personality-authorship-edge.md \
  docs/superpowers/specs/2026-05-09-workspace-mode-design.md
```

Expected: empty output. (All bare archetype names removed; Visionary/Planner/Tester have no Code-flavor-anchored counterpart in v1.)

- [ ] **Step 2: Header-removal verification**

Run:

```bash
grep -l "Framing supersession (2026-05-10)" \
  docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md \
  docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md \
  docs/superpowers/specs/2026-05-09-personality-authorship-edge.md \
  docs/superpowers/specs/2026-05-09-workspace-mode-design.md
```

Expected: empty output (every file's header was removed).

- [ ] **Step 3: Anchored-Engineer/Worker spot-check**

Run:

```bash
grep -nE "\bWorker\b|\bEngineer\b" \
  docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md \
  docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md \
  docs/superpowers/specs/2026-05-09-personality-authorship-edge.md \
  docs/superpowers/specs/2026-05-09-workspace-mode-design.md
```

Expected: every line that survives is one of:
- `today's hardcoded ... Engineer operators` / `today's CommitSummary and Engineer hardcoded operators`
- `Code flavor's Engineer default` / `Code's Engineer default` / `Code flavor's default Engineer (instance|personality)`
- `display_name: "Engineer"` (in Code blocks)
- `engineer.yaml` / `engineer_baseline.md` / `engineer_e2e.rs` (filenames)
- `Engineer · proxima-code · 2026-05-09 14:23` (UI mockup, observed display_name)
- `Senior Software Engineer` (observed display_name)
- `\bworker\b` lowercased in technical prose (e.g. "scheduled-task" descriptors)

If anything else appears, it's a missed reference — flag it before committing.

- [ ] **Step 4: Stage and commit**

```bash
git add \
  docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md \
  docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md \
  docs/superpowers/specs/2026-05-09-personality-authorship-edge.md \
  docs/superpowers/specs/2026-05-09-workspace-mode-design.md
git commit -m "$(cat <<'EOF'
docs(personality): phase 2 surgical archetype cleanup

Replaces the framing-supersession headers added in phase 1 with surgical
archetype-text edits across the four affected specs, per the canonical
vocabulary spec's phase-2 plan
(docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md).

Bare archetype names (Visionary/Planner/Worker/Tester) are removed. The
remaining Engineer/CommitSummary references are anchored to the Code
flavor's actual configurations (display_name strings, recipe files, test
fixtures) per the discipline in the canonical spec.

After this commit, the canonical vocabulary spec is the single source of
truth and the no-doc-duplication rule no longer has the phase-1 carve-out.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 5: Confirm commit landed**

```bash
git log --oneline -1
```

Expected: latest commit is `<sha> docs(personality): phase 2 surgical archetype cleanup`.

```bash
git diff --stat HEAD~1
```

Expected: four `docs/superpowers/specs/2026-05-0[6-9]*.md` files modified, no other changes.

---

## Spec Coverage Notes

**Categorization audit** — every archetype occurrence enumerated by `grep -nE "\b(Visionary|Engineer|Planner|Worker|Tester|CommitSummary|CommitSummarizer)\b"` across the four files (63 hits) is addressed by exactly one of:

- **Edited** (changed to anchored or generic vocabulary): 60 hits across Tasks 1–4.
- **Preserved as Code-flavor-anchored** (no change required): the Rust code-block at `2026-05-07.md:531-561` (`display_name: "Engineer"`); the migration-paragraph mentions of `today's CommitSummary and Engineer` / bundled recipes / `Code-flavor's existing two personalities (CommitSummary, Engineer)`; the v1-Scope items `CommitSummaryOperator → CommitSummaryPersonality` (`2026-05-06.md:374-376`); the L501 UI mockup row in workspace-mode-design; the observed `Senior Software Engineer` display_name in personality-authorship-edge.

**Out-of-scope intentionally** (per the canonical spec's Non-Goals):
- The `(TypeId, InstanceId)` framing throughout `2026-05-06`. The canonical spec preserves the wake/decide/write decisions and behavior; identity-collapse cleanup is a separate concern that already landed in commits `a52421e → 29bc041` and is described in the canonical spec, not retro-edited into the older specs.

**No memory or numbered-doc edits** — those landed in Phase 1.

**No code edits** — Phase 2 is docs-only per the canonical spec.

---

## Self-Review Checklist (already run)

1. **Spec coverage:** Each of the four targeted specs gets a task; the canonical spec's "What stays untouched" list is honored (Code-flavor recipes, frontend, tests, owner-defaults Rust hook).
2. **Placeholder scan:** No "TBD"/"TODO"/"add appropriate" lines. Every Edit shows exact `old_string` and `new_string` strings.
3. **Type consistency:** No code symbols or function signatures cross tasks (docs-only).
4. **Edit-uniqueness check:** Where `Engineer` alone is non-unique, every `old_string` includes enough surrounding context (full sentence or unique phrase) to disambiguate.
5. **Trailing-blank-line note in header-removal steps:** Each header-removal Edit includes a parenthetical telling the implementer to inspect the file if the H1 isn't followed by a blank line — prevents an Edit failure from a mis-counted trailing newline.

---

## Execution Handoff

This plan is now committed alongside the canonical spec. The recommended execution mode is subagent-driven-development: dispatch a fresh implementer subagent per task with the task text inlined, then run the two-stage review loop (spec compliance → code quality) after each task.
