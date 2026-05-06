# Goal flavor + proposal pipeline + cross-flavor evidence grounding

Spec date: 2026-05-06. Topic: introduce `flavors/goal/` as the reference
flavor for the intentional layer; ship a Goal **proposal/accept/decline
pipeline** with `External` authorship via MCP; ship typed `MotivatedBy`
edges that ground proposed Goals on cross-flavor Abstractions/Facts.

## Why now

- Decider has nothing intentional to optimise without Goals
  ([06](../../06-goals-and-self.md), [10](../../10-configuration.md)).
- A→Goal is on the roadmap as the third operator family (per typed-goals
  decision; codified by `OperatorKind::AtoGoal` in
  `crates/core/src/verbs/goal_write.rs`). The agent-driven proposal
  pipeline is the MCP mirror of that operator family; both write the
  same shape.
- Goal-related typed payloads + relations + tools naturally cluster into
  a reference flavor. Putting them in `flavors/mcp/` would conflate
  "substrate primitives" with "intentional-layer primitives." Putting
  them in core would re-open invariant 8 ("flavor crate is the unit of
  inclusion").

## Non-goals

- Engine-scheduled `OperatorKind::AtoGoal` operator family (separate
  spec; reuses `MotivatedBy`).
- Cross-domain Perspective consolidation.
- `MotivatedBy` typed `EdgePayload` sidecar (additive enrichment; lands
  later without breaking v1 consumers).
- Goal→Goal `MotivatedBy` (layering allows but no v1 use case;
  `parent_goal_ids` already covers Goal→Goal DAG of motivation between
  Goals — `MotivatedBy` is exclusively Goal→{A, F}).
- Soften invariant 20 (F→A intra-flavor). Cross-flavor synthesis goes
  through the substrate-flavor + cross-flavor A→A pattern, unchanged.

## Prerequisite

The **frontend flavor co-location migration** (separate plan in
`.plans/`) lands first. It:

- Moves existing Code + MCP frontend slices from
  `packages/frontend-core/src/flavors/` to `flavors/<name>/frontend/`
  (matching the `flavors/*/frontend` workspace pattern already declared
  in `pnpm-workspace.yaml`).
- Adds extension hooks to `packages/frontend-core/`:
  `registerPayloadRenderer`, `registerEdgeStyle`,
  `registerShellView`.
- Establishes the shell's flavor-init composition pattern (each linked
  flavor's frontend bundle exports `init()`; shell calls them at
  startup).

This spec assumes that work is done — Goal's frontend slice ships at
`flavors/goal/frontend/` from day 1 using the registration hooks.
frontend-core gains no Goal-specific code.

## Current state in the codebase (audited 2026-05-06)

These already exist and are reused as-is:

| Symbol | Location | Notes |
|---|---|---|
| `GoalState { Active, Paused, Achieved, Abandoned }` | `crates/core/src/verbs/goal_write.rs:13` | Spec **extends** with `Proposed` and `Rejected`; existing values keep their meanings |
| `GoalAuthorship { User, System(SystemOrigin), External }` | `crates/core/src/verbs/goal_write.rs:41` | `External` variant already wired; no new authorship type needed |
| `OperatorKind::AtoGoal` | same file:21 | Confirms A→Goal as an existing operator-kind reservation |
| `GoalDraft { …, parent_goal_ids, authorship, request_id, … }` | same file:48 | Existing write shape; **add** `supersedes_goal_id` and `payload` (see Prerequisites) |
| `GoalRow { …, state, supersedes, payload: Vec<u8>, … }` | `crates/core/src/verbs/query.rs:97` | Read shape already carries `supersedes` and `payload` (CBOR sidecar bytes) |
| `goals.state` text column + `goals.supersedes` uuid column | `crates/storage-pg/src/verbs/query/rows.rs:156` | DB columns already present; migration only needs to extend the `state` CHECK + add transition trigger |
| `external_agent_authorship` migration | `crates/storage-pg/migrations/20260506000030_external_agent_authorship.sql` | Already wires External authorship for memory writes; extend to Goal writes |

Gaps the spec must close:

- **`GoalPayload` trait is documented but not implemented**
  (`docs/06-goals-and-self.md:86`; absent from `crates/core/src/payload.rs`
  alongside the existing `FactPayload`, `AbstractionPayload`,
  `PerspectivePayload`, `EdgePayload` traits).
- **`GoalDraft` lacks `supersedes_goal_id`** (read shape has it, write
  shape doesn't expose it). Required for the supersession-via-write
  pattern that Accept/Modify/Decline use.
- **`GoalDraft.text: String`** is the only payload-bearing field on the
  write side. Needs a `payload: Vec<u8>` (or typed equivalent) for
  typed-payload goals.

## Architecture

### Layering — what lives where

| Layer | Crate | Owns |
|---|---|---|
| Core | `crates/core/`, `crates/storage-pg/` | Goal entity, `GoalState` enum (extended), `GoalAuthorship`, `GoalPayload` trait (NEW), `GoalWrite` verb (extended) |
| Substrate flavor | `flavors/mcp/` | Substrate primitives only — `proxima_remember` / `_derive` / `_link` / `_search_graph` / `_open`. **No Goal-specific tools.** |
| Goal flavor | `flavors/goal/` *(new)* | `MotivatedBy` (+ future `Blocks`, `Refines`) RelationDescriptors; reference GoalPayload schemas; MCP tools `goal_propose` and (optional) `goal_accept` / `goal_modify` / `goal_decline`; flavor migrations |
| Code flavor | `flavors/code/` | Unchanged in v1; can register `code_refactor_goal` payload later |
| Frontend | `flavors/goal/frontend/` *(new pnpm workspace member `@proxima/flavor-goal`)* | Typed payload renderers; propose-card with evidence chips; Proposed inbox view; flavor `init()` calling the frontend-core registration hooks |

Invariants preserved: 1, 7, 8, 11, 12, 13, 16, 20.

### Lifecycle (state machine, extending existing `GoalState`)

```
                External | System | User
   (none) ───────────────────────────────► Proposed
                                              │
                                  User        │ User
                                ┌─────────────┼──────────────┐
                                ▼             ▼              ▼
                            Active        Active         Rejected
                            (modify-       (accept)       (terminal,
                             supersede)                    new in spec)
                                │
                          User
                ┌──────────┬────┴────┬──────────────┐
                ▼          ▼         ▼              ▼
            Paused      Achieved   Abandoned      Active
                       (terminal+)(terminal-)    (modify)
```

State semantics:

| State | Existing | Meaning |
|---|---|---|
| `Proposed` | NEW | Awaiting user gate. Excluded from `G_active(ω)`. |
| `Active` | yes | In the active set. |
| `Paused` | yes | Suspended; not in `G_active(ω)`; reactivatable. |
| `Achieved` | yes | Terminal positive. |
| `Abandoned` | yes | Terminal negative (post-active "gave up"). |
| `Rejected` | NEW | Terminal — gate-time decline. Distinct from `Abandoned` to keep audit signal clear: `Rejected = "user said no at the gate"`, `Abandoned = "user tried it and gave up."` |

Active set:

```
G_active(ω) := { g ∈ goals(ω) | head_supersession(g) ∧ g.state = Active }
```

### State transition matrix (DB CHECK + trigger)

| prior | new | author allowed |
|---|---|---|
| (none) | Proposed | `External`, `System(_)`, `User` |
| (none) | Active | `User` only |
| Proposed | Active | `User` only |
| Proposed | Rejected | `User` only |
| Active | Active | `User` only (modify) |
| Active | Paused | `User` only |
| Active | Achieved | `User`, `System(_)` (operator-detected achievement) |
| Active | Abandoned | `User` only |
| Paused | Active | `User` only |
| Paused | Abandoned | `User` only |
| Achieved | * | forbidden (terminal) |
| Abandoned | * | forbidden (terminal) |
| Rejected | * | forbidden (terminal) |

The `(prior, new)` pair plus author kind is enforced by a Postgres
trigger that reads the prior row from `supersedes` (or treats
`supersedes IS NULL` as the "(none)" branch).

## Components

### Core changes

```rust
// crates/core/src/verbs/goal_write.rs (extend)
pub enum GoalState {
    Proposed,    // NEW
    Active,
    Paused,
    Achieved,
    Abandoned,
    Rejected,    // NEW
}

pub struct GoalDraft {
    pub owner:               Owner,
    pub schema_id:           SchemaId,
    pub schema_version:      SchemaVersion,
    pub text:                String,                // existing
    pub payload:             Vec<u8>,               // NEW — CBOR sidecar bytes
    pub state:               GoalState,
    pub parent_goal_ids:     Vec<GoalId>,
    pub supersedes_goal_id:  Option<GoalId>,        // NEW — supersession write path
    pub authorship:          GoalAuthorship,
    pub request_id:          String,
}

// crates/core/src/payload.rs (extend)
pub trait GoalPayload: 'static {
    const SCHEMA_ID: SchemaId;
    const SCHEMA_VERSION: SchemaVersion;
    fn encode(&self) -> Vec<u8>;          // CBOR
    fn decode(bytes: &[u8]) -> Result<Self, DecodeError> where Self: Sized;
}
```

`GoalWrite` verb gains:
- transition validation against the matrix
- `External` authorship constrained to `state = Proposed`
- supersession constraints (same Owner; prior row must be a head; prior
  state pair must be in the matrix)
- payload schema validation against registered `GoalPayload` schemas

### Storage migration

```sql
-- crates/storage-pg/migrations/<ts>_goal_proposed_rejected.sql

-- 1. Extend the state CHECK (existing column, existing constraint replaced)
ALTER TABLE goals DROP CONSTRAINT IF EXISTS goals_state_check;
ALTER TABLE goals ADD CONSTRAINT goals_state_check
    CHECK (state IN ('proposed', 'active', 'paused', 'achieved', 'abandoned', 'rejected'));

-- 2. Inbox query support
CREATE INDEX goals_proposed_inbox_idx
    ON goals (owner_principal_kind, owner_principal_id, owner_org_id, created_at DESC)
    WHERE state = 'proposed';

-- 3. Transition trigger — enforces (prior.state, new.state, new.author) matrix
CREATE OR REPLACE FUNCTION goals_validate_transition()
RETURNS trigger LANGUAGE plpgsql AS $$
DECLARE
    prior_state text;
BEGIN
    IF NEW.supersedes IS NULL THEN
        -- (none) → state
        IF NEW.state IN ('active', 'paused', 'achieved', 'abandoned')
           AND NEW.author_kind NOT IN ('user', 'system')
        THEN
            RAISE EXCEPTION 'goal: only User/System may seed state=%', NEW.state;
        END IF;
        IF NEW.state = 'rejected' THEN
            RAISE EXCEPTION 'goal: cannot create directly with state=rejected';
        END IF;
        RETURN NEW;
    END IF;

    SELECT state INTO prior_state FROM goals WHERE id = NEW.supersedes;
    IF prior_state IS NULL THEN
        RAISE EXCEPTION 'goal: supersedes references unknown id';
    END IF;
    IF prior_state IN ('achieved', 'abandoned', 'rejected') THEN
        RAISE EXCEPTION 'goal: state=% is terminal', prior_state;
    END IF;

    -- Per-pair table — see spec §"State transition matrix"
    IF NOT goals_pair_allowed(prior_state, NEW.state, NEW.author_kind) THEN
        RAISE EXCEPTION 'goal: forbidden transition %→% under author=%',
            prior_state, NEW.state, NEW.author_kind;
    END IF;

    RETURN NEW;
END;
$$;

CREATE TRIGGER goals_transition_check
    BEFORE INSERT ON goals
    FOR EACH ROW EXECUTE FUNCTION goals_validate_transition();
```

`goals_pair_allowed(prior, next, author_kind)` is a small SQL function
encoding the matrix table; trivially testable with `SELECT * FROM
unnest(...)` fixtures.

### `flavors/goal/` (new crate)

```
flavors/goal/
├── Cargo.toml
├── migrations/<ts>_goal_flavor_payloads.sql      # sidecar tables for reference payloads
├── src/
│   ├── lib.rs                                    # registers payloads, relations, tools
│   ├── payloads/
│   │   ├── mod.rs
│   │   ├── simple_text_goal.rs                   # GoalPayload — { text: String }
│   │   └── task_goal.rs                          # GoalPayload — { title, due_at?, priority? }
│   ├── relations/
│   │   ├── mod.rs
│   │   └── motivated_by.rs                       # RelationDescriptor
│   └── tools/
│       ├── mod.rs
│       ├── propose.rs                            # goal_propose
│       ├── accept.rs                             # goal_accept (optional MCP symmetry)
│       ├── modify.rs                             # goal_modify
│       └── decline.rs                            # goal_decline
└── tests/
    ├── registry_pg.rs
    ├── propose_smoke.rs
    └── accept_decline_pg.rs
```

`MotivatedBy` RelationDescriptor (registered by Goal flavor; engine
validates per invariant 13):

```rust
RelationDescriptor {
    id:                   "goal/motivated-by",
    label:                "motivated by",
    source_kind:          EntityKind::Goal,
    allowed_target_kinds: &[EntityKind::Abstraction, EntityKind::Fact],
    is_cross_domain:      true,    // explicit: cross-flavor targets allowed
}
```

Distinct from existing `parent_goal_ids` on Goal: that field is the
**Goal-DAG** (Goal→Goal motivation between Goals, e.g. sub-goals).
`MotivatedBy` is **Goal→{A, F}** evidence grounding — different kind,
different layer.

### MCP tool — `goal_propose`

Atomic Goal + MotivatedBy edges (fork **2b**):

```rust
// flavors/goal/src/tools/propose.rs
ToolFunction {
    name:        "goal_propose",
    description: "Propose a Goal for the user to accept, modify, or decline.\
                  Provide evidence: ids of Abstractions or Facts that justify\
                  the Goal. The user reviews proposals in the Inbox before they\
                  become active.",
    parameters: {
        owner:    Owner,                   // session-derived, not LLM-visible
        payload:  GoalPayloadJson,         // schema_id + typed body
        evidence: Vec<EntityId>,           // A or F ids in same Owner
    }
}
```

Engine actions, in one tx:
1. Validate `payload.schema_id ∈ registered GoalPayload schemas`.
2. Validate `evidence ⊆ entities(owner)`; reject if any id is in another
   Owner (invariant 4).
3. Insert Goal row: `state=Proposed`, `authorship=External`,
   `supersedes_goal_id=None`.
4. Insert `MotivatedBy` edges: source = new Goal id, target = each
   evidence id; `is_cross_domain` permits flavor mixing.
5. Emit `ChangeEvent::EntityAppend` for the Goal and one per edge.

Empty `evidence` is allowed (rare evidence-light proposals) but the tool
description discourages it. Inbox renders empty-evidence proposals
without chips — visible gap, no hard rejection (per fork **2b** vs 2c).

### Optional MCP tools — `goal_accept` / `goal_modify` / `goal_decline`

Mirror the user-driven path so programmatic flows (test harnesses,
future integrations) can drive the pipeline end-to-end. v1 frontend
should NOT call these — frontend uses the existing `GoalWrite` verb
directly to keep `User` authorship attribution at the wire level.

**Default: ship all four**; engine-side authorship validation (the
trigger refuses `External`-authored Active/Rejected) keeps it safe.

## Data flow

### Propose (agent via MCP)

```
agent ──goal_propose────────► engine
                              │
                              ├─ validate payload schema
                              ├─ validate evidence ids in same Owner
                              ├─ INSERT goal (state=Proposed, authorship=External)
                              ├─ INSERT motivated_by edges (Goal→evidence)
                              └─ emit ChangeEvent ──► Inbox subscribers
```

### Accept

```
user click ──► frontend GoalWrite { supersedes_goal_id=proposal_id, payload, state=Active, authorship=User }
                                       │
                                       ├─ trigger: prior is Proposed head → (Proposed, Active, User) allowed
                                       ├─ INSERT new goal row + supersession link
                                       ├─ COPY motivated_by edges (re-emit targeting new Goal id)
                                       └─ emit ChangeEvent
```

**MotivatedBy on accept — re-emit, don't traverse.** Edges target a
specific entity id; supersession changes the id. Making queries traverse
the supersession chain to find evidence hides complexity in every
consumer. Re-emit on accept: engine reads prior head's outgoing
MotivatedBy and recreates them targeting the new Goal id. The proposal
row keeps its edges (audit trail intact).

### Modify-then-Accept

Same as Accept but with edited payload **and** edited evidence (per UX
default — see §Frontend UX). The re-emit step uses the user-supplied
evidence list rather than copying from the proposal.

### Decline

Same as Accept but `state=Rejected`. Trigger enforces terminal — no
further supersession.

### Direct user Goal (no proposal)

```
frontend ──► GoalWrite { payload, state=Active, authorship=User, supersedes_goal_id=None }
```

No proposal, no MotivatedBy required (allowed if user wants to attach
evidence manually).

## Validation summary

| Rule | Enforcement |
|---|---|
| State transitions match the matrix | DB trigger `goals_validate_transition` |
| `Achieved` / `Abandoned` / `Rejected` are terminal | trigger (rejects supersession of these) |
| `External` may only author `state=Proposed` | trigger |
| Direct seed of `state=Rejected` rejected | trigger |
| `MotivatedBy.source_kind = Goal` | RelationDescriptor (engine, invariant 13) |
| `MotivatedBy.target_kind ∈ {A, F}` | RelationDescriptor (engine) |
| Cross-Owner evidence rejected | engine (invariant 4/16; pre-existing) |
| `External` authorship resolves to authenticated MCP session's agent_id | already wired by `external_agent_authorship.sql`; extend to Goal writes |
| Payload `schema_id` ∈ registered GoalPayload schemas | engine (new — depends on `GoalPayload` trait landing) |

## Frontend UX

### Inbox view (new top-level shell route)

Lives at `flavors/goal/frontend/src/views/inbox/`; registered with the
shell via `registerShellView` from the Goal flavor's `init()`. Sibling
of Atlas. Owner-scoped via existing `Subscribe` stream filtered on
`EntityKind::Goal ∧ state=Proposed`.

Per-row card:
```
┌───────────────────────────────────────────────────────────────┐
│ <typed payload renderer>                                      │
│                                                               │
│ Why · [chip] [chip] [chip]   …each = A or F id                │
│                                                               │
│        [ Accept ]  [ Modify ]  [ Decline ]                    │
└───────────────────────────────────────────────────────────────┘
```

- Chip click/hover → `proxima_open` to preview evidence inline.
- `Modify` expands an inline editor exposing **both** the typed payload
  form and the evidence chip strip (chips become removable, plus an
  "add evidence" affordance via entity picker). On save → Accept path
  with edited payload + edited evidence.
- Empty-evidence proposals render with no chip strip — gap is visible.

### Atlas additions

Atlas itself stays in frontend-core. The Goal flavor's `init()` calls
`registerEdgeStyle` for `goal/motivated-by` and per-state node styling
hooks for Goal kinds — no edits to frontend-core's Atlas source.

- Goal node styling by state:
  - `Proposed` — dashed outline
  - `Active` — solid fill
  - `Paused` — solid fill, reduced opacity
  - `Achieved` — solid fill, success accent
  - `Abandoned` / `Rejected` — greyed
- `MotivatedBy` edges rendered with the typed-edge style (per typed-goals
  memory).

### Goal CRUD (existing path)

`New Goal` button → typed payload form → writes Active under
`User` authorship directly via `GoalWrite`. No proposal involved.

## Testing

| File | Covers |
|---|---|
| `crates/storage-pg/tests/goal_state_transitions_pg.rs` | Trigger matrix exhaustively — every (prior, next, author) cell; rejects `External`-authored Active; rejects supersession of terminal states |
| `crates/storage-pg/tests/goal_g_active_pg.rs` | `G_active(ω)` filters by `state=Active` under supersession + author rules |
| `flavors/goal/tests/registry_pg.rs` | `GoalPayload` + `RelationDescriptor` registration sanity |
| `flavors/goal/tests/propose_smoke.rs` | `goal_propose` writes Goal + MotivatedBy atomically; rejects evidence in another Owner; rejects unregistered payload schema |
| `flavors/goal/tests/accept_decline_pg.rs` | Accept supersedes + re-emits MotivatedBy; Decline is terminal; Modify edits payload + evidence then accepts |
| `apps/proxima-mcp/tests/end_to_end.rs` (extend) | MCP `goal_propose` round-trip emits `ChangeEvent` |
| `flavors/goal/frontend/src/views/inbox/index.test.tsx` | Inbox renders Proposed rows; Accept/Modify/Decline call right write paths |
| `flavors/goal/frontend/src/renderers/payload-renderers.test.tsx` | Typed payload rendering for `simple_text_goal`, `task_goal` |

## Migration / rollout sequence

Order matters: prerequisites land first.

1. **`GoalPayload` trait** in `crates/core/src/payload.rs` (alongside
   existing payload traits). Add `schema_id` / `schema_version` /
   `encode` / `decode`. No registration yet.
2. **`GoalDraft` write-side fields**: add `payload: Vec<u8>` and
   `supersedes_goal_id: Option<GoalId>`. Wire through storage's
   GoalWrite implementation.
3. **`GoalState` extension**: add `Proposed` and `Rejected` variants.
4. **Storage migration**: extend the `state` CHECK + add transition
   trigger + inbox index.
5. **Extend `external_agent_authorship` wiring** to Goal writes.
6. **`flavors/goal/` crate**: scaffold + `MotivatedBy` descriptor +
   reference payload schemas + tests.
7. **Wire `flavors/goal/`** into `apps/proxima-shell/` composite binary
   (per doc 13 composite discipline).
8. **MCP `goal_propose` tool** (then optional `goal_accept` / `_modify`
   / `_decline`).
9. **`flavors/goal/frontend/` package**: payload renderers + Inbox view +
   flavor `init()` registering payload renderers, edge styles, and the
   shell view via the hooks shipped by the prerequisite migration.
10. **Wire `@proxima/flavor-goal` `init()`** into the shell's flavor
    composition entrypoint.
11. **E2E smoke**: agent proposes → user accepts/declines from Inbox.

## Open questions for plan-time decisions

1. Ship `goal_accept` / `goal_modify` / `goal_decline` MCP tools in v1
   alongside `goal_propose`, or defer? *Default: ship all four.*
2. Should `Modify` allow editing evidence (add/remove chips) or treat
   evidence as proposal-immutable? *Default: allow editing.*
3. Should the `MotivatedBy` re-emit on Accept also create a
   `derived-from-proposal` edge from the Active Goal back to the
   Proposed Goal id? *Default: no — supersession link is sufficient.*
4. Does `text: String` on `GoalDraft` survive alongside the new
   `payload: Vec<u8>`, or is `text` derived from a renderer over
   `payload` going forward? *Default: keep both for now; deprecate
   `text` once all goal writes go through typed payloads.*
