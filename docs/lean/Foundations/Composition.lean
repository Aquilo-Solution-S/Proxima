/-
Proxima Foundations — Composition

The architecture-flexibility axioms (docs 03, 08), cemented in the
kernel by explicit decision (Heinrich, 2026-06-11). The substrate is
the CENTER; applications attach as flavors; the core is total
without any of them.

CF-G — payload opacity, the domainless trick: the kernel has NO
accessor from SchemaRef into payload content. Domains exist for the
kernel only as namespaced vocabulary. This absence is the
flexibility guarantee — any application (working-hero, neko, a
learning world, a legal world) integrates as a flavor with ZERO
kernel change. The kernel is the DNA; flavors are gene expression.

CF-F — RelationClass closedness is carried by the inductive in
Foundations.Edges: flavors add relation ids, never classes.

Excluded as Rust/build mechanics (→ COVERAGE.md): the
`proxima_flavor!` macro surface (CF-8..19, CF-25), Cargo-derived
metadata (CF-26..31), freeze-guard panic list (CF-47..53 — their
kernel content is `registry_determined` + namespace discipline),
sidecar SQL mechanics (SR-35..41), tool catalogs (CF-36..42).
-/

import Foundations.Prelude
import Foundations.Owner
import Foundations.Identity
import Foundations.Memory
import Foundations.Goals
import Foundations.Edges

namespace Proxima

-- ============================================================
-- Namespaces (doc 08 §Schema Namespacing)
-- ============================================================

/-- A vocabulary contributor: `core` or one flavor. -/
axiom FlavorId : Type

/-- The distinguished substrate contributor (`core/*`). -/
axiom core_namespace : FlavorId

/-- Schema identity, flavor-qualified (`flavor_id/local_name`,
    SR-8). The kernel resolves a SchemaRef to its SchemaId and no
    further — versioning stays opaque inside SchemaRef. -/
axiom SchemaId : Type
axiom schema_ref_id : SchemaRef → SchemaId

/-- CF-B carriers — every schema and relation id wears exactly one
    namespace. Identity carries namespace, so cross-flavor collision
    is impossible by construction (CF-C: two equal ids trivially
    share a namespace). -/
axiom schema_namespace   : SchemaId → FlavorId
axiom relation_namespace : RelationId → FlavorId

-- ============================================================
-- The registry (doc 08 §Registration Mechanism, doc 03 §Registry rules)
-- ============================================================

/-- The build-time vocabulary of one composite binary: which flavors
    are linked, which schemas and relations exist. Created once,
    frozen at startup (SR-1). -/
axiom Registry : Type
axiom registry_flavors   : Registry → Set FlavorId
axiom registry_schemas   : Registry → Set SchemaId
axiom registry_relations : Registry → Set RelationId

/-- CF-A — CORE INDEPENDENCE, the center-piece axiom. Core's
    vocabulary is a fixed set, namespaced `core`, present in EVERY
    registry regardless of which flavors are linked. The substrate
    is total without any flavor; no flavor is load-bearing for the
    core (doc 08: "Core owns the substrate. Flavor crates contribute
    build-time vocabulary"). -/
axiom core_vocabulary : Set SchemaId
axiom core_relations  : Set RelationId

axiom core_vocabulary_namespaced :
  ∀ s : SchemaId, s ∈ core_vocabulary → schema_namespace s = core_namespace

axiom core_relations_namespaced :
  ∀ r : RelationId, r ∈ core_relations → relation_namespace r = core_namespace

axiom core_always_present :
  ∀ (reg : Registry),
    (∀ s : SchemaId, s ∈ core_vocabulary → s ∈ registry_schemas reg) ∧
    (∀ r : RelationId, r ∈ core_relations → r ∈ registry_relations reg)

/-- CF-B — namespace discipline (CF-20..24): every registered
    schema/relation belongs to core or to a LINKED flavor. No
    orphan vocabulary; no flavor smuggles ids under another's
    prefix. -/
axiom registry_namespace_discipline :
  ∀ (reg : Registry),
    (∀ s : SchemaId, s ∈ registry_schemas reg →
      schema_namespace s = core_namespace ∨ schema_namespace s ∈ registry_flavors reg) ∧
    (∀ r : RelationId, r ∈ registry_relations reg →
      relation_namespace r = core_namespace ∨ relation_namespace r ∈ registry_flavors reg)

/-- CF-D — REGISTRY FROZEN (SR-1, CF-2/3): the vocabulary is a pure
    function of the linked flavor set. Same flavors ⇒ same
    vocabulary; nothing else (runtime, config, data) can vary it.
    "No runtime registration tier." -/
axiom registry_determined :
  ∀ reg1 reg2 : Registry,
    registry_flavors reg1 = registry_flavors reg2 →
    registry_schemas reg1 = registry_schemas reg2 ∧
    registry_relations reg1 = registry_relations reg2

-- ============================================================
-- Entities use registered vocabulary (doc 02 §Relation Registry:
-- "Unregistered relations are invalid"; SR-2: no untyped payload)
-- ============================================================

/-- The registry of the running binary. One ambient registry per
    deployment — composite binaries are build artifacts, not plugin
    hosts (CF-54/55). -/
axiom active_registry : Registry

/-- CF-E — every Memory, Goal, Event, and CitedObject is typed by a
    registered schema; every Edge's relation is registered. -/
axiom memories_use_registered_schemas :
  ∀ m : Memory, schema_ref_id (memory_schema m) ∈ registry_schemas active_registry

axiom goals_use_registered_schemas :
  ∀ g : Goal, schema_ref_id (goal_schema g) ∈ registry_schemas active_registry

axiom events_use_registered_schemas :
  ∀ e : Event, schema_ref_id (event_schema e) ∈ registry_schemas active_registry

axiom edges_use_registered_relations :
  ∀ e : Edge, edge_relation e ∈ registry_relations active_registry

-- ============================================================
-- Special-category flag (doc 03 §Special-category declaration)
-- ============================================================

/-- SR-30..33 — declared PER SCHEMA (never per row), by the
    controller/flavor author (never inferred by the substrate). The
    accessor's domain — SchemaId, not Memory — carries the per-schema
    rule structurally. -/
axiom schema_special_category : SchemaId → Bool

end Proxima
