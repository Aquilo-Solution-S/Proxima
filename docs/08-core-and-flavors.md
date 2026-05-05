# 08 — Core and Flavors

The bare engine is one thing; the schemas, sources, and tools that
populate it are another. This doc names the cut.

## Decision

**Build-time registration only.** Schemas, tools, EventSource
*types*, relations, and prompts are Rust code in flavor crates
compiled into the binary. Adding a new flavor is a release, not an
API call. There is no runtime registration tier — no `Registrant`
enum, no parked v2+ HTTP API.

Runtime config is **instance-level only**: which Forgejo URL,
which Telegram bot, which LLM endpoint, what the default `Owner`
is, etc. Type-level extension is build-time; instance-level
config is runtime. The full runtime-config surface (LLM endpoint,
embedding model, credential resolution) is specified in
[10](docs/10-configuration.md).

## Workspace layout

```
proxima/
├── core/                       library (proxima-core)
└── flavors/
    ├── code/                   binary: proxima-code
    ├── learning/               binary: proxima-learning
    ├── jurisdiction/           binary: proxima-jurisdiction
    └── <X>/                    binary: proxima-<X>
```

Each flavor directory is a Cargo crate that depends on
`proxima-core`. Single-flavor deployments build a binary directly
from one flavor; multi-flavor deployments use a composite crate
(below).

Multi-flavor deployments in v1 use a **composite flavor crate**:
`flavors/aquilo-suite/` depends on `flavors/code` +
`flavors/learning` and produces one binary that registers all of
their schemas, sources, tools, and prompts at startup. The
workspace and Cargo are already optimised for this shape; building
flavored apps at build time is the v1 scope (Q3).

Side-by-side binaries sharing one Postgres + vector store is a
plausible future deployment mode but is **not in v1** — it would
add an Owner/schema collision matrix and a deploy-time topology
the project doesn't yet need. Re-introduce when a real deployment
demands it.

## Substrate stance

Proxima-core is the **substrate**: kernel + invariants +
philosophy (the spinning wheel, F/A/P, perspectivist
constructivism, type discipline, append-only). Flavors carry the
**flavor mass**: schemas, sources, prompts, tools. The cut is
intentional and load-bearing.

Aquilo ships proxima-core plus reference flavors (`code` first;
see project memory) that set the contract. Aquilo does **not**
claim ontology authority across verticals — a flavor may be
authored by Aquilo, a third party (signed crate, Cargo registry),
or the customer themselves. Cross-flavor insight is the value
proposition of running multiple flavors in one binary.

The marketplace shape parallels Substrate's pallet ecosystem:
independent crates, strict typed contract, build-time
composition, kernel unchanged. This is the operational
consequence of the causa proxima thesis — the contribution is
the engineering invariants, not the flavor ontology.

See [13](13-flavor-marketplace.md) for the marketplace evolution
path. v1 ships the primitives (composite crate, `proxima_flavor!`
macro, capability-bounded signed artefacts); the catalog, compose
tool, and third-party authorship workflow are formalized there as
v2+ concept.

## Composite discipline

A composite-flavor binary is the **union** of its constituent
flavors. Each flavor brings its own operators; cross-flavor
synthesis comes from whichever flavor in the mix authors A→P
operators with cross-flavor inputs (typically a cognition flavor
like `general-reasoning`), or from the composite crate itself when
deployment-specific synthesis is needed. Concretely:

- **Sidecar tables stay per-flavor.** No generic `payload jsonb`
  column. Composite = `code` and `learning` *both* present with
  their own typed sidecars (`fact_forgejo_commit_v3`,
  `fact_lecture_note_v1`, …). Strict principles enable fast
  iteration; the compiler does not lie. JSONB-blob composition
  loses migration cleanliness and is rejected. Sidecars are
  namespaced under `proxima_<FLAVOR_ID>.*` per
  [07 §Storage layout](docs/07-storage.md#storage-layout).
- **Cross-flavor read free; cross-flavor write into another
  flavor's sidecar forbidden.** An A→P, A→Goal, or edge operator
  registered by flavor B may select from `proxima_code.*` directly —
  pg-schema namespacing is for organisation and permissions, not
  query isolation. But only Code-registered operators may emit rows
  into `proxima_code.*` sidecars. Write authority follows schema
  authorship; otherwise migration ownership becomes ambiguous (who
  runs the v2 cutover? who backfills?). The macro-time check is on
  the authoring flavor of every operator's emitted output
  schemas — a flavor that lists an output payload type owned by
  another flavor is a compile-time error. Cross-flavor *synthesis*
  happens by an operator that takes cross-flavor *inputs* and emits
  its own owned output schema, not by writing into a foreign
  sidecar.
- **Relations are union-with-namespace.** `code/parent-commit` and
  `learning/cites-source` coexist; ids are flavor-prefixed (02).
  No id-overlap rule needed — the namespace already separates them.
- **A and P schemas are flat within a binary.** Every registered
  `AbstractionPayload` and `PerspectivePayload` is available to
  every operator. A's and P's are **not** flavor-owned — only Fact
  schemas, sources, and the per-FactSchema F→A prompt are.
  Perspective is Owner-scoped (universe.md §perspectivist
  constructivism); flavors are sources of inputs and templates,
  not partitions of cognition.
- **F→A is per (FactSchema, AbstractionSchema) pair.** At most one
  F→A operator per pair; multiple F→A operators may coexist over the same
  Fact schema, each producing a different typed Abstraction. Each
  flavor's F→A prompt lives alongside its operator (04). The prompt's
  declared output A schema set may include any registered
  `AbstractionPayload` — including those owned by other flavors,
  via explicit Cargo dep. A `code` Fact may produce both a
  `code/bug-fix-cluster` and a `core/work-pattern` Abstraction via
  separate operators.
- **A→P is plural; multiple operators run in parallel.** Each A→P
  operator declares its own input set and emits its own typed
  `PerspectivePayload`. Operators come from three places: the
  flavor that owns the input/output schemas (intra-flavor); a
  cognition flavor that depends on multiple schema-owning flavors
  (cross-flavor — e.g., `general-reasoning`); or the composite
  crate itself, when synthesis is specific to that deployment.
  Cross-flavor synthesis is **not** automatic — *some* A→P
  operator must take cross-flavor inputs. The standard pattern is
  to compose a cognition flavor alongside the schema-owning
  flavors; bespoke composite-level A→P is the escape hatch when
  off-the-shelf cognition flavors don't fit. The authoring cost
  exists either way, but it lives where the operator lives — not
  always in the composite.
- **Cross-flavor Perspectives are normal.** Any A→P operator that
  takes inputs from multiple flavors emits P's that span flavor
  concerns whenever the A-graph supports them. No operator-authored
  carve-out; no exception wiring.
- **Typed cross-flavor is the default; `AnyAbstraction` is opt-in.**
  Cross-flavor operators normally declare their inputs by name
  (`inputs: [code::BugFixClusterV1, learning::LectureNoteV1]`) by
  taking Cargo deps on the input flavor crates. Type discipline
  carries across the flavor boundary unchanged. `inputs:
  AnyAbstraction` is an explicit opt-out of payload typing,
  reserved for cognition that is deliberately schema-agnostic
  (self-model, goal-coverage, generic personality synthesis) —
  the class of operators whose job is "reason over whatever this
  binary happens to know." See [04 §A→P scope](docs/04-consolidation.md#ap--abstraction-to-perspective-intra-or-cross-flavor).

## No feature flags

`#[cfg(feature = "...")]` toggling of schemas / sources / tools is
explicitly **not** the model. The flavor crate itself is the unit
of inclusion. A binary that links a flavor links *all* of that
flavor; subsetting means a smaller flavor or a different flavor.

Reasons:

- `cargo tree` enumerates the surface; `--features` matrix
  multiplies it.
- Conditional compilation hides bugs that only manifest in unbuilt
  configurations.
- Build-time decisions stay in Cargo.toml, not scattered through
  source.

## Bare core

- `Owner` / `Principal` / `Group` / `Org` primitives (01).
- Memory entity, edges, directionality rules (02).
- Goal entity + `GoalPayload` trait + `GoalCoreV1` default payload (06).
- `PersonalityFlavor` trait + `PersonalitySnapshot` shape + `SelfView` shape
  + read-scope-matrix runtime (02 §Personality, 02 §Read-scope matrix, 06 §Self).
  No canonical personality is shipped by the substrate; flavors register
  the impls.
- F→A / A→P / A→Goal / Edge operators — the algorithm (04).
  Substrate ships the **dispatcher**: per-operator bounded work
  queues, dedicated worker pools, per-(Owner, `personality_id`)
  fairness scheduling within each queue, and a binary-wide LLM
  concurrency / cost cap above the per-operator workers. Flavors
  register operators and supply prompts / flavor-specific
  extractors. Disjoint output sidecars (§Composite discipline)
  make cross-operator backpressure impossible by construction.
  Phase-1 ingestion runs parallel-by-source outside the operator
  dispatcher. Runtime knobs in
  [10 §Operator concurrency](docs/10-configuration.md#operator-concurrency);
  full model in
  [04 §Execution model and isolation](docs/04-consolidation.md#execution-model-and-isolation).
- Storage primitives (07): events / memories / edges / goals /
  source_batch_f2a / read_scope_matrix / embeddings tables; sidecar discipline.
- `EventSource` trait, `ToolCallable` trait, `FactPayload` /
  `AbstractionPayload` / `PerspectivePayload` / `EdgePayload` /
  `CitedObjectPayload` / `CitationMappingPayload` /
  `PersonalityFlavor` traits, registration interface.
- `system` source — every flavor that emits actions reuses it ([05](docs/05-actions.md)).
- `MotivationV1` `AbstractionPayload` — uniform shape for
  motivation Abstractions citing Action-Facts; flavors extend with
  richer per-flavor variants (05 §MotivationV1).
- `LlmCallV1` and `EmbeddingCallV1` `FactPayload`s — uniform shape
  for the dispatcher's per-call telemetry, emitted under the
  `system` source. Every operator / decider / source LLM or
  embedding call produces one such Fact (success or failure)
  carrying caller identity, tier, vendor + model_id, token counts
  (including cache reads / writes for LLM calls), latency, and
  cost. Cost tracking, quota enforcement, and audit ride this
  single Fact stream — no parallel metrics sidecar, no shadow
  vendor-SDK path. See 05 §Dispatcher-emitted call Facts.
- `ModelTier` enum (`Fast` | `Standard` | `Deep`) and `LlmCaps`
  struct — LLM routing primitives. Operators declare a `tier` and
  `requires`; deployments map tiers to concrete `(vendor, model_id)`
  per [10 §Model tiers](docs/10-configuration.md#model-tiers). Tier
  expansion is a substrate PR, not a flavor PR.
- Citation, supersession, append-only, time partitioning.
- **Compliance vocabulary and operations** ([15](15-compliance.md)).
  `proxima_core::compliance` ships `LawfulBasis`, `RetentionPolicy`,
  `Region`, `RecipientId` (the type vocabulary used by 01, 03, 12
  registration surfaces) plus the substrate operations
  `delete_owner`, `pause_owner`, `resume_owner`, `export_owner`,
  the `compliance.*` audit schema, and the suppression-list
  mechanic. `cascade_delete` is deferred per 15 §Operations.

## What a flavor supplies

- `EventSource` impls for its Reality slice.
- `FactPayload` impls — typed Rust structs, one per
  `(schema_id, schema_version)` — plus sidecar SQL migration plus
  on-demand renderer.
- `AbstractionPayload` impls — **required** for every Abstraction
  the flavor's F→A operator emits. Sidecar SQL migration; no
  renderer (the operator-authored `text` is the text view). See
  03 §AbstractionPayload.
- `PerspectivePayload` impls — **required** for every Perspective
  the flavor's A→P operator emits. Same shape as
  AbstractionPayload.
- `GoalPayload` impls — **optional**. Bare core's `GoalCoreV1`
  covers undifferentiated goals. Register a per-flavor payload
  (e.g. `code/needs-implementation`) when the dispatch surface
  needs structured matching beyond text. See 06 §Typed payloads.
- `RelationDescriptor` registrations — one per relation id the
  flavor's EventSources author and per causal/interpretive relation
  its Perspectives may author via `link`. See 02 §Relation registry.
- `EdgePayload` impls — **optional**, one per relation whose
  descriptor declares a `payload_schema`. Substrate / core relations
  (`core/derived-from`, `core/supersedes`, `core/parent`,
  `core/motivated-by`) carry no payload and skip this. Required only
  when an edge needs flavor-specific structured state (e.g.
  `proxima-code/calls` with callsite byte ranges,
  `proxima-jurisdiction/cites` with precedent weight). Sidecar keyed
  on `edge_id`. See 03 §EdgePayload.
- `CitedObjectPayload` impls — one per kind of artefact the flavor's
  sources cite (PDF, image, video, chat session, …). Sidecar SQL
  migration; idempotency key for re-ingest dedup. See 11.
- `CitationMappingPayload` impls — one per typed annotation the
  flavor uses to point a Fact at a cited object (page+paragraph,
  bbox, message id, …). Sidecar SQL migration. See [11](docs/11-citations.md).
- `Tool` registrations — name, schema_id, callable, availability,
  plus the required `[tool.compliance]` block
  (`data_residency`, `recipients`, `legal_consequence`). The
  install-time validator rejects manifests omitting the block, and
  refuses non-AYU wiring for `legal_consequence = true` absent the
  deployment override. See
  [12 §Compliance metadata](12-tool-manifest.md#compliance-metadata).
- **Deciders** — **optional, plural.** Loops that pick which tool to
  call given Goal / Perspective / Action context. A flavor registers
  zero (fully manual or observation-only), one, or many — each scoped
  to a use case, each with its own per-call `availability` predicate.
  Programmed-rule, tool-calling LLM, and human-in-the-loop styles
  compose. LLM-driven variants declare `tier` + `requires` like other
  operators ([10 §Operator declaration](docs/10-configuration.md#operator-declaration));
  rule-based variants omit them. Substrate enforces no specific shape;
  the typical scaling path is manual → programmed → LLM-driven, grown
  per use case. Not part of bare core. See
  [05 §Deciders](docs/05-actions.md#deciders-flavor-supplied).
- **F→A and A→P prompts.** Bare core ships the operator algorithm
  ([04](docs/04-consolidation.md)); the flavor supplies the actual prompt strings. Per
  (Fact, Abstraction) pair for F→A; per A→P operator (operator-owned,
  plural). See [04 §Prompt locality](docs/04-consolidation.md#prompt-locality).
- `PersonalityFlavor` impls — **optional, plural.** A flavor may ship
  one or many personality flavors (e.g. `stoic-self`, `workhorse-programmer`,
  `tester-x`). Each declares a stable `PERSONALITY_ID`, the
  `snapshot(owner, ctx) -> PersonalitySnapshot` rule (which
  Perspectives / Goals enter `P_active` / `G_active`, top-K caps,
  fusion, identity weighting), and a `project_self(owner) -> SelfView`
  query. Multiple personalities may run concurrently per Owner;
  cross-personality reads are gated by the per-Owner read-scope
  matrix (07 §read_scope_matrix). See 02 §Personality.

## Registration mechanism

Each flavor crate declares its surface via the `proxima_flavor!`
macro. The macro expands to a `pub fn register(&mut Registry)` —
no `inventory!`, no autodiscovery — and is grep-able as a single
block per flavor.

### Schema namespacing

Every registered payload's `SCHEMA_ID` must begin with
`<crate-name>/` where `<crate-name>` is the value of
`CARGO_PKG_NAME` for the crate containing the `proxima_flavor!`
invocation. The macro asserts this at compile time and rejects
any registered type whose `SCHEMA_ID` violates it.

In practice authors don't write the prefix. A
`proxima_schema_id!("short-id")` helper macro reads
`CARGO_PKG_NAME` at compile time and emits the fully-qualified id;
flavors call it inside their payload impls (03 §FactPayload). A
deriveable shorthand —
`#[derive(ProximaFact)] #[proxima_fact(short = "lesson", version = 1)]`
— may also be provided to generate the impl entirely.

This works because **Cargo registries enforce crate-name
uniqueness within a registry.** Two third parties cannot both
publish a crate named `code` to crates.io; therefore two third
parties cannot both produce a `code/forgejo-commit` schema id.
Crate-name uniqueness gives schema-namespace uniqueness for free.

What this does *not* cover: a composite that mixes flavors from
two registries can still hit a name collision (e.g. a customer's
private `code` against a public crates.io `code`). Realistically
rare; the v2 marketplace work in 13 names signed-publisher
prefixing as the future fix. v1 ships the simpler model and the
reserved-namespace check.

### Compliance-metadata enforcement

The macro and registration paths enforce the compliance metadata
declared elsewhere — substrate startup fails when:

- Any `EventSource` instance configured in `proxima.config.toml`
  omits the `compliance` block (lawful_basis / collection_purpose /
  retention_policy / data_residency — 01 §Compliance metadata).
- Any installed `Tool` manifest omits the `[tool.compliance]`
  block (12 §Compliance metadata) or wires a
  `legal_consequence = true` tool to a non-AYU decider without
  the deployment override (05 §Deciders).

The values may be trivial (`lawful_basis = NotApplicable`,
`data_residency = Unrestricted`, `legal_consequence = false`) —
substrate enforces *presence*, not specific values. `SPECIAL_CATEGORY`
follows the same posture but with a `false` default in the trait
itself, since the bare-core and most flavor impls don't carry
special-category data — controllers handling Art. 9-class data
must explicitly override (03 §Special-category declaration). The
controller chooses what to declare under which regime; substrate
guarantees the declarations exist and are typed.

A flavor's surface is **partial** — it brings any subset of:
schemas (Fact / Abstraction / Perspective / Goal / cited / citation),
sources, T2 tools, relations, prompts, **operators** (F→A / A→P /
A→Goal / Edge), and **personality flavors** (`PersonalityFlavor` impls).
There is no flavor kind discriminator. A pure-flavor flavor brings
schemas + sources + intra-flavor operators. A pure-cognition flavor
brings cross-flavor operators only. A pure-personality flavor brings
only `PersonalityFlavor` impls — no schemas, sources, or operators of
its own — turning a substrate-with-domain-flavors into a
substrate-with-domain-flavors-and-this-voice. Most useful flavors mix.

```rust
// Flavor: schemas + sources + intra-flavor operators.
proxima_flavor! {
    name = "code",

    fact_schemas        = [ ForgejoCommitV3, ForgejoIssueV1, ForgejoCommentV1 ],
    abstraction_schemas = [ BugFixClusterV1, BugFixMotivationV1, CollaborationPatternV1 ],
    perspective_schemas = [ RepoDriftPatternV1 ],
    goal_schemas        = [ NeedsImplementationV1, NeedsReviewV1 ],
    cited_objects       = [ ForgejoRepoStateV1 ],
    citation_mappings   = [ ForgejoCommitInRepoV1 ],

    relations = [
        ("code/parent-commit", Structural),
        ("code/commit-fixes",  Causal),
    ],

    sources = [ ForgejoWebhookSource ],
    tools   = [ AskQuestionTool ],

    // F→A operators — at most one per (Fact schema, Abstraction schema) pair.
    // Multiple operators may target the same Fact schema with different
    // Abstraction outputs. `tier` and `requires` are the LLM-routing
    // fields per [10 §Operator declaration](docs/10-configuration.md#operator-declaration).
    f2a_operators = [
        (ForgejoCommitV3, BugFixClusterV1) => F2A {
            prompt:   prompts::COMMIT_F2A_BUGFIX,
            cadence:  BatchClose,
            tier:     Standard,                  // default; omit for brevity below
            requires: LlmCaps { json_mode: true, ..LlmCaps::none() },
        },
        (ForgejoCommitV3, CollaborationPatternV1) => F2A { prompt: prompts::COMMIT_F2A_COLLAB, cadence: BatchClose },  // tier defaults Standard, requires defaults none
        ForgejoIssueV1  => F2A { prompt: prompts::ISSUE_F2A,  cadence: BatchClose },
    ],

    // Intra-flavor A→P operator (optional).
    a2p_operators = [
        Intra("code/repo-drift") => A2P {
            inputs:  [ BugFixClusterV1 ],
            output:  RepoDriftPatternV1,
            prompt:  prompts::REPO_DRIFT_A2P,
            cadence: GoalRelevant("code/*"),
            tier:    Deep,                        // identity-relevant synthesis
        },
    ],
}
```

```rust
// Cross-flavor cognition flavor: operators only, no schemas/sources.
// Two cross-flavor input shapes coexist: typed (depends on input
// flavor crates, named schemas, compile-time checked) and
// polymorphic (`AnyAbstraction`, runtime union, used only when
// inputs cannot be enumerated).
proxima_flavor! {
    name = "general-reasoning",

    perspective_schemas = [ SelfModelV1, GoalCoverageV1, BugFixLearningTransferV1 ],

    a2p_operators = [
        // Typed cross-flavor: explicit input schemas across crates.
        // Cargo deps on `code` and `learning` make these names resolve.
        Cross("general/bugfix-learning-transfer") => A2P {
            inputs:  [ code::BugFixClusterV1, learning::LectureNoteV1 ],
            output:  BugFixLearningTransferV1,
            prompt:  prompts::BUGFIX_LEARNING_A2P,
            cadence: AbstractionThreshold(20),
        },

        // Polymorphic cross-flavor: union over the linked binary's full
        // A pool. Typed payloads are erased to `dyn AbstractionPayload`.
        // Reserved for cognition that genuinely cannot enumerate inputs
        // (self-model, goal coverage, generic personality synthesis).
        Cross("general/self-model") => A2P {
            inputs:   AnyAbstraction,
            output:   SelfModelV1,
            prompt:   prompts::SELF_MODEL_A2P,
            cadence:  Scheduled("nightly"),
            tier:     Deep,                        // self-model: identity-relevant
            requires: LlmCaps { long_context: true, json_mode: true, ..LlmCaps::none() },
        },
        Cross("general/goal-coverage") => A2P {
            inputs:  AnyAbstraction,
            output:  GoalCoverageV1,
            prompt:  prompts::GOAL_COVERAGE_A2P,
            cadence: OnGoalWrite,
            tier:    Standard,
        },
    ],

    edge_operators = [
        Cross("general/interpretive-link") => Edge {
            scope:    AbstractionAndPerspective,
            relation: "general/related-tension",   // registered Interpretive
            prompt:   prompts::INTERPRETIVE_LINK,
            cadence:  AbstractionThreshold(50),
            tier:     Fast,                        // high-frequency edge wiring
        },
    ],
}
```

```rust
// Pure-personality flavor: a voice over whatever schemas the binary links.
// Brings only `PersonalityFlavor` impls — no schemas, sources, or operators.
proxima_flavor! {
    name = "stoic-visionary",

    personalities = [ StoicVisionaryV1 ],
}

// Where the impl lives in the flavor crate:
impl PersonalityFlavor for StoicVisionaryV1 {
    const PERSONALITY_ID: PersonalityId = "stoic-visionary/v1";

    fn snapshot(&self, owner: Owner, ctx: SnapshotCtx) -> PersonalitySnapshot {
        // Flavor-defined selection rule — this one weights identity
        // Perspectives heavily, then goal-relevant, with a recency floor.
        // Substrate doesn't legislate the mix.
        ...
    }

    fn project_self(&self, owner: Owner) -> SelfView {
        // Flavor-defined Self projection over (P_active, G_active, ...).
        ...
    }
}
```

Composite flavors compose by listing constituents:

```rust
proxima_composite! {
    name    = "aquilo-suite",
    flavors = [ code, general_reasoning ],
}
```

`proxima_composite!` expands to a `register` that calls each
constituent's `register` in declared order. The composite must
satisfy:

- **F→A exclusivity.** At most one F→A operator per (Fact schema,
  Abstraction schema) pair across all linked flavors. Duplicate
  registration on the same pair is a compile-time macro error.
- **A→P plurality allowed.** Multiple A→P operators may register
  for overlapping input sets; each produces its own typed
  `PerspectivePayload`. They run in parallel by design.
- **No re-declaration.** A composite cannot redefine schemas,
  relations, tools, or operators a constituent owns; it may add
  its own cross-flavor surface (Perspective payloads, edge
  operators, cross-flavor A→P).
- **Personality plurality allowed.** Multiple `PersonalityFlavor`
  impls may register; `PERSONALITY_ID` must be unique across all
  linked flavors (compile-time macro error on collision). Per-Owner
  active-personality selection and the read-scope matrix are runtime
  config (07 §read_scope_matrix), not build-time facts.

Core `main` calls the active binary's `register` once at startup.
The active binary is a build-time choice (Cargo `--bin`); unlinked
flavors are absent. Conflicts are caught at compile time —
duplicate `SCHEMA_ID`s, duplicate relation ids, duplicate tool ids,
**duplicate F→A registration on one (Fact schema, Abstraction schema)
pair**, **duplicate `PERSONALITY_ID`s**, all surface as macro errors
before linking.

## Payload traits

The bare core ships seven payload traits (definitions in 03 / 06 / 11):

| Trait | Required | Renderer | Sidecar |
|---|---|---|---|
| `FactPayload`           | every Fact          | yes (deterministic, cheap) | `fact_<schema>_v<n>` |
| `AbstractionPayload`    | every Abstraction   | no — `Memory.text` is the view | `abstraction_<schema>_v<n>` |
| `PerspectivePayload`    | every Perspective   | no — `Memory.text` is the view | `perspective_<schema>_v<n>` |
| `GoalPayload`           | every Goal          | no — `Goal.text` is the view | `goal_<schema>_v<n>` |
| `EdgePayload`           | edges whose `RelationDescriptor` declares a `payload_schema` (substrate / core relations carry no payload) | no — substrate edge row carries the discriminators | `edge_<schema>_v<n>` (keyed on `edge_id`) |
| `CitedObjectPayload`    | per artefact kind   | no — sidecar carries S3 path / handle to the artefact | `cited_<schema>_v<n>` |
| `CitationMappingPayload`| per annotation kind | no — typed annotation only | `citation_<schema>_v<n>` |

`SCHEMA_ID` is the same string across versions; `SCHEMA_VERSION`
monotonic per id. Sidecar table per `(kind, SCHEMA_ID, SCHEMA_VERSION)`.

Bare core registers `GoalCoreV1` — the minimal `GoalPayload` for
goals that need no flavor-specific structure. Flavors register
richer per-flavor `GoalPayload` schemas; capability-based dispatch
(tool `availability` predicates matching on goal payload kind)
falls out of this ([06 §Typed payloads](docs/06-goals-and-self.md#typed-payloads-goalpayload)).

## Schema evolution

See [03 §Schema evolution: code + migration](03-schema-registry.md#schema-evolution-code--migration)
and §Streaming migration discipline therein. Migration is mandatory;
backfill runs as a chunked atomic stream; old sidecars are dropped
after cutover.

The schema set itself is the linker's output — there is no runtime
`schemas` registry table. `schema_migrations` (refinery /
sqlx-migrate style) tracks applied SQL migrations as in any
sqlx-style project.

## Diff against prior docs

- **03.** "Runtime-registered Fact schemas (Frappe DocType pattern)"
  flips to "compile-time `FactPayload` structs activated at startup
  from flavor code." `FieldType` / DDL-from-FieldDef machinery
  drops; sidecar tables are hand-written SQL migrations. Versioning,
  migration discipline, and renderers stay; the namespace is now
  binary-scoped (per [03 §Scoping](docs/03-schema-registry.md#scoping-one-namespace-per-binary)).
- **05.** Tool registration is build-time only. The `Registrant`
  enum is removed; there is no parked v2+ runtime variant.
- **07.** `schemas(schema_id, schema_version, json_schema, …)` row
  in the table sketch drops in favor of `schema_migrations`. The
  `tools.registrant` column drops with the `Registrant` enum.

## What this does not foreclose

- **Per-tenant schema scoping (enterprise).** Per [03 §Scoping](docs/03-schema-registry.md#scoping-one-namespace-per-binary); if
  needed, flavors emit per-tenant variants at compile time, not
  registry rows at runtime.

## Why this is the right cut

- **Type safety end-to-end.** Compiler enforces payload shape;
  no "unknown schema" runtime error class.
- **Performance.** No JSON-schema validation per ingest. Sidecar
  tables are regular SQL, indexed and tuned per flavor.
- **Auditability.** `cargo tree` enumerates the deployment's
  surface.
- **Migration realism.** Schema changes already need code review
  and staged deploys; coding them as code makes it explicit
  instead of pretending they're hot-pluggable.

Core stays small. Flavors carry flavor mass. New flavors land as
PRs against a flavor crate, not as registry mutations against a
running engine.

## Anchors

- `decision`
- `workspace-layout`
- `substrate-stance`
- `composite-discipline`
- `no-feature-flags`
- `bare-core`
- `what-a-flavor-supplies`
- `registration-mechanism`
- `payload-traits`
- `schema-evolution`
- `diff-against-prior-docs`
- `what-this-does-not-foreclose`
- `why-this-is-the-right-cut`
