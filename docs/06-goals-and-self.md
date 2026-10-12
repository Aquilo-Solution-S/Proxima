# 06. Goals And Self

## Contract

| Surface | Contract |
|---|---|
| Goal | Direction: desired future state, lifecycle head, topology source |
| Memory | Observation or interpretation; never a Goal |
| Self | Query result, not entity |
| Assignment | `Goal.assignment_t` -> Perspective `t` |
| Wake policy | `Goal.wake_id : Option<WakeId>`; no wake entity |

<a id="goal-entity"></a>

## Goal Entity

`Goal` is a core entity.

Not Memory:

| Kind | Meaning | Lifecycle |
|---|---|---|
| Fact | observation | immutable |
| Abstraction | operator-authored synthesis | later `t` on one handle |
| Perspective | query/framing view | later `t` on one handle |
| Goal | intended direction | later `t` on one handle |

Goal series and version fields:

| Surface | Fields |
|---|---|
| `goal_head` | stable `handle`, registered `schema_id`, frozen `owner`, current `t` |
| Goal version | `handle`, UUIDv7 `t`, `owner`, `title`, nullable `text`, `state`, Owner-scoped `request_id` |
| Topology | `assignment_t`, `dependency_t[]`, `evidence_t[]` |
| Lifecycle/write | `close_fact_t`, optional `wake_id`, optional `write_act_t` |
| Body | optional schema-specific typed sidecar keyed by `t`; no payload bytes on the Goal row. The prompt `text` is the one body field the row keeps (below) |

States:

| State | Active set | Terminal | Meaning |
|---|---:|---:|---|
| `Active` | yes | no | Live direction |
| `Paused` | no | no | Suspended direction |
| `Achieved` | no | yes | Positive close |
| `Abandoned` | no | yes | Post-active negative close |

Goal-to-Goal decomposition, dependency, and inspiration are **Goal row
fields**: `dependency_t`, `evidence_t`, and `assignment_t`. The Goal owns the
statement, so those columns are its pins, written in its transaction; the Goal
side is rebuildable.

Lifecycle:

```
(none) -> Active
Active -> Active       # modification
Active -> Paused
Paused -> Active
Active -> Achieved
Active -> Abandoned
```

Every transition writes a new `t` on the same `handle`; `goal_head.t` advances.
There is no `supersedes` column and no in-place version mutation.
An owner erase is the only hard-delete path.

Active set:

```
G_active(owner) =
  current Goal heads
  where state = Active
```

### Goal text

`goal.text` is the prompt of that version, so a read returns it
without the sidecar. `NULL` means unknown, never empty: a version no replay
declaration can speak for (written before v0.0.13) stays `NULL`.

*Blank* has one definition, in the database: nothing is left after removing
ASCII whitespace (space, tab, line feed, carriage return, vertical tab, form
feed). The check `goal_text_nonblank_chk` refuses a blank text, and the
migration's backfill uses the same expression. The write spring
(`check_trimmed_len`: trim Unicode `White_Space`, then refuse empty) is
stricter and trims, so Rust never writes a text the database refuses. A text
the database admits but Rust would call blank, such as `U+00A0` alone, can only
come from input that skipped the spring (a hand-built `GoalPayloadWrite`, or a
row written outside the Goal write verbs); it is stored and read as it is.

Create, modify and each decomposed child store the text they were given, as
given: a hand-built draft with a blank text is refused by the check with a
`ConstraintViolation`, never stored as `NULL`. A transition (pause, resume,
abandon) and an achievement store exactly the prior version's stored text,
byte for byte and `NULL` included, with no judgement of it in Rust, so the text
of a series changes only at a modify.

The Goal write verbs carry the prior text by construction: the row they build
for a transition or an achievement takes its `text` from the prior version, and
the database does not check the carry. `write_goal`, the low-level writer,
stores the `text` of the `GoalWriteCommand` it is given and has no production
caller: it is a test fixture, available with the `test-fixtures` feature. The
foreign key `goal_owner_matches_head_fk`, from `goal (handle, owner_id)` to
`goal_head (handle, owner_id)`, keeps every new version in a series of its own
owner. Referential checks do not go through row-level security, so no scope can
add a version to another owner's series. The key is `NOT VALID`: a row an
earlier raw insert put into another owner's series stays, and every new row is
checked.

A read also returns `set_at`, the time of the series' first version, and
`changed_at`, the time of the version read; a first version has
`set_at == changed_at`. Both come from the UUIDv7 `t`, not from a column.
`set_by` and per-version authors are not part of the row; a host that needs
them records them from the goal-change feed
([18 §Who wrote what](18-fact-outbox.md#who-wrote-what)).

<a id="goal-write-api"></a>

## Goal-Write API

`GoalWrite` is the protocol write surface for Goal rows.

Rules:

| Rule | Effect |
|---|---|
| Owner-scoped | caller must access `draft.owner` |
| Schema-checked | head `schema_id` resolves to `GoalPayload`; a sidecar is written only when that schema declares one |
| Append-only | create or append a later `t`; never update a version |
| Idempotent | same `(Owner, request_id, body)` returns same `GoalId` |
| Conflict-detecting | reused request id with different body fails |
| Stream-visible | successful write emits `announce` |

Head-advance constraints:

| Constraint | Rule |
|---|---|
| Same Owner | prior and new Goal share Owner |
| Current head | stale prior cannot be lifecycle head |
| Valid transition | prior state and new state pair is admitted |
| Payload typed | the new `t` satisfies the registered payload contract; its sidecar may be absent |

A transition is the Goal's later `t` on the same `handle`. A terminal
transition (`Achieved`, `Abandoned`) writes a close Fact: the one the new row
names in `close_fact_t`, in the same transaction. When the runtime has an
outbox intake, every goal write that inserts a Goal row (set, modify, each
decomposed child, pause, resume, abandon, achieve) also writes one
`core/goal-change-v1` Fact in that transaction, so the write reaches the feed
([18 §Goal changes on the feed](18-fact-outbox.md#goal-changes-on-the-feed)).
Without an intake it writes no such Fact.

A pause says what happens to the work a scheduler already queued for the Goal.
The transition request carries a `GoalTransitionKind`, which replaces
`next_state`:

| `GoalTransitionKind` | Next state | `pending` |
|---|---|---|
| `Pause { pending }` | `Paused` | `PendingWork::Keep` or `PendingWork::Discard`, required |
| `Resume` | `Active` | does not exist |
| `Abandon` | `Abandoned` | does not exist |

`Achieved` is not a transition: `mark_achieved` has its own verb. `pending`
exists only on `Pause`, so a resume or abandon that names it does not compile.
Proxima records the decision (in the change Fact and in the replay
declaration) and does not interpret it: `keep` means the scheduler holds the
queued work and runs it after a resume, `discard` means it drops the queued
work, and work already running finishes either way. A transition replayed under
the same request id with a different `pending` is a conflict, as is a pause
recorded before `pending` existed.

Public Rust surface:

| Caller | API | Notes |
|---|---|---|
| Embedded host / product app | `Engine::create_goal` + `GoalCreateRequest::product` | typed `GoalPayload`, stable `IdempotencyKey`, no table knowledge |
| MCP client | `core_goal action=set` | tool-shaped JSON wrapper over the same storage atom |
| Storage implementer | `CreateGoalAtomicRequest` | low-level atom; not the preferred host API |

`GoalCreateRequest::product` defaults its transient command metadata to
`GoalAuthorship::User`. System-originated flows may override that value for
admission checks; it is not persisted as Goal authorship. The host must pass an
assignment target because active-goal projection is defined through
`Goal.assignment_t`. Simple owner-scoped, unassigned Goal creation remains
out-of-scope.

Operator-authored A→Goal writes use
`GoalAuthorship::System(SystemOrigin::Operator)` and
require a nonempty list of Abstraction evidence. The `core_goal` set, modify,
and decompose actions resolve `A:<uuid>` handles before the Engine write. The
episode wrapper permits its local `derive` Abstraction and applies the same
rule to external handles. Host-authored writes, including completion through
`mark_achieved`, retain the kernel's Fact-or-Abstraction evidence contract.

<a id="self--flavor-projection"></a>

## Self -- Query Projection

There is no Self row. Self is not parameterless.

```
situatedSelf(owner, cue) =
  head Perspectives of owner that touch cue
  (t ∈ cue ∨ a pin of that P is in cue)

recall(owner, situation ∪ question) is how an agent retrieves this.
The owner-wide Perspective set is a candidate pool, not Self.
```

Kernel: `situatedSelf` / `cueTouches` (`Causa.Goals`). Question text is
the recall cue (protocol). Different cues ⇒ different Self.

Once a Perspective `t` is in hand, `active_goals` for that assignment
is unchanged.

Self is never cached as:

| Forbidden | Reason |
|---|---|
| Memory row | duplicates Perspective and Goal state |
| Goal row | confuses direction with identity |
| materialized causal chain | cache would become authority |

<a id="goal-assignment"></a>

## Goal Assignment

Assignment is a column on the Goal row:

```
goal.assignment_t -> the Perspective `t` this Goal inspires
```

One `reference` index entry is derived from it, Goal → Perspective. There is
no `GoalConnection` sidecar and no assignment edge to write: the Goal knows
who it inspires, so the statement lives on the Goal.

`active_goals(perspective_id, read_owners)`:

```
assigned = Goals where assignment_t = perspective_t
heads = select goal_head.t for each assigned Goal handle
return heads where state = Active and Goal owner is readable
```

Assignment means "this Goal may inspire this Perspective." It does not imply
execution, obligation, repository scope, or wake policy.

## Goal Core Boundary

Core owns:

| Surface | Owned by core |
|---|---|
| Entity | Goal row, Goal identity, 4-state lifecycle |
| Payloads | `GoalPayload` schemas |
| Verb | `GoalWrite` |
| Tools | `core_goal` action dispatcher: `set`, `transition` (pause with a required `pending` / resume / abandon), `modify`, `mark_achieved`, `decompose` |
| Topology | `assignment_t`, `dependency_t`, `evidence_t` on the Goal row; index entries are derived from them |
| Query | active-goal traversal |
| Renderers | Goal payload views |

Evidence:

```
goal.evidence_t -> the Facts and Abstractions this Goal rests on
```

One `reference` index entry per element.

The evidence layer follows the write attribution:

| Write | Evidence contract |
|---|---|
| operator-authored `set` / `modify` / `decompose` | nonempty Abstraction set; exact admitted `t` values are stored |
| host-authored Goal write | Fact or Abstraction set, including an empty topology where the host contract permits it |
| `mark_achieved` completion | nonempty Fact or Abstraction set; this is completion evidence, not A→Goal input |

An omitted `modify` evidence field carries the prior Goal's exact stored
`evidence_t` vector. The read is owner-scoped and does not join through Memory,
so a cooled, missing, or unreadable target cannot shorten the successor's
statement; reauthorization and the transactional write fail closed when the
whole vector cannot be admitted. Explicit `[]` is distinct from omission on
the operator MCP action and is rejected there.

Retained Goal validity may preserve cooled or witnessed pins; successor
admission requires hot, readable assignment, evidence, lifecycle, and carried
wake targets, so hydrate them first.

At the low-level contract, a named stale prior is a caller-fixable conflict;
an unnamed head-snapshot drift is retryable.

## Goal-Scoped Wake Policy

Goal assignment does not create wake behavior.

Wake is an optional Goal-owned config:

| Field | Value |
|---|---|
| Storage | `proxima_core.wake_config(wake_id)` referenced by `Goal.wake_id` |
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

Protocol reachability (see [14 §Core Memory MCP Surface](14-protocol-surface.md#core-memory-mcp-surface)):

| Half | Surface |
|---|---|
| arm / re-arm / disarm | `core_goal` `set`/`decompose` `wake`, `modify` `wake`/`clear_wake` (omit both = carry prior head's config forward) |
| candidate admission | `proxima://wake-candidates{?fact,limit}` over `Engine::list_goal_wake_candidates` |

PR6 does not add a scheduler, executor, runtime plugin body, tool table, or
tool invocation table. External harnesses plan and execute; emitted outputs
must be ordinary Facts written through FactIngest and recorded on the Goal:

```
goal.evidence_t <- emitted Fact   (one reference entry follows)
```

Goals do not bind repos, worktrees, commands, or workers directly.

<a id="scoping"></a>

## Scoping

Owner is per row.

| Object | Scope |
|---|---|
| Goal | Owner columns on Goal row |
| Perspective | Owner columns on Memory row |
| assignment entry | source-owned: edge Owner = source Goal Owner |
| evidence entry | source-owned: edge Owner = source Goal Owner |
| dependency entry | source-owned: edge Owner = source Goal Owner |
| WakeConfig | Goal-owned row; no independent Owner or handle |

Cross-owner Goal assignment/evidence is write-surface policy — the writer
needs write authority on the Goal and read authority on the target — not the
global edge ownership law, which is uniformly "the row is owned by the source
owner". Owner means the owning Group — org is not part of Owner.

## Write attribution

There is no Goal or Memory authorship column/blob. `GoalAuthorship` on current
Rust command DTOs is transient admission metadata only. Durable attribution is
a write-act Fact: Goals may pin it in `write_act_t`; produced Memories may name
it in `refs`.
