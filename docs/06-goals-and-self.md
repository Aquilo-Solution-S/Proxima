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
| `authorship` | `User`, `System`, or `External` |
| `request_id` | Owner-scoped idempotency key |

States:

| State | Active set | Terminal | Meaning |
|---|---:|---:|---|
| `Active` | yes | no | Live direction |
| `Paused` | no | no | Suspended direction |
| `Achieved` | no | yes | Positive close |
| `Abandoned` | no | yes | Post-active negative close |

Goal-to-Goal decomposition, dependency, and inspiration are ordinary
Edge topology with relation descriptors, not Goal row fields.

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

Public Rust surface:

| Caller | API | Notes |
|---|---|---|
| Embedded host / product app | `Engine::create_goal` + `GoalCreateRequest::product` | typed `GoalPayload`, stable `IdempotencyKey`, no table knowledge |
| MCP client | `core_goal action=set` | tool-shaped JSON wrapper over the same storage atom |
| Storage implementer | `CreateGoalAtomicRequest` | low-level atom; not the preferred host API |

`GoalCreateRequest::product` defaults to `GoalAuthorship::User` because
an embedded product flow writes on behalf of the authenticated owner.
System-originated host flows may override authorship explicitly; External
authorship still cannot seed concrete Goal state. The host must pass
`target_self_perspective_id`; assignment is explicit because
`active_goals(instance)` is defined through `core/inspires` edges to the
Self Perspective. Simple owner-scoped, unassigned Goal creation remains
out-of-scope until the Self query model changes.

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

Assignment means "this Goal may inspire this Self." It is a causal
edge (`core/inspires`), not a structural membership edge. It does not
imply execution, obligation, repository scope, or wake policy.

## Goal Core Boundary

Core owns:

| Surface | Owned by core |
|---|---|
| Entity | Goal row, Goal identity, 4-state lifecycle |
| Payloads | `GoalPayload` schemas |
| Verb | `GoalWrite` |
| Lifecycle Facts | activated / paused / achieved / abandoned schemas |
| Tools | `core_goal` action dispatcher: `set`, `transition` (pause / resume / abandon), `modify`, `mark_achieved`, `decompose` |
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
| `core/inspires` edge | source-owned: edge Owner = source Goal Owner; descriptor/write policy may tighten target scope |
| `core/motivated-by` edge | source-owned: edge Owner = source Goal Owner; descriptor/write policy may tighten target scope |
| Lifecycle Fact | same Owner as Goal write |

Cross-owner Goal assignment/evidence is relation/write-surface policy,
not the global Edge ownership law. Owner means the owning Group — org is
not part of Owner.

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
