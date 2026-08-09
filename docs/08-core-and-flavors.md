# 08. Core And Flavors

<a id="decision"></a>
## Decision

Core is the Rust runtime framework core. It owns graph contracts,
build-time flavor registry, protocol verbs, Goal/Self/WakeConfig runtime,
agent long-term memory substrate, substrate MCP tools, inference config
vocabulary, and storage ports. Flavor crates contribute build-time
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
  wires storage + model clients + transports
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
| `dependency_satisfaction_rules` | Build-time dependency rules for flavor schemas. |

Unknown keys are compile errors. Macro-registered schemas, tools, and
dependency rules must start with `name + "/"`, except dependency rules may
target `core/` schemas.

There is no `relations` or `edge_schemas` key. A flavor that needs its nodes
connected declares reference fields on the payload that owns the statement
(see [09](09-developing-flavors.md#declaring-references)); the index rows
follow.

## Public Tiers

| Tier | Import | Surface |
|---|---|---|
| Host API | `use proxima::{Proxima, RuntimeBuilder, Engine, AuthzContext};` | boot/migrate/serve/query |
| Flavor SDK | `use proxima::flavor::{FlavorBundle, FlavorRegistry, FactPayload, Tool};` | schemas, reference declarations, sidecars, tools |

Root `proxima::*` is host-facing. Flavor authoring imports live under
`proxima::flavor`.

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
  active Goal heads whose `assignment_perspective_id` names it
```

Wake is Goal-owned config:

```
Goal.wake = none | some WakeConfig
WakeConfig.toolset subset-of actor ToolScope intersect deployment profile
```

There is no flavor-owned runtime agent trait. A flavor may provide payload
schemas, the reference fields on them, MCP tools, and dependency rules. External
harnesses drive model and execution decisions; PR6 core exposes candidate
reads only, not an executor.

## Substrate MCP Surface

Default substrate memory surface:

| Read | Write |
|---|---|
| `proxima://memory/{id}` resource | `core_derive` |
| `core_search_memories` | `core_remember` |
| `proxima://memory/{id}/lineage` resource | `core_record_utterance` |
| `proxima://edges{?kind,source,target,limit,cursor}` resource | `core_interpret` |

Substrate MCP config tools are core-registered flat tools plus action
dispatchers for goals and Facts. Schema,
edge, tool, graph, memory-hydration, lineage, and event reads are MCP
resources.

Flavor MCP tools extend the MCP catalog, flat tools and action dispatchers
alike. Goal WakeConfig validation checks registered trigger/tool shape;
candidate reads apply actor and deployment tool-scope narrowing.

**Resources are substrate-only, by design.** `CORE_RESOURCES` is the whole
resource catalog, `FlavorRegistry` carries no resource vocabulary at all, and
`proxima://` dispatch is a closed `match` in `mcp-server/src/server.rs`. This
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
| Verb | `GoalWrite` create / supersede semantics |
| Query | active-goal heads and assignment traversal |
| Topology | `assignment_perspective_id`, `dependency_goal_ids`, `evidence_memory_ids` on the Goal row; the index entries are derived from them |
| Payloads | core `GoalPayload` schemas |
| Tools | `core_goal` action dispatcher: `set`, `transition`, `mark_achieved`, `modify`, `decompose` |

Goal creation uses `core_goal` with `{"action":"set", ...}`. Lifecycle
writes use the same tool with the matching action key.

<a id="freeze-guards"></a>
## Freeze Guards

`FlavorRegistry::try_freeze()` rejects with `FlavorRegistryError`:

1. Duplicate schemas, tool names, `FlavorDescriptor::flavor_id`, and
   dependency satisfaction rules.
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

Prefix violations in macro-registered schemas, MCP tools, and dependency
rules fail during registration before freeze — schema-id prefixes as `const`
assertions, so a misprefixed id fails the build rather than the first boot.

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

1. Each schema, MCP tool, and dependency rule keeps its flavor prefix.
2. Cross-flavor reads obey Owner role resolution and authorized read-owner sets.
3. Cross-flavor connections are ordinary references declared by whichever
   payload owns the statement; there is no vocabulary to agree on.
4. Composite binaries do not introduce ad-hoc runtime vocabulary.
