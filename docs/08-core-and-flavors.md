# 08. Core And Flavors

<a id="decision"></a>
## Decision

Core is the Rust runtime framework core. It owns graph contracts,
build-time flavor registry, protocol verbs, wake/personality runtime,
agent long-term memory substrate, substrate MCP tools, inference config
vocabulary, and storage traits. Flavor crates contribute build-time
vocabulary. Composite binaries choose flavor crates at build time and
freeze the registry at startup.

Proxima is a framework: core plus linked flavor crates composed into an
app. Generic agent memory and Goal substrate are core; domain
vocabularies such as code remain flavors. There is no external
flavor/tool catalog and no runtime registration.

The formal kernel is `docs/lean/Foundations/`, not the Rust crate
boundary.

## Boundary

```
runtime framework core (`proxima-core`)
  ids, Owner, Memory / Goal / Edge contracts
  F/A/P and Goal payload traits
  core GoalPayload schemas + lifecycle Fact schemas
  relation descriptor validation
  frozen registry
  storage traits
  agent long-term memory sidecars + tools
  GoalWrite, active-goal queries, core Goal tools
  personality runtime rows + wake-entry contracts
  substrate MCP config tools

flavor crate
  typed payload schemas
  typed EdgePayload schemas
  relation descriptors
  MCP tools

composite binary
  links selected flavor crates
  calls register(&mut FlavorRegistry)
  freezes registry
  wires storage + model clients + transports
```

No runtime registration tier. Schemas, tools, sources, prompts, and
relations are build-time vocabulary.

<a id="registration-mechanism"></a>
## Registration Mechanism

Every composite binary creates `FlavorRegistry::new()`, calls each linked
flavor's generated `register(&mut FlavorRegistry)`, then calls
`freeze()`. Freeze-time failure is startup failure.

Core registers substrate schemas, core GoalPayload schemas, Goal
lifecycle Fact schemas, core relation descriptors, and substrate MCP
memory/config/goal tools in the default registry. Flavor crates append
their own descriptors through `proxima_flavor!`.

<a id="macro-surface"></a>
## Macro Surface

`proxima_flavor!` emits one `register(&mut FlavorRegistry)` function.
Supported keys:

| Key | Contract |
|---|---|
| `name` | Flavor id and required prefix for contributed ids. |
| `display_name` | Optional UI label; defaults to `name`. |
| `fact_schemas` | `FactPayload` sidecar schemas. |
| `abstraction_schemas` | `AbstractionPayload` sidecar schemas. |
| `perspective_schemas` | `PerspectivePayload` sidecar schemas. |
| `goal_schemas` | `GoalPayload` sidecar schemas. |
| `edge_schemas` | `EdgePayload` sidecar schemas for typed relations. |
| `cited_object_schemas` | `CitedObjectPayload` sidecar schemas (see 11). |
| `citation_mapping_schemas` | `CitationMappingPayload` sidecar schemas (see 11). |
| `opaque_cited_object_schemas` | Untyped cited-object schema ids. |
| `opaque_citation_mapping_schemas` | Untyped citation-mapping schema ids. |
| `schema_capability_tags` | Build-time capability tags on registered payload schemas. |
| `relations` | `RelationDescriptor` values. |
| `mcp_tools` | Flavor MCP tools; tool names must use the flavor prefix. |
| `dependency_satisfaction_rules` | Build-time dependency rules for flavor schemas. |

Unknown keys are compile errors. Macro-registered schemas, relations,
MCP tools, and dependency rules must start with `name + "/"`, except
dependency rules may target `proxima-core/` schemas.

<a id="schema-namespacing"></a>
## Schema Namespacing

Flavor-owned ids use `flavor_id/local_name`.

| Id kind | Prefix rule |
|---|---|
| Schema ids | Flavor schemas start with `flavor_id + "/"`; core schemas start with `core/`. |
| Relation ids | Flavor relations start with `flavor_id + "/"`; core relations start with `core/`. |
| MCP tool names | Flavor MCP tools start with `flavor_id + "/"`; substrate MCP tools start with `core/`. |

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
personality classes.

## Runtime Personality Boundary

Personality is runtime substrate state:

```
PersonalityInstance
  root Perspective
  wake entries:
    trigger kind + id
    goal scope
    probability
    label
    instructions
```

There is no flavor-owned runtime personality trait. A flavor may provide
payload schemas, MCP tools, and dependency rules, but
personality instances remain substrate rows.

Wake entries are detect config. External harnesses drive model, tool,
and execution decisions.

## Substrate Tool Pack

Default substrate memory tools:

| Read | Write |
|---|---|
| `core/get_memory` | `core/derive` |
| `core/search_memories` | `core/remember` |
| `core/walk_memory_lineage` | `core/record_utterance` |
|  | `core/link` |

Substrate MCP config tools are separate `core/*` MCP tools registered by
core, including personality CRUD, wake-entry CRUD, schema/edge/tool
listing, read-scope, fact-retention, citations, events, and goals.

Flavor MCP tools extend the MCP catalog. Wake-entry validation checks
trigger uniqueness and detect-config shape only.

Core exposes no generic `create_edge` personality tool. Relation creation
is relation-specific because typed relations require descriptor masks and
payload validation.

## Goal Boundary

Core owns:

| Surface | Core contract |
|---|---|
| Entity | Goal identity and 4-state lifecycle (`Active`, `Paused`, `Achieved`, `Abandoned`) |
| Verb | `GoalWrite` create / supersede semantics |
| Query | active-goal heads and assignment traversal |
| Relations | `core/inspires`, `core/motivated-by` |
| Payloads | core `GoalPayload` schemas |
| Tools | `core/goal_set`, `core/goal_transition`, `core/goal_mark_achieved`, `core/goal_modify`, `core/goal_decompose` |

Goal creation uses the core `core/goal_set` tool. Lifecycle writes use
the rest of the `core/goal_*` family.

<a id="freeze-guards"></a>
## Freeze Guards

`FlavorRegistry::freeze()` rejects:

1. Invalid relation descriptor masks.
2. Typed relations whose payload schema is not a registered Edge schema.
3. Duplicate `FlavorDescriptor::flavor_id`.
4. Duplicate MCP tool names.
5. Duplicate dependency satisfaction rules for one schema id.

Prefix violations in macro-registered schemas, relations, MCP tools, and
dependency rules panic during registration before freeze.

<a id="inclusion"></a>
## Inclusion

Flavor crate = inclusion unit. Composite binary = build artifact.

```
proxima-mcp                  = substrate
proxima-mcp --features code  = substrate + code
```

Composite binaries are framework apps, not plugin hosts.

<a id="no-feature-flags"></a>
## No Feature Flags

Flavor inclusion is by crate linkage and explicit `register()` call.
`proxima-mcp` exposes one packaging feature, `code`, to include the
`proxima-code` flavor in that host binary. There is no per-schema or
runtime feature-flag matrix.

<a id="composite-discipline"></a>
## Composite Discipline

Composite binaries may combine flavors, but registry ownership remains
per flavor id:

1. Each schema, relation, MCP tool, and dependency rule keeps its flavor
   prefix.
2. Cross-flavor reads obey owner/read-scope rules.
3. Cross-flavor edges must use registered relation descriptors.
4. Composite binaries do not introduce ad-hoc runtime vocabulary.
