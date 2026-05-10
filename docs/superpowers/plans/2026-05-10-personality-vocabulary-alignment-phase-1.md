# Personality Vocabulary Alignment — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the vocabulary-alignment changes prescribed by `docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md` — framing-supersession headers on four affected specs, surgical edits on two numbered docs, six memory rewrites/spot-edits, and one `MEMORY.md` index update — in a single docs-only commit on `road-to-v1`.

**Architecture:** Docs-only change. No engine code, no tests. The four spec headers + two numbered-doc edits land in one git commit. Memory files live outside the repo at `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/` and are NOT part of the commit; they are rewritten in place. A final grep verification confirms every affected spec carries the framing header.

**Tech Stack:** Markdown only.

---

## Spec Coverage

This plan implements Phase 1 of the spec. Phase 2 (surgical archetype cleanup of the four old specs to remove framing-supersession headers and replace archetype prose with generic vocabulary) is explicitly **out of scope here** and will be a follow-up plan.

**Memory files the spec listed but this plan omits.** The spec's Phase 1 work breakdown listed `project_first_flavor.md`, `project_typed_goals.md`, `project_decider_in_flavor.md`, and `project_personality_as_composed_behaviors.md` as candidate spot-edit targets ("most of their content is already correct"). A direct grep against current memory files (`grep -lE "\b(Visionary|Engineer|Planner|Worker|Tester)\b" memory/*.md`) returns zero archetype mentions in those four files, so they are no-ops in this plan. The plan instead covers `project_goals_vs_wake_config.md` and `project_personality_self_perspective.md`, which the same grep flagged as containing archetype prose the spec did not anticipate. Net memory scope: 6 files modified + `MEMORY.md` index update (vs. the spec's notional 8 + index).

## File Structure

**Files modified in repo (will be committed):**

| Path | Change |
|---|---|
| `docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md` | Insert framing-supersession header after H1 |
| `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md` | Insert framing-supersession header after H1 |
| `docs/superpowers/specs/2026-05-09-personality-authorship-edge.md` | Insert framing-supersession header after H1 |
| `docs/superpowers/specs/2026-05-09-workspace-mode-design.md` | Insert framing-supersession header after H1 |
| `docs/02-memory.md` | Surgical edit: anchor "Stoic Visionary" / "Workhorse Programmer" as hypothetical labels |
| `docs/10-configuration.md` | Surgical edit: clarifying note that `proxima-code/engineer-v1` is a flavor-side identifier |

**Files modified in memory (NOT committed; live at `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/`):**

| File | Change |
|---|---|
| `project_personality_multi_instance.md` | Full rewrite (wrong identity claim + archetype examples) |
| `project_spinning_wheel_architecture.md` | Full rewrite (wrong architecture framing) |
| `project_code_flavor_demo_config.md` | Full rewrite (Engineer-as-archetype framing) |
| `project_personality_decision_loop.md` | Two surgical edits |
| `project_goals_vs_wake_config.md` | One surgical edit |
| `project_personality_self_perspective.md` | Three surgical edits |
| `MEMORY.md` | Update affected index hook lines |

**Files explicitly NOT touched:**

- `flavors/code/recipes/engineer.yaml`, `flavors/code/recipes/commit_summary.yaml` (real Code-flavor recipes — names correct)
- `flavors/code/frontend/src/index.ts` (typeId/label registration — real flavor-side UI hint)
- `flavors/code/tests/engineer_e2e.rs`, `flavors/code/tests/personality_registry.rs` (test names match real configurations)
- All engine code (`crates/core/src/personality/...`)
- `AGENTS.md`, `CLAUDE.md`, `README.md` (no archetype mentions present)
- `docs/03-schema-registry.md` (only "query planner" mentions — false positives)
- The canonical spec itself (`docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md` — already committed as `b916811`)

---

## Tasks

### Task 1: Add framing-supersession header to all four affected specs

**Files:**
- Modify: `docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md`
- Modify: `docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md`
- Modify: `docs/superpowers/specs/2026-05-09-personality-authorship-edge.md`
- Modify: `docs/superpowers/specs/2026-05-09-workspace-mode-design.md`

The same header text goes into all four files, immediately after the H1 line. Each file currently has the structure:

```markdown
# <H1 title>

**Status:** <draft|design>
```

After this task, each will have:

```markdown
# <H1 title>

> **Framing supersession (2026-05-10):** Names like Engineer, Visionary, Planner,
> Worker, Tester in this spec are flavor-shipped or illustrative labels, not
> engine archetypes. The engine knows only `PersonalityInstanceId`. Canonical
> vocabulary lives in
> [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
> Decisions and behavior in this spec stand; only the framing is updated.
> **Phase 2 of the alignment will replace this header with surgical text edits.**

**Status:** <draft|design>
```

- [ ] **Step 1: Add header to `2026-05-06-personality-wake-decide-write-design.md`**

Use Edit tool with `old_string` and `new_string`:

`old_string`:
```
# Personality Wake/Decide/Write Architecture

**Status:** Draft
```

`new_string`:
```
# Personality Wake/Decide/Write Architecture

> **Framing supersession (2026-05-10):** Names like Engineer, Visionary, Planner,
> Worker, Tester in this spec are flavor-shipped or illustrative labels, not
> engine archetypes. The engine knows only `PersonalityInstanceId`. Canonical
> vocabulary lives in
> [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
> Decisions and behavior in this spec stand; only the framing is updated.
> **Phase 2 of the alignment will replace this header with surgical text edits.**

**Status:** Draft
```

- [ ] **Step 2: Add header to `2026-05-07-personality-as-composed-behaviors.md`**

`old_string`:
```
# Personality as Composed Behaviors

**Status:** Draft
```

`new_string`:
```
# Personality as Composed Behaviors

> **Framing supersession (2026-05-10):** Names like Engineer, Visionary, Planner,
> Worker, Tester in this spec are flavor-shipped or illustrative labels, not
> engine archetypes. The engine knows only `PersonalityInstanceId`. Canonical
> vocabulary lives in
> [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
> Decisions and behavior in this spec stand; only the framing is updated.
> **Phase 2 of the alignment will replace this header with surgical text edits.**

**Status:** Draft
```

- [ ] **Step 3: Add header to `2026-05-09-personality-authorship-edge.md`**

`old_string`:
```
# Personality authorship edge: `core/authored`

**Status:** design
```

`new_string`:
```
# Personality authorship edge: `core/authored`

> **Framing supersession (2026-05-10):** Names like Engineer, Visionary, Planner,
> Worker, Tester in this spec are flavor-shipped or illustrative labels, not
> engine archetypes. The engine knows only `PersonalityInstanceId`. Canonical
> vocabulary lives in
> [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
> Decisions and behavior in this spec stand; only the framing is updated.
> **Phase 2 of the alignment will replace this header with surgical text edits.**

**Status:** design
```

- [ ] **Step 4: Add header to `2026-05-09-workspace-mode-design.md`**

`old_string`:
```
# Workspace mode — Phase 1e

**Status:** design
```

`new_string`:
```
# Workspace mode — Phase 1e

> **Framing supersession (2026-05-10):** Names like Engineer, Visionary, Planner,
> Worker, Tester in this spec are flavor-shipped or illustrative labels, not
> engine archetypes. The engine knows only `PersonalityInstanceId`. Canonical
> vocabulary lives in
> [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
> Decisions and behavior in this spec stand; only the framing is updated.
> **Phase 2 of the alignment will replace this header with surgical text edits.**

**Status:** design
```

- [ ] **Step 5: Verify header presence**

Run:
```bash
grep -L "Framing supersession (2026-05-10)" \
  docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md \
  docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md \
  docs/superpowers/specs/2026-05-09-personality-authorship-edge.md \
  docs/superpowers/specs/2026-05-09-workspace-mode-design.md
```

Expected: empty output (every file carries the header).

---

### Task 2: Surgical edit `docs/02-memory.md` — anchor "Stoic Visionary" as hypothetical

**Files:**
- Modify: `docs/02-memory.md` (around line 497)

The current paragraph uses "Stoic Visionary" and "Workhorse Programmer" as if they were named entities in the supersession example. Reframe them as *hypothetical user-authored personality labels* so the prose matches the canonical vocabulary (archetype names are user-chosen, not engine entities).

- [ ] **Step 1: Apply the edit**

Use Edit tool:

`old_string`:
```
**Supersession is intra-personality by default.** Re-deriving under a
different `personality_id` produces a *parallel* lineage, not a
supersession — Stoic Visionary's Abstraction over Fact `f` does not
supersede Workhorse Programmer's Abstraction over the same `f`. They
coexist as parallel interpretations.
```

`new_string`:
```
**Supersession is intra-personality by default.** Re-deriving under a
different `personality_id` produces a *parallel* lineage, not a
supersession — an Abstraction authored by a personality with one
self-Perspective (e.g., a hypothetical "Stoic Visionary" personality) over
Fact `f` does not supersede an Abstraction authored by a personality with a
different self-Perspective (e.g., "Workhorse Programmer") over the same
`f`. They coexist as parallel interpretations. (Names like "Stoic
Visionary" and "Workhorse Programmer" here are user-chosen labels, not
engine archetypes; see
[`docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md`](superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md).)
```

- [ ] **Step 2: Verify the edit landed**

Run:
```bash
grep -n "hypothetical \"Stoic Visionary\"" docs/02-memory.md
```

Expected: one matching line (anchored phrase present).

---

### Task 3: Surgical edit `docs/10-configuration.md` — clarify `engineer-v1` is flavor-side

**Files:**
- Modify: `docs/10-configuration.md` (around line 360–366)

The current per-personality dispatcher config uses `id = "proxima-code/engineer-v1"` and `id = "proxima-code/commit-summary-v1"` as keys. These are real flavor-side identifiers; add a single clarifying line right above the block.

- [ ] **Step 1: Apply the edit**

Use Edit tool:

`old_string`:
```
[[personalities.per_personality]]
id        = "proxima-code/commit-summary-v1"
workers   = 2
timeout_s = 180

[[personalities.per_personality]]
id          = "proxima-code/engineer-v1"
workers     = 1
queue_depth = 256
```

`new_string`:
```
# Note: `id` strings here are flavor-shipped personality identifiers (e.g.,
# `proxima-code/engineer-v1` is a Code-flavor recipe / type identifier), not
# engine archetypes. The engine's runtime personality identity is
# `PersonalityInstanceId`. See
# `docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md`.

[[personalities.per_personality]]
id        = "proxima-code/commit-summary-v1"
workers   = 2
timeout_s = 180

[[personalities.per_personality]]
id          = "proxima-code/engineer-v1"
workers     = 1
queue_depth = 256
```

- [ ] **Step 2: Verify the edit landed**

Run:
```bash
grep -n "flavor-shipped personality identifiers" docs/10-configuration.md
```

Expected: one matching line.

---

### Task 4: Rewrite `project_personality_multi_instance.md` (memory)

**Files:**
- Modify: `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_personality_multi_instance.md` (full rewrite)

The current file claims identity is `(TypeId, InstanceId)` — wrong post-collapse — and uses archetype examples (Senior/Junior, Worker, Alice-Engineer/Bob-Engineer, "create another Engineer"). Replace the entire file.

- [ ] **Step 1: Write the new content**

Use Write tool with `file_path: /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_personality_multi_instance.md` and `content`:

```markdown
---
name: Personality identity is PersonalityInstanceId
description: Personality runtime identity is `PersonalityInstanceId(Uuid)` only — no `(TypeId, InstanceId)` tuple. Multiple personality instances per owner are first-class. `type_id` survives only as a transient borrowed string in tool-invocation context.
type: project
originSessionId: 7f991cd2-e6e2-4f50-b9af-7bb002f73997
---
Personality runtime identity is `PersonalityInstanceId(Uuid)` — full stop. The road-to-v1 identity-collapse work (commits `a52421e` → `29bc041`) dropped `personality_type_id` columns from rows, response shapes, and the PG schema. There is no `(TypeId, InstanceId)` tuple at runtime.

Where you see `type_id: &'a str` in code (`PersonalityToolContext`, `WakeTokenContext` in `crates/core/src/personality/mod.rs`), it is a *transient borrowed string* carried through tool invocation for telemetry and provenance — not a stored identity, not an enum, not a registered type.

**Why:** Multiple personality instances per owner is the load-bearing capability — parallel task-shard scale-out, instance-vs-instance debates, reasoning chains. Modeling instance differences as type-level (Senior/Junior-as-types) was conflating identity with role. The simplification to instance-id alone keeps multi-instance unchanged in spirit while removing the spurious type axis.

**How to apply:**
- Self-wake forbidden by **dispatcher invariant**: dispatcher always excludes events authored by the waking instance (`event.authoring_instance_id == self.instance_id`). Filter language doesn't express self-exclusion; it's structural.
- Wake filter `authored_by` slot identifies a specific instance via `PersonalityInstanceId` (or `Any`).
- Instantiation is a substrate verb that mints a new `PersonalityInstanceId`, a Root Perspective memory_id, and a WakeConfig with N WakeEntry rows. Owner-provisioning may default-instantiate one or more personalities via flavor-shipped `register_owner_defaults(owner)` for v1 ergonomics.
- "Create another personality" UI mints a new instance with a custom `display_name` in the self-payload.
- Cross-instance interaction via multi-instance: instance A's Perspective triggers instance B's wake (filter passes because dispatcher self-excludes A's events from A's queue, but B is a different instance so A's Perspective IS visible to B). Cycle bounded by external entropy + per-wake stop conditions + `MAX_WAKE_CHAIN_DEPTH`.

See `project_personality_as_composed_behaviors.md` and `docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md` for the canonical vocabulary.
```

- [ ] **Step 2: Verify**

Run:
```bash
grep -c "PersonalityInstanceId" /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_personality_multi_instance.md
```

Expected: ≥3 matches (header description + body references).

```bash
grep -E "Senior|Junior|Alice-Engineer|Bob-Engineer" /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_personality_multi_instance.md
```

Expected: empty output (old archetype examples gone).

---

### Task 5: Rewrite `project_spinning_wheel_architecture.md` (memory)

**Files:**
- Modify: `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_spinning_wheel_architecture.md` (full rewrite)

The current file frames V→E→P→W→T as *the* architecture. Reframe: cross-personality chains are a *composition pattern*; archetype names are user-chosen labels; engine knows only `PersonalityInstanceId`.

- [ ] **Step 1: Write the new content**

Use Write tool with `file_path: /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_spinning_wheel_architecture.md` and `content`:

```markdown
---
name: Cross-personality chains as a composition pattern
description: Long-term Code-flavor usage involves chains of personality instances coordinating via Reality Events through cross-personality wake filters. Names like Visionary/Engineer/Planner/Worker/Tester are user-chosen labels for instances in such a chain — not engine archetypes. The engine knows only `PersonalityInstanceId` and supports any chain composition without rework.
type: project
originSessionId: 7f991cd2-e6e2-4f50-b9af-7bb002f73997
---
Long-term Code-flavor usage will compose chains of personality instances coordinating via Reality Events. A typical chain a user or flavor author might build: an instance that proposes direction (label it "Visionary"), one that emits engineering perspectives (label it "Engineer"), one that creates implementation plans ("Planner"), one or more that do the work ("Worker"), and one that validates ("Tester"). Coordination flows through cross-personality wake filters: instance B's WakeEntry on schema X fires when instance A authors a row with that schema. The "spinning wheel" closes via cross-personality event chains, not self-wake.

**Why:** Decomposes the agent loop into specialist personalities with bounded scope, instead of one all-knowing decider. Each instance's tool palette + WakeConfig + Goal set is small enough to reason about. The wake filter (`{ schema_id, authored_by }`) is the load-bearing primitive that makes any chain work.

**How to apply:** Names like Visionary, Engineer, Planner, Worker, Tester are *user-chosen labels* for personality instances — not engine archetypes. The engine knows only `PersonalityInstanceId`. Code's v1 ships with two flavor-shipped default personalities (one labeled "Engineer," one "CommitSummary") via `register_owner_defaults`; users compose additional instances with whatever labels they choose. The dispatcher, wake-filter shape, and tool-palette mechanism already accommodate any chain length without architectural rework. See `docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md` for the canonical vocabulary.
```

- [ ] **Step 2: Verify**

Run:
```bash
grep -c "user-chosen labels" /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_spinning_wheel_architecture.md
```

Expected: ≥1 match.

---

### Task 6: Rewrite `project_code_flavor_demo_config.md` (memory)

**Files:**
- Modify: `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_code_flavor_demo_config.md` (full rewrite)

Anchor "Engineer" to "Code-flavor default-instantiated personality."

- [ ] **Step 1: Write the new content**

Use Write tool with `file_path: /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_code_flavor_demo_config.md` and `content`:

```markdown
---
name: Code flavor demos configurable wake sources
description: Code flavor exposes editable wake-source configuration in its frontend for the personality instances it ships (notably the default Engineer-labeled instance) — for demonstration and experimentation, not deferred.
type: project
originSessionId: 7f991cd2-e6e2-4f50-b9af-7bb002f73997
---
The Code flavor is the demo vehicle for runtime personality configuration. Its frontend (canvas + inspector + recipe pickers + tool-palette pickers) exposes editable wake-source config for personality instances — including the default `display_name: "Engineer"` instance Code's `register_owner_defaults` mints per new owner. Examples: "wake on commit summary," and other event types as the wake-source surface grows.

**Why:** Demonstrates the runtime-config story (project_personality_decision_loop) end-to-end without admitting runtime *code* registration. Code is v1's first flavor and the marketplace pitch — the configurable wake-source UI is the visible artifact that proves "flavors decide initial wake policy via `register_owner_defaults`; users can edit per-instance via the personality canvas (NOT Goals — see project_goals_vs_wake_config.md)."

**How to apply:** Any plan touching the Code flavor's personality wiring should include the frontend wake-source UI (or explicitly flag its absence as a follow-up) — don't defer it as a v1.1 concern. "Engineer" here refers to the Code-flavor default-instantiated personality (display_name string + bundled recipe `flavors/code/recipes/engineer.yaml`), not an engine archetype; see `docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md`. Other flavors may choose hard-coded wake policies; only Code is committed to user-editable config in v1.
```

- [ ] **Step 2: Verify**

Run:
```bash
grep -c "Code-flavor default-instantiated personality\|default-instantiated personality" /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_code_flavor_demo_config.md
```

Expected: ≥1 match.

---

### Task 7: Spot edit `project_personality_decision_loop.md` (memory)

**Files:**
- Modify: `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_personality_decision_loop.md` (two surgical edits)

- [ ] **Step 1: Replace "Multi-personality spinning wheel" sentence**

Use Edit tool:

`old_string`:
```
 Multi-personality "spinning wheel" architecture (Visionary → Engineer → Planner → Worker → Tester via cross-personality wake filters) is the long-term shape — see project_spinning_wheel_architecture.md.
```

`new_string`:
```
 Cross-personality chains via cross-personality wake filters are the long-term composition pattern (names for instances in any such chain are user-chosen labels, not engine archetypes — see project_spinning_wheel_architecture.md and `docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md`).
```

- [ ] **Step 2: Replace "Engineer-alone" v1-scope phrase**

Use Edit tool:

`old_string`:
```
 AND Engineer-alone tool-calling decider implementation; multi-personality chains as v1.1+.
```

`new_string`:
```
 AND a tool-calling decider implementation for v1's flavor-shipped default personalities (Code's Engineer + CommitSummary); richer multi-instance chain examples as v1.1+.
```

- [ ] **Step 3: Verify**

Run:
```bash
grep -E "Visionary → Engineer → Planner|Engineer-alone tool-calling" /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_personality_decision_loop.md
```

Expected: empty output.

---

### Task 8: Spot edit `project_goals_vs_wake_config.md` (memory)

**Files:**
- Modify: `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_goals_vs_wake_config.md` (one surgical edit)

- [ ] **Step 1: Replace "Engineer creates a Goal and edge-links it to Worker" example**

Use Edit tool:

`old_string`:
```
 Goals authored by personalities (e.g., Engineer creates a Goal and edge-links it to Worker) flow through the wake filter as ordinary Reality Events
```

`new_string`:
```
 Goals authored by personalities (e.g., personality A creates a Goal and edge-links it to personality B's self-Perspective) flow through the wake filter as ordinary Reality Events
```

- [ ] **Step 2: Verify**

Run:
```bash
grep -E "Engineer creates a Goal and edge-links it to Worker" /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_goals_vs_wake_config.md
```

Expected: empty output.

---

### Task 9: Spot edit `project_personality_self_perspective.md` (memory)

**Files:**
- Modify: `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_personality_self_perspective.md` (three surgical edits)

- [ ] **Step 1: Replace `"I want to solve problems" for Engineer` phrasing**

Use Edit tool:

`old_string`:
```
expressing the personality's identity ("I want to solve problems" for Engineer).
```

`new_string`:
```
expressing the personality's identity (e.g., "I want to solve problems" for a personality with the engineering-labeled self-Perspective Code's flavor ships as a default).
```

- [ ] **Step 2: Replace "Cross-personality coordination (Visionary edge-linking..." phrase**

Use Edit tool:

`old_string`:
```
 Cross-personality coordination (Visionary edge-linking a Goal to Engineer's self) flows through the same mechanism.
```

`new_string`:
```
 Cross-personality coordination (e.g., a personality with `proxima-goal/goal_propose` in its palette edge-linking a Goal to another personality's self-Perspective) flows through the same mechanism.
```

- [ ] **Step 3: Replace "Visionary reading Engineer's..." discoverability example**

Use Edit tool:

`old_string`:
```
 (e.g. Visionary reading Engineer's "I want to solve problems" before deciding to assign a Goal to Engineer rather than to Worker).
```

`new_string`:
```
 (e.g., one personality reading another's self-Perspective payload before deciding to assign a Goal to that instance rather than to a different one).
```

- [ ] **Step 4: Verify**

Run:
```bash
grep -E "\"I want to solve problems\" for Engineer|Visionary edge-linking|Visionary reading Engineer" /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/project_personality_self_perspective.md
```

Expected: empty output.

(Note: the line later in the same file mentioning `self_schema = code/engineer-self-v1` stays — it's a real Code-flavor schema id, correctly anchored to its flavor.)

---

### Task 10: Update `MEMORY.md` index hooks (memory)

**Files:**
- Modify: `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/MEMORY.md` (three index lines)

The hooks for the three rewritten memory files (multi-instance, spinning wheel, code-flavor-demo-config) reference the old framing and need updating to match the new content.

- [ ] **Step 1: Update the Spinning wheel hook**

Use Edit tool on `/Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/MEMORY.md`:

`old_string`:
```
- [Spinning wheel — multi-personality chain](project_spinning_wheel_architecture.md) — Long-term: Visionary → Engineer → Planner → Worker → Tester via wake filters; v1 = Engineer alone but design must accommodate chain
```

`new_string`:
```
- [Cross-personality chains as composition pattern](project_spinning_wheel_architecture.md) — Chains of personality instances coordinate via cross-personality wake filters; archetype names are user-chosen labels, engine knows only PersonalityInstanceId
```

- [ ] **Step 2: Update the Personality identity hook**

`old_string`:
```
- [Personality identity = (TypeId, InstanceId)](project_personality_multi_instance.md) — Static type-level ID + per-instance ID (= self-Perspective memory_id); multiple instances per type per owner are first-class; enables Worker scale-out and Engineer debates
```

`new_string`:
```
- [Personality identity is PersonalityInstanceId](project_personality_multi_instance.md) — Identity is `PersonalityInstanceId(Uuid)` only post-collapse; `type_id` survives only as transient borrowed string in tool context; multi-instance per owner is first-class
```

- [ ] **Step 3: Update the Code flavor demos hook**

`old_string`:
```
- [Code flavor demos configurable wake sources](project_code_flavor_demo_config.md) — Code frontend exposes editable wake-source config for Engineer (v1 deliverable, not deferred)
```

`new_string`:
```
- [Code flavor demos configurable wake sources](project_code_flavor_demo_config.md) — Code frontend exposes editable wake-source config for Code's flavor-shipped default personality instances (v1 deliverable, not deferred)
```

- [ ] **Step 4: Verify**

Run:
```bash
grep -E "Visionary → Engineer → Planner|Personality identity = \(TypeId|wake-source config for Engineer\b" /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/MEMORY.md
```

Expected: empty output.

```bash
grep -cE "Cross-personality chains as composition pattern|Personality identity is PersonalityInstanceId|flavor-shipped default personality instances" /Users/heinrichvonhelmolt/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/MEMORY.md
```

Expected: 3 matches.

---

### Task 11: Phase 1 verification grep + commit

**Files:**
- (None modified in this task — only verification + commit)

- [ ] **Step 1: Verify all four spec headers present**

Run:
```bash
grep -L "Framing supersession (2026-05-10)" \
  docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md \
  docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md \
  docs/superpowers/specs/2026-05-09-personality-authorship-edge.md \
  docs/superpowers/specs/2026-05-09-workspace-mode-design.md
```

Expected: empty output.

- [ ] **Step 2: Verify numbered-doc edits**

Run:
```bash
grep -n "hypothetical \"Stoic Visionary\"" docs/02-memory.md && \
grep -n "flavor-shipped personality identifiers" docs/10-configuration.md
```

Expected: one matching line in each file.

- [ ] **Step 3: Inspect what will be committed**

Run:
```bash
git status --short && echo "---" && git diff --stat
```

Expected: 6 modified files in `docs/` (4 specs + 2 numbered docs), no other changes.

- [ ] **Step 4: Stage and commit**

Run:
```bash
git add \
  docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md \
  docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md \
  docs/superpowers/specs/2026-05-09-personality-authorship-edge.md \
  docs/superpowers/specs/2026-05-09-workspace-mode-design.md \
  docs/02-memory.md \
  docs/10-configuration.md && \
git commit -m "$(cat <<'EOF'
docs(personality): align vocabulary with engine reality (phase 1)

Adds the framing-supersession header to the four personality-related
specs and surgical clarifiers to docs/02-memory.md and
docs/10-configuration.md, per the canonical spec at
docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md.

Phase 2 (surgical archetype cleanup of the four old specs) lands as
a follow-up commit.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

- [ ] **Step 5: Verify commit landed**

Run:
```bash
git log --oneline -3
```

Expected: top commit is `docs(personality): align vocabulary with engine reality (phase 1)`.

---

## Notes for the executor

- **Memory files are NOT in the repo.** They live at `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/`. Do not stage them, do not commit them. Each memory rewrite/edit is a standalone Edit/Write operation outside git.
- **Use Edit, not Write, for surgical changes.** The numbered-doc edits, spot edits on memory files, and `MEMORY.md` hook updates are surgical — use the Edit tool with exact `old_string`/`new_string`. Memory rewrites in Tasks 4, 5, 6 use Write because they replace the entire file.
- **Don't edit anything in `flavors/code/...`.** "Engineer" strings in flavor code are real configurations; renaming them is out of scope.
- **Don't edit the canonical spec.** It is already committed (`b916811`).
- **One commit at the end.** All six repo edits batch into the single Task 11 commit on `road-to-v1`.
