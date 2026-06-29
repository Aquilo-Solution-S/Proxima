# 06. Goals And Self

## Contract

| Surface | Contract |
|---|---|
| Goal | Direction: desired future state, lifecycle head, topology source |
| Memory | Observation or interpretation; never a Goal |
| Self | Query result, not entity |
| Assignment | `Goal --core/inspires--> Perspective` |
| Wake policy | `Goal.wake : Option<WakeConfig>`; no wake entity |

<a id="goal-entity"></a>

## Goal Entity

`Goal` is a core entity.

Not Memory:

| Kind | Meaning | Lifecycle |
|---|---|---|
| Fact | observation | immutable |
| Abstraction | operator-authored synthesis | supersession |
| Perspective | query/framing view | supersession |
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
`target_perspective_id`; assignment is explicit because active-goal
projection is defined through `core/inspires` edges to a Perspective
selector. Simple owner-scoped, unassigned Goal creation remains out-of-scope.

<a id="self--flavor-projection"></a>

## Self -- Query Projection

There is no Self row.

Runtime Self for a Perspective selector:

```
Self(perspective_id, read_owners) =
  Perspective row if readable
  + active_goals(perspective_id, read_owners)
```

The self anchor is a readable Perspective memory row. Self evolution is
ordinary Perspective supersession and Goal topology changes; no current-root
pointer is a runtime authority source.

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
Perspective
```

Storage endpoint kinds:

| Endpoint | Kind |
|---|---|
| source | `Goal` |
| target | `Perspective` |

No `GoalConnection` sidecar.

`active_goals(perspective_id, read_owners)`:

```
assigned = incoming core/inspires targets where target = perspective_id
heads = follow Goal supersession for each assigned source
return heads where state = Active and Goal owner is readable
```

Assignment means "this Goal may inspire this Perspective." It is a causal
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
| Relations | `core/inspires`, `core/motivated-by`, `core/wake-motivated-by` |
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

Wake is an optional Goal-owned config:

| Field | Value |
|---|---|
| Storage | `proxima_core.goal_wake_config(goal_id)` |
| Trigger | `FactSchema { schema_id, schema_version }` or `FactMemory { memory_id }` |
| Toolset | registered provider-safe tool ids or exact action leaf scope keys |
| Prompt | nonblank planning prompt |
| Hard memory | readable context memory ids checked at admission |

Candidate admission:

```
current Goal head
state = Active
wake IS SOME
trigger Fact is readable through its actual Owner
hard memories are readable through their actual Owners
wake.toolset subset-of actor ToolScope intersect deployment profile
```

PR6 does not add a scheduler, executor, runtime plugin body, tool table, or
tool invocation table. External harnesses plan and execute; emitted outputs
must be ordinary Facts written through FactIngest and linked:

```
Goal --core/wake-motivated-by--> emitted Fact
```

Goals do not bind repos, worktrees, commands, or workers directly.

<a id="scoping"></a>

## Scoping

Owner is per row.

| Object | Scope |
|---|---|
| Goal | Owner columns on Goal row |
| Perspective | Owner columns on Memory row |
| `core/inspires` edge | source-owned: edge Owner = source Goal Owner; descriptor/write policy may tighten target scope |
| `core/motivated-by` edge | source-owned: edge Owner = source Goal Owner; descriptor/write policy may tighten target scope |
| `core/wake-motivated-by` edge | source-owned: edge Owner = source Goal Owner; emitted Fact write authority checked separately |
| WakeConfig | Goal-owned row; no independent Owner or handle |
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

Memory authorship remains separate. Perspective attribution is represented
with `core/authored` edges. Operator-invocation proof carriers are deferred
to PR7; PR6 does not preserve row-level authorship ids as substitutes.
