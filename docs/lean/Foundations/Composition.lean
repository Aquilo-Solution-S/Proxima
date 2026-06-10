/-
Proxima Foundations — Composition

The architecture-flexibility axioms (docs 03, 08), cemented in the
kernel by explicit decision (Heinrich, 2026-06-11). The substrate is
the CENTER; applications attach as flavors; the core is total
without any of them.

Minimized trusted core (2026-06-11): the registry is DEFINED by its
composition law — vocabulary = core ∪ the linked flavors'
contributions (doc 08 §Registration Mechanism: created once,
populated by each flavor's register(), frozen). Core-presence,
flavor-determination (SR-1/CF-D "no runtime registration"), and
namespace discipline are THEOREMS of that law plus the per-
contributor namespacing axioms. An earlier `registry_determined`
axiom asserted cross-binary determination keyed on version-free
flavor-id sets — more than doc 08 states; the composition law
carries the doc-true content and the determination follows
pointwise.

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
kernel content is the composition law + namespace discipline),
sidecar SQL mechanics (SR-35..41), tool catalogs (CF-36..42).
-/

import Foundations.Prelude
import Foundations.Owner
import Foundations.Identity
import Foundations.Memory
import Foundations.Goals
import Foundations.Edges
import Foundations.Citations

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
-- Contributions — what core and each flavor bring
-- ============================================================

/-- CF-A — core's own vocabulary: a FIXED set, independent of any
    flavor (doc 08: "Core owns the substrate. Flavor crates
    contribute build-time vocabulary"). -/
axiom core_vocabulary : Set SchemaId
axiom core_relations  : Set RelationId

/-- Each flavor's build-time contribution (what its `register()`
    appends — doc 08 §Registration Mechanism), as a function of the
    flavor id alone. -/
axiom flavor_schemas   : FlavorId → Set SchemaId
axiom flavor_relations : FlavorId → Set RelationId

/-- Namespacing of contributions (CF-9/20..24, doc 08 §Macro Surface:
    "Macro-registered schemas, relations, … must start with
    name + '/'"; core schemas start with `core/`). One axiom, four
    arms — the merged CF-B/CF-A-namespacing trusted statement. -/
axiom contributions_namespaced :
  (∀ s : SchemaId, s ∈ core_vocabulary → schema_namespace s = core_namespace) ∧
  (∀ r : RelationId, r ∈ core_relations → relation_namespace r = core_namespace) ∧
  (∀ (f : FlavorId) (s : SchemaId), s ∈ flavor_schemas f → schema_namespace s = f) ∧
  (∀ (f : FlavorId) (r : RelationId), r ∈ flavor_relations f → relation_namespace r = f)

-- ============================================================
-- The registry and its composition law
-- ============================================================

/-- The build-time vocabulary of one composite binary: which flavors
    are linked, which schemas and relations exist. Created once,
    frozen at startup (SR-1). -/
axiom Registry : Type
axiom registry_flavors   : Registry → Set FlavorId
axiom registry_schemas   : Registry → Set SchemaId
axiom registry_relations : Registry → Set RelationId

/-- THE composition law (SR-1, CF-1..4): a registry's vocabulary is
    EXACTLY core plus the linked flavors' contributions — nothing
    else can add vocabulary (no runtime registration tier), and
    nothing linked is dropped. -/
axiom registry_composition :
  ∀ reg : Registry,
    (∀ s : SchemaId, s ∈ registry_schemas reg ↔
      (s ∈ core_vocabulary ∨ ∃ f : FlavorId, f ∈ registry_flavors reg ∧ s ∈ flavor_schemas f)) ∧
    (∀ r : RelationId, r ∈ registry_relations reg ↔
      (r ∈ core_relations ∨ ∃ f : FlavorId, f ∈ registry_flavors reg ∧ r ∈ flavor_relations f))

/-- CF-A — CORE INDEPENDENCE: core's vocabulary is present in EVERY
    registry regardless of which flavors are linked; the substrate is
    total without any flavor. THEOREM of the composition law. -/
theorem core_always_present :
    ∀ (reg : Registry),
      (∀ s : SchemaId, s ∈ core_vocabulary → s ∈ registry_schemas reg) ∧
      (∀ r : RelationId, r ∈ core_relations → r ∈ registry_relations reg) := by
  intro reg
  obtain ⟨hs, hr⟩ := registry_composition reg
  exact ⟨fun s h => (hs s).mpr (Or.inl h), fun r h => (hr r).mpr (Or.inl h)⟩

/-- CF-B — namespace discipline (CF-20..24): every registered
    schema/relation belongs to core or to a LINKED flavor. THEOREM:
    composition law + contribution namespacing. -/
theorem registry_namespace_discipline :
    ∀ (reg : Registry),
      (∀ s : SchemaId, s ∈ registry_schemas reg →
        schema_namespace s = core_namespace ∨ schema_namespace s ∈ registry_flavors reg) ∧
      (∀ r : RelationId, r ∈ registry_relations reg →
        relation_namespace r = core_namespace ∨ relation_namespace r ∈ registry_flavors reg) := by
  intro reg
  obtain ⟨hcs, hcr, hfs, hfr⟩ := contributions_namespaced
  obtain ⟨hs, hr⟩ := registry_composition reg
  constructor
  · intro s hmem
    rcases (hs s).mp hmem with h | ⟨f, hf, hsf⟩
    · exact Or.inl (hcs s h)
    · exact Or.inr (by rw [hfs f s hsf]; exact hf)
  · intro r hmem
    rcases (hr r).mp hmem with h | ⟨f, hf, hrf⟩
    · exact Or.inl (hcr r h)
    · exact Or.inr (by rw [hfr f r hrf]; exact hf)

/-- CF-D — REGISTRY FROZEN / no runtime registration (SR-1, CF-2/3):
    the vocabulary is a pure function of the linked flavor set.
    THEOREM of the composition law, stated pointwise. -/
theorem registry_determined :
    ∀ reg1 reg2 : Registry,
      (∀ f : FlavorId, f ∈ registry_flavors reg1 ↔ f ∈ registry_flavors reg2) →
      (∀ s : SchemaId, s ∈ registry_schemas reg1 ↔ s ∈ registry_schemas reg2) ∧
      (∀ r : RelationId, r ∈ registry_relations reg1 ↔ r ∈ registry_relations reg2) := by
  intro reg1 reg2 hf
  obtain ⟨hs1, hr1⟩ := registry_composition reg1
  obtain ⟨hs2, hr2⟩ := registry_composition reg2
  constructor
  · intro s
    rw [hs1 s, hs2 s]
    constructor
    · rintro (h | ⟨f, hmem, hc⟩)
      · exact Or.inl h
      · exact Or.inr ⟨f, (hf f).mp hmem, hc⟩
    · rintro (h | ⟨f, hmem, hc⟩)
      · exact Or.inl h
      · exact Or.inr ⟨f, (hf f).mpr hmem, hc⟩
  · intro r
    rw [hr1 r, hr2 r]
    constructor
    · rintro (h | ⟨f, hmem, hc⟩)
      · exact Or.inl h
      · exact Or.inr ⟨f, (hf f).mp hmem, hc⟩
    · rintro (h | ⟨f, hmem, hc⟩)
      · exact Or.inl h
      · exact Or.inr ⟨f, (hf f).mpr hmem, hc⟩

-- ============================================================
-- Entities use registered vocabulary (doc 02 §Relation Registry:
-- "Unregistered relations are invalid"; SR-2: no untyped payload)
-- ============================================================

/-- The registry of the running binary. One ambient registry per
    deployment — composite binaries are build artifacts, not plugin
    hosts (CF-54/55). -/
axiom active_registry : Registry

/-- CF-E — every schema-typed entity (Memory, Goal, Event,
    CitedObject, CitationMapping) is typed by a registered schema,
    and every Edge's relation is registered. One axiom, six arms —
    the merged registration discipline (minimization pass). -/
axiom entities_use_registered_vocabulary :
  (∀ m : Memory, schema_ref_id (memory_schema m) ∈ registry_schemas active_registry) ∧
  (∀ g : Goal, schema_ref_id (goal_schema g) ∈ registry_schemas active_registry) ∧
  (∀ e : Event, schema_ref_id (event_schema e) ∈ registry_schemas active_registry) ∧
  (∀ c : CitedObject, schema_ref_id (cited_object_schema c) ∈ registry_schemas active_registry) ∧
  (∀ c : CitationMapping, schema_ref_id (citation_mapping_schema c) ∈ registry_schemas active_registry) ∧
  (∀ e : Edge, edge_relation e ∈ registry_relations active_registry)

/-- ME-19 in its original shape — projection theorem. -/
theorem edges_use_registered_relations :
    ∀ e : Edge, edge_relation e ∈ registry_relations active_registry :=
  entities_use_registered_vocabulary.2.2.2.2.2

-- ============================================================
-- Special-category flag (doc 03 §Special-category declaration)
-- ============================================================

/-- SR-30..33 — declared PER SCHEMA (never per row), by the
    controller/flavor author (never inferred by the substrate). The
    accessor's domain — SchemaId, not Memory — carries the per-schema
    rule structurally. -/
axiom schema_special_category : SchemaId → Bool

end Proxima
