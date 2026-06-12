# 08. Core And Flavors

<a id="decision"></a>
## Decision

Core owns the substrate. Flavor crates contribute build-time vocabulary.
Composite binaries choose flavor crates at build time and freeze the
registry at startup.

## Boundary

```
core
  ids, Owner, Memory / Goal / Edge contracts
  F/A/P and Goal payload traits
  relation descriptor validation
  frozen registry
  storage traits
  wake dispatcher
  personality runtime rows + wake-entry contracts
  substrate personality tools
  substrate MCP config tools

flavor crate
  typed payload schemas
  GoalPayload schemas
  typed EdgePayload schemas
  relation descriptors
  MCP tools
  optional frontend package

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

Core registers substrate schemas, core relation descriptors, and
substrate MCP config tools in the default registry. Flavor crates append
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
  inference tier / target binding
  wake entries
  substrate tool palette
```

There is no flavor-owned runtime personality trait. A flavor may provide
payload schemas, MCP tools, dependency rules, and frontend rendering, but
personality instances remain substrate rows.

Wake entries, not flavors, choose trigger schema, goal scope, model tier,
tool palette, instructions, and execution mode.

## Substrate Tool Pack

Default personality tools:

| Read | Write |
|---|---|
| `core/fetch_memory` | `core/emit_abstraction` |
| `core/list_self_perspectives` | `core/emit_perspective` |
| `core/walk_lineage` |  |
| `core/search_memories` |  |
| `core/list_active_goals` |  |

Substrate MCP config tools are separate `core/*` MCP tools registered by
core, including personality CRUD, wake-entry CRUD, inference binding, and
schema/edge/tool listing.

Flavor MCP tools extend the MCP catalog. Wake-entry validation checks
palettes against the substrate personality tool pack and registered MCP
tools.

Core exposes no generic `create_edge` personality tool. Relation creation
is relation-specific because typed relations require descriptor masks and
payload validation.

## Goal Boundary

`Goal` is a core entity. Core owns Goal identity, lifecycle states,
GoalWrite semantics, active-goal query semantics, and `core/inspires`.

`proxima-goal` is the reference flavor for GoalPayload schemas,
proposal/accept/decline/modify MCP tools, sidecars, renderers, and
`proxima-goal/motivated-by`; it does not own the Goal entity contract
(see [06 §Goal Entity](06-goals-and-self.md#goal-entity)).

Goal creation uses flavor tools, not a substrate `emit_goal` tool.

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
proxima-mcp       = substrate + agent-memory + goal
proxima-code      = substrate + code
proxima-shell     = substrate + code + goal + agent-memory
```

Composite binaries are not plugin hosts.

<a id="no-feature-flags"></a>
## No Feature Flags

Flavor inclusion is by crate linkage and explicit `register()` call.
There is no feature-flag matrix for partial flavor inclusion.

<a id="composite-discipline"></a>
## Composite Discipline

Composite binaries may combine flavors, but registry ownership remains
per flavor id:

1. Each schema, relation, MCP tool, and dependency rule keeps its flavor
   prefix.
2. Cross-flavor reads obey owner/read-scope rules.
3. Cross-flavor edges must use registered relation descriptors.
4. Composite binaries do not introduce ad-hoc runtime vocabulary.
