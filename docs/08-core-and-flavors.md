# 08. Core And Flavors

<a id="decision"></a>
## Decision

Core is the Rust runtime framework core. It owns graph contracts,
build-time flavor registry, protocol verbs, Goal/Self/WakeConfig runtime,
agent long-term memory substrate, substrate MCP tools, embedding
capability vocabulary, and storage ports. Flavor crates contribute build-time
vocabulary. Composite binaries choose flavor crates at build time and
freeze the registry at startup.

Proxima is a framework: core plus linked flavor crates composed into an
app. Generic agent memory and Goal substrate are core; domain
vocabularies such as code remain flavors. There is no external
flavor/tool catalog and no runtime registration.

The formal kernel is `docs/lean/Causa/`, not the Rust crate
boundary.

## Boundary

```
runtime framework core (`proxima-core`)
  ids, Owner, Memory / Goal contracts
  the closed edge kinds and the edge index
  F/A/P and Goal payload traits
  core GoalPayload schemas + lifecycle Fact schemas
  frozen registry
  storage ports
  agent long-term memory sidecars + tools
  GoalWrite, active-goal queries, core Goal tools
  Goal-owned WakeConfig + candidate reads
  substrate MCP config tools

flavor crate
  typed payload schemas
  reference fields declared on those payloads
  MCP tools

composite binary
  links selected flavor crates
  calls register(&mut FlavorRegistry)
  freezes registry
  wires storage + embedding client + transports
```

No runtime registration tier. Schemas, tools, sources, and prompts are
build-time vocabulary. Connections are not vocabulary at all: a flavor
connects nodes only by declaring `references()` on its payloads, and the two
edge kinds are closed to core.

<a id="registration-mechanism"></a>
## Registration Mechanism

Every composite binary creates `FlavorRegistry::new()`, calls each linked
flavor's generated `register(&mut FlavorRegistry) -> Result<(), FlavorRegistryError>`,
then calls `try_freeze()`. Registry failure is typed startup failure.
`FlavorRegistryFrozen` is constructed only by successful `try_freeze()` and
has no post-freeze schema mutation surface.

Core registers substrate schemas, core GoalPayload schemas, Goal
lifecycle Fact schemas, and substrate MCP memory/config/goal tools in the
default registry. Flavor crates append
their own descriptors through `proxima_flavor!`.

<a id="macro-surface"></a>
## Macro Surface

`proxima_flavor!` emits one fallible registration function:

```rust
pub fn register(registry: &mut FlavorRegistry) -> Result<(), FlavorRegistryError>;
```

Supported keys:

| Key | Contract |
|---|---|
| `name` | Flavor id and required prefix for contributed ids. |
| `display_name` | Optional UI label; defaults to `name`. |
| `fact_schemas` | `FactPayload` sidecar schemas. |
| `abstraction_schemas` | `AbstractionPayload` sidecar schemas. |
| `perspective_schemas` | `PerspectivePayload` sidecar schemas. |
| `goal_schemas` | `GoalPayload` sidecar schemas. |
| `cited_object_schemas` | `CitedObjectPayload` sidecar schemas (see 11). |
| `citation_mapping_schemas` | `CitationMappingPayload` sidecar schemas (see 11). |
| `opaque_cited_object_schemas` | Untyped cited-object schema ids. |
| `opaque_citation_mapping_schemas` | Untyped citation-mapping schema ids. |
| `schema_capability_tags` | Build-time capability tags on registered payload schemas. |
| `mcp_tools` | Flavor tool descriptors projected to MCP; tool names must use the flavor prefix. A tool that declares `ACTION_ARG_SPECS` is a dispatcher: its actions become `tool:action` scope leaves rather than one whole-tool grant, and its `Args` must be an internally tagged enum tagged on `action` (see [12 §Action-Dispatch Tools](12-tool-manifest.md#action-dispatch-tools)). |

Unknown keys are compile errors. Macro-registered schemas start with
`name + "/"`; tool names start with `name + "/"` or `name + "_"`.

There is no `relations` or `edge_schemas` key. A flavor that needs its nodes
connected declares reference fields on the payload that owns the statement
(see [09](09-developing-flavors.md#declaring-references)); the index rows
follow.

## Public Tiers

| Tier | Import | Surface |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, Engine, AuthzContext};` | boot/migrate/serve/query |
| Host extra-table | `AppContext::{clone_pool_for_host, pg_tuning_for_host}` | wrap the pool + resolved query policy in a flavor store inside `FlavorApp::services`; tools see the store, never the pool |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FactPayload, Tool};` | schemas, reference declarations, sidecars, tools. No `PgPool` |

Root `proxima::*` is host-facing. Flavor authoring imports live under
`proxima::flavor`. Flavor crates depend on the `proxima` crate only
(same git selector — `tag` **or** `rev`, never mixed — as the host).
Do not add `proxima-core` / `proxima-storage-pg` / `proxima-auth-oidc`
unless you are a backend-owned adapter. Cargo treats `tag = "vX"` and
`rev = "<that tag's commit>"` as two sources and duplicates `proxima-core`.

Sidecar-only flavors call neither Host extra-table accessor. Extra tables the
flavor migrates are host-wired: clone the pool and query policy, wrap them,
insert the store on `FlavorServices`. `proxima_core.*` SQL stays denied in
flavor `src/`.

<a id="schema-namespacing"></a>
## Schema Namespacing

Flavor-owned ids use `flavor_id/local_name`.

| Id kind | Prefix rule |
|---|---|
| Schema ids | Flavor schemas start with `flavor_id + "/"`; core schemas start with `core/`. |
| Tool names | Flavor tools start with `flavor_id + "/"` or provider-safe `flavor_id + "_"`; substrate tools use provider-safe `core_...` names. |

`proxima_schema_id!("x")` expands to `CARGO_PKG_NAME + "/x"`.

<a id="flavor-metadata"></a>
## Flavor Metadata

Every macro invocation adds one `FlavorDescriptor`:

| Field | Contract |
|---|---|
| `flavor_id` | `name`; crate-level namespace prefix. |
| `display_name` | `display_name` or `flavor_id`. |
| `package_version` | Calling crate `CARGO_PKG_VERSION`. |
| `author` | First `CARGO_PKG_AUTHORS` entry, if present. |
| `provenance` | `Builtin` in v1; other wire variants are reserved compatibility values. |

The frozen registry exposes `list_flavors()` and `flavor(flavor_id)`.
Flavor metadata describes linked vocabulary; it does not define runtime
agent classes.

## Runtime Self/Wake Boundary

Self is a query over existing rows:

```
Self(perspective_id, read_owners)
  readable Perspective selector
  active Goal heads whose `assignment_t` names it
```

Wake is Goal-owned config:

```
Goal.wake_id = none | some WakeId
WakeConfig.toolset subset-of actor ToolScope intersect deployment profile
```

There is no flavor-owned runtime agent trait. A flavor may provide payload
schemas, the reference fields on them, and MCP tools. External harnesses own
model, execution, and dependency-satisfaction policy; core exposes candidate
reads, not an executor.

## Substrate MCP Surface

Default substrate memory surface:

| Read | Write |
|---|---|
| `proxima://memory/{id}` resource | `core_derive` |
| `core_search_memories` | `core_remember` |
| `proxima://memory/{id}/lineage` resource | `core_record_utterance` |
| neighbors / lineage | `core_interpret` |

Substrate MCP config tools are core-registered flat tools plus action
dispatchers for goals and Facts. Schema,
edge, tool, graph, memory-hydration, lineage, and event reads are MCP
resources.

Flavor MCP tools extend the MCP catalog, flat tools and action dispatchers
alike. Goal WakeConfig validation checks registered trigger/tool shape;
candidate reads apply actor and deployment tool-scope narrowing.

**Resources are flavor #0's, by design.** The `ResourceContract` entries on
flavor #0 are the whole resource catalog; `try_freeze` rejects a resource
declared by any other flavor, and `proxima://` dispatch is a closed `match`
in `mcp-server/src/server.rs` whose paths are `const`-evaluated out of those
same declarations. This
is not the gap that tools had: a flavor resource would need its own scope-key
namespace, a URI-template parser for its parameters, and a pagination
contract — a separate feature with its own design, not a missing forwarding
line. Nothing in the tool work above changes it.

Core exposes no edge-writing tool at all — not a generic one and not a
specific one. An edge follows from what a node says, so the write that owns
the statement is the only thing that can produce one.

## Goal Boundary

Core owns:

| Surface | Core contract |
|---|---|
| Entity | Goal identity and 4-state lifecycle (`Active`, `Paused`, `Achieved`, `Abandoned`) |
| Verb | `GoalWrite` create / later-`t` head advancement |
| Query | active-goal heads and assignment traversal |
| Topology | `assignment_t`, `dependency_t`, `evidence_t` on the Goal row; index entries are derived from them |
| Payloads | core `GoalPayload` schemas |
| Tools | `core_goal` action dispatcher: `set`, `transition`, `mark_achieved`, `modify`, `decompose` |

Goal creation uses `core_goal` with `{"action":"set", ...}`. Lifecycle
writes use the same tool with the matching action key.

<a id="freeze-guards"></a>
## Freeze Guards

`FlavorRegistry::try_freeze()` rejects with `FlavorRegistryError`:

1. Duplicate schemas, tool names, and `FlavorDescriptor::flavor_id`.
2. Capability tags for unregistered schemas.
3. An opaque Fact, Abstraction, Perspective, or Goal descriptor. Only
   `CitedObject` and `CitationMapping` schemas may be opaque;
   `try_add_opaque_schema` rejects the invalid kinds before freeze too.
4. Any schema/ingress mismatch: each typed schema has exactly one protocol
   ingress parser, each opaque citation schema has none, and every parser
   resolves to a typed schema.
5. A registered MCP tool with no resolvable behaviour declaration.
6. A tool whose `Args` is an internally tagged enum — so its schema carries
   `x-proxima-actions` and clients see a dispatcher — that declares no
   `ACTION_ARG_SPECS`. Nothing would then enumerate its actions: the scope
   gate falls back to whole-tool grants, the catalog lists none, REST serves
   no action route, and arguments are validated against every variant's
   fields merged together.
7. `ACTION_ARG_SPECS` that disagree with the derived schema: a discriminator
   other than `action`, a different action set, or different
   `allowed_fields`/`required_fields` for an action. Two specs naming the same
   action fail here too — a set comparison cannot see the collapse, and the
   later spec would never be read — as do specs on a tool whose `Args` is a
   plain struct, and a schema whose `x-proxima-actions` is present but not an
   object, which is a malformed extension rather than an absent one.
8. Contract/registration drift: two contracts claiming the same ordinal, a
   contract set with no flavor #0 in it, resources declared by a flavor other
   than #0, a contract schema id carrying another flavor's prefix, a
   `NotTransferable` schema naming no enforcement site, a contract entry for a
   schema nothing registered, a schema registered under a flavor whose
   contract does not declare it, and a contract naming an unregistered MCP
   tool.
9. A declaration that disagrees with the registration it duplicates. Four,
   all of the same shape — the contract says one thing, something else in the
   binary already said another, and until these checks nothing kept the two
   equal:
   - `EmbeddingRecipe::Never` on a schema whose payload type declares
     `EMBEDDABLE = true`, or a recipe with units on one that declares
     `false`. The disagreement is not symmetric in cost: the `Never`-plus-
     `true` direction files embedding jobs the drain can only drop.
   - `natural_key_columns` that are not the columns the registration carries.
   - A `ToolContract::actions` list that is not the registered
     `ACTION_ARG_SPECS` action list, compared IN ORDER — palette scope keys
     are `"<wire_name>:<action>"`, so the list is read by people, and one
     that agrees on membership while disagreeing on order has stopped being
     a copy of the thing it describes.
   - A `ToolContract::idempotent` that is not what the tool's resolved
     annotations say. Read-only resolves as idempotent: calling a read twice
     is calling it once, and MCP's `readOnlyHint` carries that.
10. A surface whose declared rule names no leg the generator can run:
    `UndeletableSurface`, `UnmovableSurface`, `UnforgettableSurface`.
    Each of the three partitions (`EraseLeg`, `TransferLeg`, `ForgetLeg`) has
    an `Unreachable` arm, and freeze refuses a registry that produces one —
    which is how a rule/`KeyShape` pair that no statement could be generated
    for becomes a boot failure instead of a silently skipped table.

Prefixes in macro-registered schemas and MCP tools are `const` assertions, so
a misprefixed id fails the build rather than the first boot.

<a id="contract-reach"></a>
## Contract Reach

**What the registry derives is mostly the LISTS.** Erase, export, the
owner-pinned sidecar set, the MCP resource catalog, the lexical-stamp
migration guardrail and unscoped search's core-sidecar selection all iterate
the contracts to learn *which tables exist*.

**Search is the exception: it reads VALUES.** `core_search_memories` renders
its statements FROM the declaration, so these fields are consumed, not
described:

| Declared | Consumed as |
|---|---|
| `SearchProjectionDecl::Projected { fields, tag_column, language }` | the projection row's `search_tsv`, `tags` and `regconfig` |
| `bands` (resolved by `BAND_NAME_EXACT` / `_RESCUE` / `_SUBSTRING`) | the floor and width each arm's `ts_rank` is scaled into |
| `Band::normalization` | the trailing `ts_rank` normalization flag — omitted entirely for `TS_RANK_NORMALIZATION_NONE`, so declaring the flag an arm already renders is score-free at the level of the emitted text |
| `substring` | whether a schema contributes to the substring statement at all; `SubstringArm::Off` contributes no statement and no rows |
| `ProjectionSpec::overfetch_k` | the ranked statement's `LIMIT` |
| `ProjectionSpec::band_comparability` | whether a tag-scoped query may merge this flavor's scores with core's |
| `ProjectionSpec::rank_source` | whether the core renderer can serve the flavor at all (`SidecarWithProjectionOwner` names a shape it cannot) |
| declared field `weight`s | `ts_rank`'s `{D,C,B,A}` array, bucketed into PostgreSQL's four classes; one uniform level passes no array at all |

Freeze checks what it can: a flavor claiming `CoreBands` must declare bands
inside `[0, 1]`, and one claiming `RankSource::Projection` must declare all
three band names plus a uniform language, band set and weight array across
its projected schemas — because one statement per flavor can spell each of
those only once. A wrong VALUE is therefore a boot failure, not a silently
different score, which is a stronger guarantee than the list-shaped lanes
above have.

**What freeze does not check is the claim against the behaviour.** A flavor
whose verbs read sidecar columns can declare `RankSource::Projection` and be
believed; what it loses is a statement shape it cannot use, not anything a
caller can see. Same for `BandComparability::CoreBands`: the bands are
checked for being inside core's window, not for meaning what core's mean.
Both are declarations on trust, in the same sense as the per-`Surface` rule
arms below — the difference is that these two have a consumer.

`SubstringArm` is the third of them, and the boundary runs *inside* the
type. The core renderer discriminates `Off` from not-`Off` and nothing
finer: every schema it serves gets the memory-first nested loop, so a
core-served schema declaring `SameTableLike` would be believed and rendered
as `MemoryFirstNestedLoop`. The distinction is real where it is read — the
code flavor's chunk and commit search each gate their own `LIKE` lane on
`matches!(…, SameTableLike)`, which is what keeps that lane a declared arm
rather than an undeclared third mechanism — and no schema in the tree is
both `SameTableLike` and core-served. A branch discriminating the two arms
inside the core renderer would therefore have no reachable input, so the
boundary is recorded here instead of built: the same treatment
`RankSource::Projection` gets, for the same reason.

**The per-`Surface` rule arms are the erase and the export.**
`EraseRule`, `ExportRule` and `KeyShape` are no longer vocabulary. The owner
erase generates a statement per `ByKey` / `ByOwner` surface and emits none
for a `Cascade` or a `Never`; the owner export generates a statement per
`Rows` / `Allowlist` surface and none for an `Excluded`, with the key's
columns as the row order. Freeze refuses a flavor that declares an
exportable surface it cannot reach, a test asserts every declared surface is
reached by a generated leg or named in one sorted exemption list, and
another asks the `pg_constraint` catalog whether each declared cascade
exists. The three `DECLARED GAP` exclusions (`wake_config`, `blob_uploads`,
`content` — erased but never exported) are still gaps; they are now gaps the
declaration causes rather than gaps it merely describes.

`ForgetRule` is consumed, and by two iteration sources rather than one. The
`Dumped` legs are still walked from the ROW STAMP (`memory.sidecar_tables`),
deliberately: the stamp is the historical record of what the dump actually
read, so a registry that gained a sidecar after a row was written cannot
delete from a table that row never touched, and one that lost a table can
still forget rows written before it went. The `Deleted` legs — the embedding
triple and the sketch, derived rows nothing stamps — come off the
declaration, and a `DeleteWithMemory` surface the forget cannot reach is
`FlavorRegistryError::UnforgettableSurface` at boot. `Surface::completeness`
is what separates the two: a `DeleteWithMemory` surface whose parent FK
already cascades generates no statement, because the constraint is the list.

**`Provenance` is the lineage walk.** `core_think`'s `ancestors` direction
expanded `memory.origins` for every node, which is right for the schemas
that write origins and silent for the ones that do not. `Provenance` says
which is which, and the walk now asks: `OriginEdges` expands the array,
`None` expands nothing, and `PayloadOnly { subject_columns }` loads the
node's payload and takes the references whose declared FIELD is one of the
named columns. That last arm is plan checkpoint 9 — an interpretation is
made ABOUT its subjects and not FROM them, so its `origins` are empty by
construction and it was a lineage dead end. No new statement was needed:
`SidecarPayload::references()` already carries the field each reference came
from, and `subject_columns` is what picks the grounding ones out of the rest.

The `descendants` direction is deliberately NOT symmetric. Its inverse
question — which nodes name me in a declared subject column — has no index
to answer it, so it would be a sequential scan per hop. Descendants find
what pinned you, not what named you.

Three declarations were wrong when the field was first read, all in the code
flavor and all in the same direction: `code-chunk-v1` and `execution-plan-v1`
write an origin on every ingest and said `None`, and `work-assignment-v1`
grounds through two payload columns and said `None` while the comment at its
write site said `PayloadOnly` in prose. A fourth was over-declared: core's
`interpretation-v1` named `subject_kinds` as a subject column, and it holds
no memory id.

Two declarations were wrong when the field was first read, and neither could
have been found by reading: `ingest_keys` said `DeleteWithMemory` while the
shipped forget kept the rows (as `core_forget`'s own wire description
promises), and `memory_head` said it while the shipped forget rewinds. Both
are `Keep { why }` now, and
`cooling_keeps_the_receipt_and_rewinds_the_head_while_erase_takes_both` fails
if either is restored.

Two places are worth knowing about individually, because each looks like it
reads the contract and does not:

1. `storage-pg/src/lib.rs` — the boot marker's `to_regclass` relation probes
   are a hand-written list. It names `proxima_core.agent_note_v1`, which is a
   flavor-#0 declared sidecar, and `proxima_code.code_chunk_v1`, which is
   another flavor's sidecar named in kernel code.
One more used to be on that list, and Phase 4 removed it:
`storage-pg/src/access/owner_columns.rs` did not read `TransferRule` at all —
*which columns move on a transfer* was code, and the declaration merely
described it. It is a `TransferLeg` partition now, resolved once by
`OwnerSurfaces::for_registry` and read by the verb: `Rehomed` and `Dropped`
generate their statements from the declared key and owner columns,
`FollowOrDedupe`'s `dedupe_key` and `remaps` generate the dedupe probe and
the repointing updates, and a `Follow` surface no leg reaches is
`FlavorRegistryError::UnmovableSurface` at boot rather than a row that stays
readable by the source owner after the memory moved.

Two more used to be on that list, and both were symptoms of the code flavor
shipping no `FlavorContract`: nothing existed for those lanes to iterate, and
`check_owner_pinned_against_contracts` skipped the flavor entirely. Now that
it declares one:

- Repo erase moved out of the kernel. `storage-pg/src/verbs/code_repo_erase.rs`
  hand-listed five of the flavor's sixteen sidecars in the `UNION` that
  collected the affected `t`s and named two detail tables the constraints
  already cascaded. It is `flavors/code/src/repos/erase.rs` now, next to the
  contract, and a unit test fails when a declared surface is reached by
  neither the sweep, a cascade, nor a named exemption. The substrate half is
  gone outright: the flavor hands its admissions to
  `verbs::forget::erase_memory_series`, which walks the sidecar registry.
- `flavors/code/src/mcp/search_commits.rs` and `search_chunks.rs` render
  their score windows from the flavor's own `bands` declaration, and that
  declaration is built from flavor #0's `BAND_EXACT` / `BAND_RESCUE` /
  `BAND_SUBSTRING` — which is what makes a code-flavor score comparable with
  core's. The window values live in exactly one place now: flavor #0's
  declaration. There is no `proxima_core::flavor::BAND_*` free constant to
  read them from, deliberately, so "comparable with core" cannot be claimed
  by copying a number.

**Not every kernel relation is a declared `Surface`.** Two carry
owner-scoped state and appear in no contract: `lexical_languages` and
`lexical_default`, both boot-probed by the stamp guardrail. There were six —
`owner_fact_retention`, `owner_legal_holds` and `compliance_audit_log` were
three of the others, and Phase C deleted all three tables;
`group_memberships` was the fourth, and Phase 4 declared it rather than
deleting it, with `EraseRule::Never { why }` saying in the contract what
`UNDECLARED_BUT_INTENTIONAL` used to say in a test. A membership names two
owners and belongs to neither exclusively, which is what its EMPTY
`owner_columns` claims.
Both are named in a `ResourceContract`'s `reads`, which is a different
claim — what a handler touches, not a surface with erase/export/forget rules.

**`flavor_surface` enforces one direction, not both.** The database trigger
on `proxima_core.memory` is `stamp ⊆ registry`: a `sidecar_tables` stamp
naming an undeclared table is refused. The converse — every declared sidecar
has a `flavor_surface` row — is a Rust test in
`storage-pg/tests/migrations.rs`, not a constraint, because an array FK is
not expressible in Postgres.

<a id="inclusion"></a>
## Inclusion

Flavor crate = inclusion unit. Composite binary = build artifact.

```
proxima-mcp                      = substrate + code
proxima-mcp --no-default-features = substrate
```

Composite binaries are framework apps, not plugin hosts.

<a id="no-feature-flags"></a>
## No Feature Flags

Flavor inclusion is by crate linkage and explicit `register()` call.
`proxima-mcp` defaults the `code` packaging feature on so the shipped host
is the Code-flavor MCP. `--no-default-features` is substrate-only. There
is no per-schema or runtime feature-flag matrix.

<a id="composite-discipline"></a>
## Composite Discipline

Composite binaries may combine flavors, but registry ownership remains
per flavor id:

1. Each schema and MCP tool keeps its flavor prefix.
2. Cross-flavor reads obey Owner role resolution and authorized read-owner sets.
3. Cross-flavor connections are ordinary references declared by whichever
   payload owns the statement; there is no vocabulary to agree on.
4. Composite binaries do not introduce ad-hoc runtime vocabulary.
