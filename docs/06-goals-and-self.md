# 06 — Goals & Self

Personality `Π = PersonalityFlavor::snapshot(owner)` — see
[02 §Personality](02-memory.md#personality). 02 covered Perspectives
and the substrate's hosting of plural personalities. 06 covers Goals
(core) and Self (flavor projection).

Goals are a **core entity**, not a Memory kind, and not a flavor concern.
The Spinning Wheel (universe.md) commits to four pillars — Self,
Reality, Perspective, Goals — and the wheel does not spin without
Goals as the gravitational center. Every Proxima binary compiles
the Goal entity. What flavors do with Goals via typed payloads is up
to them; whether Goals exist is not a per-binary choice.

Self, by contrast, *is* a flavor concern. The substrate ships no Self
entity, no canonical projection, and no fixed query — different
personality flavors emit different Selves over the same memory graph.
See §Self.

## Goal entity

Distinct entity. Core ships the entity shape; flavors extend the
typed payload via `GoalPayload` (see §Typed payloads).

```rust
struct Goal {
    id:              GoalId,                  // UUIDv7
    owner:           Owner,                    // scope (see [01](docs/01-event-source.md))
    schema_id:       SchemaId,                 // NOT NULL — every Goal carries a typed payload
    text:            String,                    // human-authored or operator-authored prose; the playing space
    state:           GoalState,
    parent_goal_ids: Vec<GoalId>,             // DAG
    supersedes:      Option<GoalId>,           // prior version
    authorship:      GoalAuthorship,
    created_at:      Timestamp,
}

enum GoalState {
    Active,
    Paused,
    Achieved,    // terminal
    Abandoned,   // terminal
}

enum GoalAuthorship {
    User,                                       // explicit UI write
    System { origin: SystemOrigin },             // synthesized — by operator or by tool extraction
    External,                                    // reserved (future API integrations)
}

enum SystemOrigin {
    Operator {
        operator_id:            OperatorId,    // A→Goal-derived ([04](docs/04-consolidation.md))
        operator_kind:          OperatorKind,   // AtoGoal
        model_id:               ModelId,
        prompt_version:         PromptVersion,
        personality_id:         PersonalityId,  // which personality flavor produced this (08)
        personality_state_hash: Hash,           // that flavor's snapshot at run time
    },
    Tool { tool_id: ToolId },                 // tool-emitted (e.g. conversation extractor, 05)
}
```

Storage layout for this entity is defined in [07-storage.md](docs/07-storage.md).

The enum design eliminates all `Option<>` chains: the conditional CHECK
that `authorship_kind=System => operator-invocation columns NOT NULL only
when origin=Operator` becomes the structural shape of the nested enums.
The "you can't construct a User-authored Goal with operator metadata"
guarantee moves from runtime CHECK to the type system. Operator-invocation
columns mirror those on Memory rows for A/P (02 §Core entity): they
record reproducibility metadata for A→Goal outputs. They enter the
A→Goal invocation key (04 §Idempotence).

## Typed payloads — `GoalPayload`

Every Goal carries a typed payload via the `GoalPayload` trait — the
sixth payload trait alongside FactPayload / AbstractionPayload /
PerspectivePayload / CitedObjectPayload / CitationMappingPayload (03,
08 §Payload traits, 11). Selective extraction: the typed payload
captures queryable scaffolding only; `Goal.text` remains the playing
space. No JSON escape; required fields only. Build-time registration
via `proxima_flavor!` ([08](docs/08-core-and-flavors.md)).

```rust
trait GoalPayload {
    const SCHEMA_ID: &'static str;       // e.g. "code/needs-implementation"
    const SCHEMA_VERSION: u32;           // const on impl; version implicit in sidecar table name
    const SPECIAL_CATEGORY: bool;        // see [03 §Special-category declaration](03-schema-registry.md#special-category-declaration)
}
```

Stored in a per-schema sidecar `goal_<schema_id>_v<n>` joined by
`goal_id`. Version is implicit in sidecar table membership. Same
migration discipline as 03.

Bare core registers `GoalCoreV1` — a minimal payload (engine-relevant
scaffolding only, no domain fields) for goals that don't need a
flavor-specific shape. Flavors register richer schemas:

- `code/needs-implementation { ticket_ref, blocking: bool }`
- `code/needs-review        { pr_ref, severity }`
- `learning/master-topic    { topic_id, target_proficiency }`
- `legal/case-resolution    { case_id, deadline }`

This is the extensibility hook for capability-based dispatch. A tool's
`availability` predicate can match on goal payload kind:

```rust
availability: |ctx| ctx.active_goal_payload_is::<NeedsImplementation>()
```

— giving the cortex-platform "one IsFeasible predicate" property
(see project_typed_goals memory) without porting cortex's apparatus
into core.

## Hierarchy — DAG

`parent_goal_ids: Vec<GoalId>` — multiple parents allowed (e.g.
"learn German" under both "improve cognition" and "travel to
Germany"). No single tree root. Each user has a forest of their own
goals.

## Lifecycle — supersession only

No mutation. State transitions write a new Goal with `supersedes`
pointing back. Pause / resume / achieve / abandon are all
supersessions with the appropriate `state`.

Goal supersession sits alongside A/P supersession (02 §Re-derivation
and supersession) as one of the three supersedable kinds; **Facts are
not supersedable** — that asymmetry is intentional, not an oversight.
Stateful Fact projections express "current X" via head-by-natural-key
queries on the schema sidecar (03 §Stateful Fact schemas), not via
`supersedes`. The Goal lifecycle below is its own story.

Active set query:

```
G_active = { g ∈ Goal | g.state == Active ∧ ¬∃ g' ∈ Goal . g'.supersedes == g.id }
```

## Authorship paths

Three ways a Goal enters the system:

1. **User UI write.** `authorship = User`. Operator-invocation columns
   NULL. Founding-letter onboarding (per-flavor) and ad-hoc
   additions/edits both route through this path.
2. **Tool extraction.** `authorship = System { Tool { tool_id } }`. A
   conversation extractor tool (registered per [05](docs/05-actions.md)) emits a `SYSTEM`
   Action-Fact and writes a Goal in the same transaction. The
   Action-Fact's `core/motivated-by` edge points at the newly-created
   Goal — self-referential, the action *is* the goal's creation.
3. **A→Goal operator.** `authorship = System { Operator { operator_id } }`.
   Operator-invocation columns NOT NULL. Agent-discovered goals: an
   operator retrieves an A-set under Π and synthesizes one or more new
   Goals with provenance edges back to the input Abstractions.
   Specified in [04 §A→Goal operator](docs/04-consolidation.md#a-to-goal-operator).

User confirms / edits System-authored Goals post-hoc through the app's
UI; that correction is a separate `supersede_goal` write under
`GoalAuthorship::User`.

## Goals participate in the edge graph

Goals reference and are referenced by memories and other goals.
Edge endpoints use the `EntityId` sum type (`Memory | Goal`) defined
in 02 §Edges; relations involving Goals register the same way as
Memory-only relations, with `EntityKind::Goal` allowed in
`source_kind_mask` / `target_kind_mask`. Bare core registers:

| From → To | Relation | Class | Authorship |
|---|---|---|---|
| Goal → Abstraction | `core/derived-from` | Provenance | `OperatorAtoGoal(g)` — owned by the produced Goal ([04](docs/04-consolidation.md)) |
| Goal → Goal | `core/parent` | Structural | API (set on Goal write from `parent_goal_ids`) |
| Goal → Goal | `core/supersedes` | Supersedes | engine on re-derivation |
| Memory → Goal | `core/motivated-by` | Causal | `PerspectiveLink(p)` for P-mediated, or `EventSource(SYSTEM)` for tool-emitted Action-Facts |

The `core/motivated-by` edge is the **replacement for the prior
`motivated_by_goal: Option<GoalId>` payload field on Action-Facts**
([05](docs/05-actions.md)). The hint moves from a typed payload field to a typed edge —
same information, but addressable via the edge graph rather than
buried inside payload structures, and uniform across Action-Facts
emitted by any flavor.

The F/A/P layering rule (`layer(src) ≥ layer(tgt)`) still applies
*within* the F/A/P universe. Goals sit outside that layering — they
have their own edge rules above. Cross-edges (Goal ↔ Memory) carry no
layer constraint beyond the relation's source_kind_mask /
target_kind_mask.

## Self — flavor projection

No Self entity. No cache. No core operator. Self is a *projection*
defined per `PersonalityFlavor` and computed on demand:

```rust
trait PersonalityFlavor {
    // ... see [02 §Personality](02-memory.md#personality)
    fn project_self(&self, owner: Owner) -> SelfView;
}

struct SelfView {
    personality_id: PersonalityId,
    perspectives:   Vec<MemoryRef<Perspective>>,    // descriptive — "how I see"
    goals:          Vec<GoalRef>,                    // directive — "what I want to change"
    // additional flavor-specific fields permitted; the substrate hashes the
    // serialised view for change detection but does not interpret it
}
```

For Owner ω, asking "what is Self?" means asking *which Self* —
naming a `personality_id`. The substrate has no canonical Self; a
binary that links Stoic Visionary, Workhorse Programmer, and Tester X
projects three Selves over the same memory graph, all queryable, all
simultaneously real. This is the architectural commitment: *the agent
is not a self; the agent is a substrate that hosts selves* (see
universe.md / 13).

The two-arity structure (Perspective + Goal) survives the move to
flavor projection: a `SelfView` always carries Perspectives
(descriptive — *how I see*) and Goals (directive — *what I want to
change*) at minimum, honoring the universe.md commitment. A
Perspective is revised when Reality contradicts it; a Goal is revised
through achievement, abandonment, or Self-evolution. Both bias future
operators (via the snapshot — see [02 §Personality](02-memory.md#personality)),
but the agent's relationship to them — and the forces that update them
— differ.

What `project_self` may include *beyond* the two-arity minimum is the
flavor's call: a flavor might attach an identity-Perspective summary,
a goal-coverage scoring, an active-tools manifest, etc. The substrate
serialises the resulting `SelfView` for change-detection but does not
parse its flavor-specific fields.

Read scope: a personality's `project_self` always reads its own A/P/Goals
(diagonal of the read-scope matrix); cross-personality reads obey
`M[self][*]` per [02 §Read-scope matrix](02-memory.md#read-scope-matrix).
A flavor that wants to project a unified "what-everyone-thinks" Self
explicitly opts into all-personalities retrieval. Other personalities
asking "what does V's Self say?" get whatever V projects — they don't
bypass V's matrix.

## Scoping

Memories and Goals carry `Owner` (defined in 01). Queries and
consolidation operators run per Owner. Cross-owner edges not allowed
in v1.

Schema registry is binary-scoped (per [03 §Scoping](docs/03-schema-registry.md#scoping-one-namespace-per-binary)). `GoalPayload`
schemas register through the same mechanism as every other payload
trait.

## Goal-write API

```rust
fn write_goal(draft: GoalDraft, owner: Owner, author: GoalAuthorship) -> GoalId;
fn supersede_goal(prior: GoalId, draft: GoalDraft, author: GoalAuthorship) -> GoalId;
```

`supersede_goal` requires `prior.owner == draft.owner` — supersession
within Owner only. Both calls validate the typed payload against its
registered `GoalPayload` schema; unknown schema_id is rejected at the
API boundary, same as A/P payloads ([03](docs/03-schema-registry.md)).

**Cross-personality supersession.** When `prior.authorship` carries a
`personality_id` (operator-authored, A→Goal-derived) and `draft.authorship`
carries a *different* `personality_id`, the call is rejected unless
the author is `User` — cross-personality supersession is an editorial
gesture reserved for explicit user-API writes (see
[02 §Re-derivation and supersession](02-memory.md#re-derivation-and-supersession)).
Operators superseding their own personality's prior outputs is the
default, allowed path. Re-deriving under a *different* personality
produces a parallel goal, not a supersession.

## Bootstrap — per flavor, no engine root

No engine-level founding goal. Each flavor (single or composite) owns
its onboarding:

1. User signs up to the deployment (Memophant, Justitia, …).
2. Onboarding flow asks flavor-specific founding-letter questions.
3. Initial Goals are written via `write_goal` under that user's
   scope, with payloads typed against the flavor's registered
   `GoalPayload` schemas.

After onboarding, Goals continue to enter via UI (explicit user
creation), conversation extraction (system tools detect `"I need to
learn X"` and emit a Goal write as their tool effect), and A→Goal
operators (agent-discovered).

## Retrieval

`goal.text` is embedded. Vector index alongside structural columns
(`owner`, `state`, `schema_id`, `parent_goal_ids`, supersession
chain). Used for extractor dedup ("is there already a goal like
this?"), for A→P / A→Goal / decider context lookup, and for
capability-based dispatch (filter by payload schema).

## Conversation-extraction authorship

Extractor is a registered tool (build-time, like every tool — see
05/08). One invocation, one transaction:

- `SYSTEM` Fact under a `system-goal-extracted` schema.
- `Goal` record with `authorship = System { Tool { tool_id } }`.
- `core/motivated-by` edge from the Action-Fact to the new Goal —
  self-referential, the action *is* the goal's creation.

User confirms / edits the Goal post-hoc through the app's UI; that
correction is a separate `supersede_goal` write under
`GoalAuthorship::User`.

## Anchors

- `goal-entity`
- `typed-payloads-goalpayload`
- `hierarchy-dag`
- `lifecycle-supersession-only`
- `authorship-paths`
- `goals-participate-in-the-edge-graph`
- `self-flavor-projection`
- `scoping`
- `goal-write-api`
- `bootstrap-per-flavor-no-engine-root`
- `retrieval`
- `conversation-extraction-authorship`
