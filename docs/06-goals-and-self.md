# 06. Goals And Self

## Contract

| Surface | Contract |
|---|---|
| Goal | Direction: desired future state, DAG position, lifecycle head |
| Memory | Observation or interpretation; never a Goal |
| Self | Query result, not entity |
| Assignment | `Goal --core/inspires--> Self-Perspective` |
| Wake policy | Explicit wake entries; never stored on Goal |

<a id="goal-entity"></a>

## Goal Entity

`Goal` is a core entity.

Not Memory:

| Kind | Meaning | Lifecycle |
|---|---|---|
| Fact | observation | immutable |
| Abstraction | operator-authored synthesis | supersession |
| Perspective | personality-relative view | supersession |
| Goal | intended direction | supersession |

Goal row fields:

| Field | Rule |
|---|---|
| `goal_id` | UUIDv7 identity |
| `owner` | per-row access scope |
| `schema_id`, `schema_version` | registered `GoalPayload` |
| `title`, `text` | core retrieval/render text |
| `payload` | typed sidecar bytes |
| `state` | lifecycle state |
| `supersedes` | previous Goal head, nullable |
| `parent_goal_ids` | DAG parents |
| `authorship` | `User`, `System`, or `External` |
| `request_id` | Owner-scoped idempotency key |

States:

| State | Active set | Terminal | Meaning |
|---|---:|---:|---|
| `Active` | yes | no | Live direction |
| `Paused` | no | no | Suspended direction |
| `Achieved` | no | yes | Positive close |
| `Abandoned` | no | yes | Post-active negative close |

Lifecycle:

```
(none) -> Active
Active -> Active       # modification
Active -> Paused
Paused -> Active
Active -> Achieved
Active -> Abandoned
```

Every transition writes a new Goal row. No in-place mutation.
Compliance erasure is the only delete path.

Active set:

```
G_active(owner) =
  current Goal heads
  where state = Active
```

<a id="goal-write-api"></a>

## Goal-Write API

`GoalWrite` is the protocol write surface for Goal rows.

Rules:

| Rule | Effect |
|---|---|
| Owner-scoped | caller must access `draft.owner` |
| Schema-checked | `schema_id` / `schema_version` resolves to `GoalPayload` |
| Append-only | create or supersede; never update |
| Idempotent | same `(Owner, request_id, body)` returns same `GoalId` |
| Conflict-detecting | reused request id with different body fails |
| Stream-visible | successful write emits `change_event` |

Supersession constraints:

| Constraint | Rule |
|---|---|
| Same Owner | prior and new Goal share Owner |
| Current head | stale prior cannot be lifecycle head |
| Valid transition | prior state and new state pair is admitted |
| Payload typed | new row carries the target schema payload |

Lifecycle Facts are observations of Goal lifecycle writes. They do not
replace Goal identity.

<a id="self--flavor-projection"></a>

## Self -- Flavor Projection

There is no Self row.

Runtime Self for one personality instance:

```
Self(instance) =
  current_root_perspective(instance)
  + active_perspective_heads(instance)
  + active_goals(current_root_perspective(instance))
```

The self anchor is a Perspective memory row whose schema is declared by
the personality flavor. Self evolution is Perspective supersession plus
the instance's current-root pointer changing to the new head.

`active_perspective_heads(instance)` is a bounded wake-context projection
of same-personality, non-root Perspective heads. It excludes superseded
rows and Self/root identity schemas.

Self is never cached as:

| Forbidden | Reason |
|---|---|
| Memory row | duplicates Perspective and Goal state |
| Goal row | confuses direction with identity |
| materialized causal chain | cache would become authority |

<a id="goal-assignment"></a>

## Goal Assignment

Assignment is a typed edge:

```
Goal
  --core/inspires-->
Self-Perspective
```

Storage endpoint kinds:

| Endpoint | Kind |
|---|---|
| source | `Goal` |
| target | `Perspective` |

No `GoalConnection` sidecar.

`active_goals(instance)`:

```
self = current_root_perspective(instance)
assigned = incoming core/inspires targets where target = self
heads = follow Goal supersession for each assigned source
return heads where state = Active
```

Assignment means "this Goal may inspire this Self." It does not imply
execution, obligation, repository scope, or wake policy.

## Goal Core Boundary

Core owns:

| Surface | Owned by core |
|---|---|
| Entity | Goal row, Goal identity, 4-state lifecycle |
| Payloads | `GoalPayload` schemas |
| Verb | `GoalWrite` |
| Lifecycle Facts | activated / paused / achieved / abandoned schemas |
| Tools | `core_goal_set`, `core_goal_transition` (pause / resume / abandon), `core_goal_mark_achieved`, `core_goal_modify`, `core_goal_decompose` |
| Relations | `core/inspires`, `core/motivated-by` |
| Query | active-goal traversal |
| Renderers | Goal and lifecycle payload views |

Evidence:

```
Goal --core/motivated-by--> Fact
Goal --core/motivated-by--> Abstraction
```

Lifecycle Fact provenance:

```
Lifecycle Fact --core/derived-from--> Fact evidence
```

Abstraction evidence remains represented by `core/motivated-by`.

## Goal-Scoped Wake Policy

Goal assignment does not create wake behavior.

Goal-reactive execution requires an enabled wake entry whose policy
matches the emitted event. The usual goal-reactive trigger:

| Field | Value |
|---|---|
| `trigger_kind` | `on_memory` |
| `trigger_id` | `core/goal-activated-v1` |
| `goal_scope` | `trigger_goal_assigned` |

`goal_scope = trigger_goal_assigned` means the wake receives the Goal
only when the triggering lifecycle Fact refers to a Goal assigned to the
personality's current Self-Perspective.

Planner-first execution:

```
Goal -> planner personality -> execution-request Fact -> worker
```

Goals do not bind repos, worktrees, commands, or workers directly.

<a id="scoping"></a>

## Scoping

Owner is per row.

| Object | Scope |
|---|---|
| Goal | Owner columns on Goal row |
| Self-Perspective | Owner columns on Memory row |
| `core/inspires` edge | same Owner as endpoints |
| `core/motivated-by` edge | same Owner as endpoints |
| Lifecycle Fact | same Owner as Goal write |

Cross-owner Goal assignment and cross-owner evidence are rejected.
Owner means principal — org is a billing annotation, not part of
Owner (doc 01 §Owner, renegotiated 2026-06-11).

## Authorship

Goal authorship:

| Author | Use |
|---|---|
| `User` | direct user Goal writes |
| `External` | outside-agent Goal writes |
| `System(Tool)` | tool-authored lifecycle close |
| `System(Operator)` | A->Goal operator output |

Memory authorship remains separate. Personality-authored Memory rows
carry personality identity and wake-chain depth. Operator-invocation
reproducibility is row metadata, not citation.
