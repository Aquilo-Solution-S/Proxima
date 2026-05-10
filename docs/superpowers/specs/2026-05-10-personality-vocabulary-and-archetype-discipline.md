# Personality Vocabulary and Archetype Discipline

## Goal

Establish one canonical statement of the personality vocabulary that
matches what the engine actually does, and remove the impression —
created by several earlier specs and assistant-side memory entries —
that names like Engineer, Visionary, Planner, Worker, Tester are
first-class engine entities.

They are not. The engine deals only in `PersonalityInstanceId(Uuid)`
instances composed from `{Root Perspective, WakeConfig, WakeEntry[],
ToolPalette refs, Recipe ref}`. Archetype names are user-chosen
labels, optionally shipped as flavor-side
`register_owner_defaults(owner)`.

This spec is the single source of truth for that vocabulary. Other
personality-related specs are dated historical artifacts that capture
what we decided when; they ride on this vocabulary going forward and
will be cleaned up surgically in Phase 2 of this alignment.

## Non-Goals

- **No engine code changes.** The framework is already generic. The
  misalignment is in docs, not code. The road-to-v1 personality
  identity-collapse commits (`a52421e` → `29bc041`) dropped type-id
  columns from rows, response shapes, and the PG schema and
  collapsed identity to `instance_id`. This spec describes that
  reality, not a future state. (Note: throughout this spec, "Phase 1"
  and "Phase 2" refer to the *alignment work itself* — the canonical
  spec landing now and the cleanup that follows. Distinct from the
  road-to-v1 phasing referenced here.)
- **No removal of flavor-side `engineer.yaml`, `engineer-v1`
  registration, or `engineer_e2e.rs`.** Those are real Code-flavor
  configurations, correctly named.
- **No design of hierarchical-mutation-via-MCP-CRUD.** That is a
  separable follow-on that rides on this vocabulary; it is tracked
  separately and is explicitly out of scope here.
- **No design of the personality-authoring walkthrough.** Setting up
  concrete personalities through the canvas/inspector or MCP is
  sequenced after the vocabulary lands.
- **No introduction of role slots, archetype enums, or any move
  toward typed archetypes.** This spec doubles down on
  archetypes-as-labels. If a future need pushes back, it gets its
  own spec.
- **No invalidation of prior decisions.** Specifically, the
  rejection of "non-LLM Worker personality" in
  `2026-05-06-personality-wake-decide-write-design.md` stands.
  Only framing changes.

## The Three Layers

### Engine layer

The engine knows three things:

1. **`PersonalityInstanceId(Uuid)`.** The runtime identity of every
   personality. Full stop — there is no `(TypeId, InstanceId)` tuple
   at runtime. The road-to-v1 identity-collapse work dropped type-id
   columns from rows and response shapes. The `PersonalityRef`
   wrapper carries only the instance id.

2. **What an instance points to.** One Root Perspective (memory_id,
   append-only), one WakeConfig with N WakeEntry rows, and a stable
   `core/authored` edge convention from Root Perspective → outputs
   the personality writes during a wake.

3. **What a WakeEntry composes.**
   `{trigger_kind ∈ {OnMemory, OnEdge}, trigger_id, ContextBuilder,
   recipe_ref, tool_palette, max_rounds, probability}`. Each entry
   is unique per `(trigger_kind, trigger_id)` per personality.

There is no engine-level concept of an "Engineer," a "Visionary,"
or any other archetype. Where you see `type_id: &'a str` in code
(`PersonalityToolContext`, `WakeTokenContext`), it is a *transient
borrowed string* carried through tool invocation for telemetry and
provenance — not a stored identity, not an enum, not a registered
type.

### Flavor layer

Flavors are the only place archetype names exist as real strings in
code. For the Code flavor specifically:

- **`flavors/code/recipes/engineer.yaml`** — a Goose recipe file the
  user can attach to any personality instance.
- **`flavors/code/recipes/commit_summary.yaml`** — likewise.
- **`flavors/code/frontend/src/index.ts`** — flavor-side UI hint
  registering `(typeId: "proxima-code/engineer-v1", label:
  "Engineer")` for the personality canvas. The typeId is a recipe /
  flavor-default identifier, *not* an engine type.
- **`flavors/code/register_owner_defaults`** — Rust code that mints
  two default personality instances per new owner: one with
  `display_name: "Engineer"` + the engineer recipe, one with
  `display_name: "CommitSummary"` + the commit-summary recipe.
  After provisioning, those instances are *fully owned by the user*
  with no template to diff against.

These are flavor-shipped configurations, not engine archetypes. A
user could rename, reconfigure, or delete them. A different flavor
could ship totally different defaults. A marketplace flavor could
ship a "Visionary" recipe with no engine support needed.

### User layer

Names like Visionary, Engineer, Planner, Worker, Tester are
user-chosen labels for personality instances. The engine sees only
`display_name: String` on the self-Perspective payload.
Cross-personality discovery happens through `list_self_perspectives`,
which returns each instance's current self-Perspective; an LLM
picking "an Engineer" is reading display_name strings out of those
payloads, not querying a typed registry.

The implication: the long-term "spinning wheel" pattern (a chain
of personalities like Visionary → Engineer → Planner → Worker →
Tester) is a *composition pattern* a user or flavor author can
build, not a fixed architecture the engine knows. Any chain is
valid as long as each link is a personality instance with a
WakeEntry that fires on the prior link's outputs.

## How to Write About Personalities

The discipline this imposes on prose:

**Engine-side prose.** Never use Engineer/Visionary/Planner/Worker/
Tester as if they were engine entities. Use:

- "personality A," "personality B"
- "instance A," "instance B"
- "the personality with WakeEntry on schema X"
- "high-frequency wake personality," "low-probability wake
  personality"
- "the wake-trigger schema," "the recipe author"

**Flavor-side prose.** Free to use archetype names when they refer
to actual flavor configurations, but always anchor the name to its
flavor: "the Code flavor's default Engineer personality," "Code's
CommitSummary default," "an Engineer-recipe instance."

**Cross-personality interaction examples.** Name instances A and B
(or by their wake-trigger schema), not by archetype. Save archetype
names for examples that are explicitly framed as flavor-shipped.

### Example pairs

| Wrong (engine-as-archetype framing) | Right (vocabulary-aligned) |
|---|---|
| "Engineer wakes on commit-summary-v1." | "A personality with a WakeEntry on `proxima-code/commit-summary-v1` wakes." (or, if the example is specifically about Code's default: "Code's default Engineer instance wakes on `proxima-code/commit-summary-v1`.") |
| "Engineer-Alice ↔ Engineer-Bob debates." | "Two personality instances A and B with overlapping wake schemas can interleave Perspectives." |
| "Visionary at 0.001 probability subscribes to most schemas." | "A low-probability wake personality (e.g. p=0.001) can subscribe to many schemas without flooding the dispatcher." |
| "Code's CommitSummary and Engineer migrate to Personalities." | (Already correct — names anchored to "Code's".) |
| "Worker scale-out: instantiate 5 Workers." | "Multi-instance scale-out: instantiate N personality instances of the same configuration with task-shard-keyed wake configs." |
| "A Planner personality should immediately re-plan on new Active goals." | "A personality whose recipe needs to react to goal-activation can add a WakeEntry on `proxima-goal/goal-activated-v1`." |
| "Visionary picks an Engineer based on self-Perspective payload." | "A personality with `list_self_perspectives` in its read palette can pick another personality by reading display_name and payload content." |

## Phase 1 Work (this PR)

1. **This canonical spec.** Lands at
   `docs/superpowers/specs/2026-05-10-personality-vocabulary-and-archetype-discipline.md`.
2. **Framing-supersession headers** added to four affected specs,
   right after the H1 title:
   - `2026-05-06-personality-wake-decide-write-design.md`
   - `2026-05-07-personality-as-composed-behaviors.md`
   - `2026-05-09-personality-authorship-edge.md`
   - `2026-05-09-workspace-mode-design.md`

   Header form:

   > **Framing supersession (2026-05-10):** Names like Engineer,
   > Visionary, Planner, Worker, Tester in this spec are
   > flavor-shipped or illustrative labels, not engine archetypes.
   > The engine knows only `PersonalityInstanceId`. Canonical
   > vocabulary lives in
   > [2026-05-10-personality-vocabulary-and-archetype-discipline.md](./2026-05-10-personality-vocabulary-and-archetype-discipline.md).
   > Decisions and behavior in this spec stand; only the framing is
   > updated. **Phase 2 of the alignment will replace this header
   > with surgical text edits.**

3. **Surgical numbered-doc edits.**
   - `docs/02-memory.md:497` — reframe "Stoic Visionary" example as
     a hypothetical user-authored personality, not an architectural
     entity.
   - `docs/10-configuration.md:366` — `id =
     "proxima-code/engineer-v1"` stays (real Code-flavor recipe id);
     add one short clarifying line that this id is a flavor-side
     identifier, not an engine type.

4. **Memory rewrites** in
   `~/.claude/projects/-Users-heinrichvonhelmolt-Repos-Proxima/memory/`.
   Most important corrections:
   - `project_personality_multi_instance.md` — claims identity is
     `(TypeId, InstanceId)`. Post-Phase-2, identity is
     `PersonalityInstanceId` only; `type_id` survives as a transient
     borrowed string in tool context. Rewrite.
   - `project_spinning_wheel_architecture.md` — frames V→E→P→W→T as
     *the* architecture. Reframe: chains-of-personalities are the
     architecture; archetype names are user-chosen labels for any
     composition.
   - `project_first_flavor.md`,
     `project_code_flavor_demo_config.md` — anchor "Engineer" to
     "Code flavor's default-instantiated personality with
     `display_name: \"Engineer\"`."
   - The remaining four
     (`project_personality_decision_loop.md`,
     `project_typed_goals.md`,
     `project_decider_in_flavor.md`,
     `project_personality_as_composed_behaviors.md`) get spot edits.
   - Update `MEMORY.md` index lines accordingly.

5. **Single commit on `road-to-v1`.** Suggested message:
   `docs(personality): align vocabulary with engine reality (phase 1)`.

## Phase 2 Cleanup (follow-up commit)

Phase 1 leaves a duplication window: the canonical spec is the
source of truth, but the four old specs still carry archetype-laden
prose under their framing-supersession header. Phase 2 closes that
window before it can drift, honoring the project's no-doc-duplication
rule.

**Approach: surgical text edits in place** on the four affected
specs. Walk through each archetype reference and replace it
according to its kind:

- **References to the Code flavor's actual default personality**
  (e.g., `2026-05-09-workspace-mode-design.md:19` "Today the
  Engineer wakes on `proxima-code/commit-summary-v1`") → keep the
  name, anchor it: "the Code flavor's default Engineer personality
  wakes on..."
- **Generic illustrative use** (e.g., `2026-05-06`'s Engineer-Alice
  ↔ Engineer-Bob debates) → rename to `personality A ↔ personality
  B` or `instance A ↔ instance B`. The point of those examples is
  multi-instance behavior, not Engineering.
- **Aspirational future-flavor mentions** (e.g., `2026-05-06:412`
  "Visionary, Worker, Tester chain — additive, no rework needed") →
  reword as: "Additional personalities (whatever names users or
  future flavors give them) — additive, no rework needed."
- **Stress-test setup paragraphs** (most of `2026-05-07` lines
  663-796) → rewrite to use generic personality names; keep test
  logic identical.

After surgical edits, **remove the framing-supersession header**.
Cross-references to this canonical spec stay only where genuinely
useful.

Phase 2 is **not blind find-replace.** It needs eyes on each
occurrence. ~80–100 edits across four files based on grep counts.
Land it in its own commit (`docs(personality): phase 2 surgical
archetype cleanup`) so the Phase 1 PR stays reviewable.

## What Stays Untouched

- All flavor-side code: `flavors/code/recipes/engineer.yaml`,
  `flavors/code/recipes/commit_summary.yaml`,
  `flavors/code/frontend/src/index.ts`,
  `flavors/code/tests/engineer_e2e.rs`,
  `flavors/code/tests/personality_registry.rs`,
  `register_owner_defaults`.
- All engine code.
- Specs and numbered docs without archetype leakage (anything
  outside the four named specs and the two numbered docs above).
- AGENTS.md, CLAUDE.md, README.md (no archetype mentions
  detected).

## Verification

Docs-only change, so verification is reviewer eyes plus one grep
at the end of Phase 2:

```bash
grep -nE "\b(Visionary|Engineer|Planner|Worker|Tester)\b" \
  docs/superpowers/specs/2026-05-0[6-9]*.md
```

Expected after Phase 2: only Code-flavor-anchored mentions
(e.g. "Code's default Engineer," "the engineer recipe"). No bare
archetype usage that implies engine-level types.

For Phase 1, a softer check:

```bash
grep -L "Framing supersession (2026-05-10)" \
  docs/superpowers/specs/2026-05-06-personality-wake-decide-write-design.md \
  docs/superpowers/specs/2026-05-07-personality-as-composed-behaviors.md \
  docs/superpowers/specs/2026-05-09-personality-authorship-edge.md \
  docs/superpowers/specs/2026-05-09-workspace-mode-design.md
```

Expected: empty output (every file carries the header).

## Rollback

Trivial — `git revert` per phase. Phase 1 can roll back without
affecting Phase 2; Phase 2 can roll back without affecting Phase 1.
No code paths depend on the framing.

## References

- `crates/core/src/personality/mod.rs` — current
  `PersonalityInstanceId`, `PersonalityRef`, WakeEntry shape, and
  the `type_id: &'a str` transient field in
  `PersonalityToolContext`.
- `flavors/code/recipes/engineer.yaml`,
  `flavors/code/recipes/commit_summary.yaml` — actual Code-flavor
  recipes.
- `flavors/code/frontend/src/index.ts` — typeId / label
  registration for the personality canvas.
- Road-to-v1 identity collapse commits: `a52421e`
  ("collapse identity toward instance_id"), `9acecb8`
  ("drop personality_type_id from rows and response shapes"),
  `1805acd` ("drop personality_type_id columns from PG schema"),
  `29bc041` ("remove PersonalityFlavor trait + provision_owner
  verb").
- Project rule:
  `feedback_no_doc_duplication.md` — "all duplications drift;
  remove duplicates with cross-references rather than aligning two
  copies."
