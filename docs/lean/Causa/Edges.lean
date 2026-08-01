/-
Causa — Edges

The connection index (doc 16-edges.md, which supersedes doc 02 §Edges,
§Relation Registry and §The Directionality Rule). The thesis, verbatim:

  "Edges are fundamental, non-extensible connection patterns. An edge
   carries no information beyond its existence: its endpoints, its
   direction, its creation time, and its kind. All content lives in
   nodes; meaning arises from the synthesis of the connected nodes."

Seven obligations, and E7 is the master invariant the rest serve:

  E1 Existence    — both endpoints exist (`EdgeEndpointsExist`, resolved
                    against the admitted node tables).
  E2 Ownership    — `edge.owner = source.owner` (`EdgeSourceOwned`). It
                    constrains the SOURCE owner only; a cross-owner target is
                    what makes cross-owner provenance expressible, and it is
                    admitted when readable — the uniform admission rule lives
                    in `Causa.EdgeAuthorization`.
  E3 Layering     — ℓ(source) ≥ ℓ(target) for memory endpoints, Goal
                    endpoints outside the comparison (`EdgeLayeringValid`).
  E4 Kind follows — `origin` rows come only from a node write's derivation
     operation      declaration, `reference` rows only from schema-declared
                    reference fields
                    (`derived_edge_kind_follows_operation`). Raw `Edge` values
                    remain constructible, as every row shape in this kernel is
                    (D14/D16); what E4 says is that no row is ADMITTED except
                    through some node's declaration, and `deriveEdges` is the
                    only producer an admitted table has. A declaration with
                    ZERO origins is legal and contributes no `origin` rows — an
                    interpretation Perspective grounds through its references
                    and consumes nothing.
  E5 Structural   — the primary key IS the row. There is no `EdgeId` type to
     idempotency     mint from (Causa.Identity); two rows satisfying E2 with
                    the same endpoints and kind are the SAME VALUE
                    (`edge_key_determines_row`), and in an admitted table the
                    same holds of the stored ADDRESSES, which is `edges_pkey`
                    itself (`edge_table_key_unique`, via E1 and row-id
                    uniqueness). Under edge ids both were false, which is why
                    replay needed a content hash and a partial unique index.
  E6 No content   — no payload, sidecar, citation or status accessor exists on
                    `Edge`. Structural absence, sharpened by E5: there is no
                    field two rows with one key could differ in.
  E7 Rebuildability — the edge set is a FUNCTION of node content
                    (`deriveEdges`). Dropping the table and re-deriving it
                    yields the same set (`EdgeTableRebuildable`), and a store
                    whose declarations are layer-legal rebuilds into a VALID
                    table (`rebuilt_table_valid`) — E2 and E3 fall out of the
                    derivation rather than being checked after the fact.

What is deliberately absent, and why the absence is the invariant:

  * No `RelationClass`, `RelationDescriptor`, `RelationRegistry`,
    `RelationId`, endpoint/authorship mask, owner policy, or target-access
    policy. The kind is a consequence of the write, so there is no vocabulary
    for a writer to pick from and no per-relation policy cell to consult.
  * No `EdgeAuthorship`. Who reasoned is answered by the node that owns the
    statement (`memories.authoring_perspective_id`).
  * No Supersession kind. Supersession is a lineage pointer on the row
    (Causa.Memory, Causa.Goals) — the same thing persisting through revision
    is not a connection between two things.
  * No Causal or Interpretive kind. A causal or interpretive claim is a NODE:
    an interpretation Perspective whose payload references its subjects
    (`interpretationOf`). That is where U-2's commitment now lives — "cosine
    similarity is observer-independent and so cannot encode an
    observer-relative relation" is enforced by there being nowhere to put such
    a relation, plus E3 keeping a Fact source from reaching anything above a
    Fact (`fact_source_reaches_only_facts`).
  * No creation time. It is the one column of the runtime row that is NOT a
    function of node content; modeling it would make E7 false as stated, and
    no kernel obligation reads it (the kernel's time axis is
    `Memory.created_at`).

ME-15 — causal chains are queries, not entities: chain(f, P_active) =
reference backbone among Facts + interpretation Perspectives under P_active,
through their own references + origin closure to Facts. No Chain primitive
exists here, by design (doc 02 §Causal Chain Query).

CI-12/13 — edges do not cite: there is no citation accessor on `Edge`.
Anything citation-worthy is on the node that owns the statement.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity
import Causa.Memory
import Causa.Goals

namespace Causa

-- ============================================================
-- The closed kind vocabulary (doc 16 §Kinds are closed)
-- ============================================================

/-- The CLOSED substrate vocabulary — two kinds, and the enum is not
    extensible, not by flavors and not by core features. A feature that seems
    to need a third kind fails the node-home test and is missing a NODE, not a
    kind. Closedness is carried by the inductive itself (CF-F). -/
inductive EdgeKind where
  /-- A node declared what it was made from. Written only by the node write
      carrying that derivation declaration, in its own transaction. -/
  | origin
  /-- A schema-declared reference field of the source's payload points here.
      Derived at ingest from payload content. -/
  | reference
  deriving DecidableEq, Repr

-- ============================================================
-- Endpoints (doc 16 §The edge table is an index)
-- ============================================================

/-- The five endpoint labels the index stores (`proxima_core.edge_endpoint_kind`).
    It is a superset of the memory kinds because the ADDRESS FORM is part of
    what an endpoint is: F/A/P address a memory row, `Goal` a goal row, and
    `FactEntityHead` a stateful-Fact aggregate that follows its head. That is
    where the retired descriptor's `FollowHead`/`Pin` cell went — into the
    address itself, so the two can never disagree (ST-FE). -/
inductive EndpointKind where
  | Fact
  | Abstraction
  | Perspective
  | Goal
  | FactEntityHead
  deriving DecidableEq, Repr

/-- ℓ over endpoint labels (`proxima_core.edge_endpoint_layer`): F=0, A=1,
    P=2, a FactEntity head is Fact-like, and `Goal` has NO layer — Goal
    endpoints sit outside the F/A/P comparison as their own entity axis. -/
def EndpointKind.layer : EndpointKind → Option Nat
  | .Fact           => some 0
  | .FactEntityHead => some 0
  | .Abstraction    => some 1
  | .Perspective    => some 2
  | .Goal           => none

/-- The endpoint label a memory kind addresses. -/
def MemoryKind.endpointKind : MemoryKind → EndpointKind
  | .Fact        => .Fact
  | .Abstraction => .Abstraction
  | .Perspective => .Perspective

/-- The endpoint labels agree with the F/A/P layer order. -/
theorem memoryKind_endpoint_layer (k : MemoryKind) :
    k.endpointKind.layer = some k.layer := by
  cases k <;> rfl

/-- A memory row never addresses the Goal axis. -/
theorem memoryKind_endpointKind_ne_goal (k : MemoryKind) : k.endpointKind ≠ .Goal := by
  cases k <;> intro h <;> exact (nomatch h)

/-- A memory row never addresses a stateful-Fact head: pinning a row and
    following a head are different address forms. -/
theorem memoryKind_endpointKind_ne_head (k : MemoryKind) :
    k.endpointKind ≠ .FactEntityHead := by
  cases k <;> intro h <;> exact (nomatch h)

/-- An edge endpoint: a memory row, a Goal row, or a stateful-Fact head.
    `FactEntity` is not a fifth semantic node kind — it resolves to its current
    Fact and is Fact-like for the layer rule. -/
inductive NodeRef where
  | memory     (m : Memory)
  | goal       (g : Goal)
  | factEntity (e : FactEntity)

def NodeRef.owner : NodeRef → Owner
  | .memory m     => memory_owner m
  | .goal g       => goal_owner g
  | .factEntity e => fact_entity_owner e

/-- The address half of an endpoint: which row it names. -/
def NodeRef.id : NodeRef → Id
  | .memory m     => memory_id m
  | .goal g       => goal_id g
  | .factEntity e => fact_entity_id e

/-- The label half of an endpoint. -/
def NodeRef.endpointKind : NodeRef → EndpointKind
  | .memory m     => (memory_kind m).endpointKind
  | .goal _       => .Goal
  | .factEntity _ => .FactEntityHead

/-- The two columns that address one endpoint, together. Row identity is by
    address, not by object: this is what the primary key and the self-loop
    refusal compare. -/
def NodeRef.addr (r : NodeRef) : EndpointKind × Id := (r.endpointKind, r.id)

/-- Memory-kind view for endpoints that participate in F/A/P rules. FactEntity
    endpoints are Fact-like; Goal endpoints sit outside the comparison. -/
def NodeRef.memoryKind? : NodeRef → Option MemoryKind
  | .memory m     => some (memory_kind m)
  | .goal _       => none
  | .factEntity _ => some .Fact

/-- The memory-kind view and the endpoint label agree on the layer. -/
theorem nodeRef_layer_of_memoryKind (r : NodeRef) (k : MemoryKind) :
    r.memoryKind? = some k → r.endpointKind.layer = some k.layer := by
  cases r with
  | memory m =>
    intro h
    have hk : memory_kind m = k := Option.some.inj h
    rw [NodeRef.endpointKind, hk]
    exact memoryKind_endpoint_layer k
  | goal _ => intro h; exact (nomatch h)
  | factEntity _ =>
    intro h
    have hk : MemoryKind.Fact = k := Option.some.inj h
    rw [← hk]
    rfl

/-- A stateful-Fact head endpoint is always Fact-like: the address form is the
    binding, so there is no descriptor cell that could claim otherwise. -/
theorem factEntityEndpointIsFact (e : FactEntity) :
    (NodeRef.factEntity e).memoryKind? = some .Fact := rfl

-- ============================================================
-- The row (doc 16 §The edge table is an index)
-- ============================================================

/-- One index row, and this is the WHOLE model. No id, no relation, no
    namespace, no authorship column, no payload, no citation, no status (E6) —
    a connection that needs to say more than "these two, this way" is a node.
    `owner` is present because the runtime row denormalizes it; E2 is what
    pins it to the source. -/
structure Edge where
  source : NodeRef
  target : NodeRef
  kind   : EdgeKind
  owner  : Owner

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_source : Edge → NodeRef := Edge.source

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_target : Edge → NodeRef := Edge.target

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_kind : Edge → EdgeKind := Edge.kind

/-- Compatibility accessor for prose/Rust vocabulary. -/
def edge_owner : Edge → Owner := Edge.owner

/-- SR-25 / ST-5 — index rows are immutable and insert-only; a re-derivation
    re-asserts the same row rather than replacing one. -/
instance : Immutable Edge := ⟨⟩
instance : AppendOnly Edge := ⟨⟩

-- ============================================================
-- E2, E3 — row validity
-- ============================================================

/-- E2 (ME-9) — index rows are SOURCE-owned: the row's Owner is its source
    endpoint's Owner. The TARGET is deliberately unconstrained; that is what
    makes cross-owner provenance expressible. Query surfaces may still redact
    an unreadable target (Causa.Compliance). -/
def EdgeSourceOwned (e : Edge) : Prop :=
  (edge_source e).owner = edge_owner e

/-- The layer rule between two endpoints: whenever both carry a layer,
    ℓ(source) ≥ ℓ(target). A Goal endpoint carries none, so it is outside the
    comparison on either side. -/
def endpointsLayered (source target : NodeRef) : Prop :=
  ∀ ls lt : Nat,
    source.endpointKind.layer = some ls →
    target.endpointKind.layer = some lt →
    lt ≤ ls

/-- E3 (ME-10) — the F/A/P directionality rule, stated on the endpoints
    themselves. The nine-cell class matrix it replaces said exactly this and
    nothing more once the class vocabulary closed at two kinds that neither
    widen nor narrow it (doc 02 §The Directionality Rule). -/
def EdgeLayeringValid (e : Edge) : Prop :=
  endpointsLayered (edge_source e) (edge_target e)

/-- A row is admitted only between two distinct addresses. A self-loop asserts
    that a node relates to itself, which no node write can mean
    (`edges_no_self_loop_chk`). -/
def EdgeNoSelfLoop (e : Edge) : Prop :=
  (edge_source e).addr ≠ (edge_target e).addr

/-- One persisted index row's validity: everything the row itself can be
    checked against. E1 needs the node tables and lives below; E4–E7 are
    properties of how rows come to exist, not of a row in isolation. -/
structure EdgeValid (e : Edge) : Prop where
  sourceOwned : EdgeSourceOwned e
  layering : EdgeLayeringValid e
  noSelfLoop : EdgeNoSelfLoop e

/-- Table-scoped index validity. -/
def EdgeTableValid (edges : Set Edge) : Prop :=
  ∀ e : Edge, e ∈ edges → EdgeValid e

/-- E2 in its projection shape. -/
theorem edge_source_owned : ∀ e : Edge, EdgeValid e → (edge_source e).owner = edge_owner e := by
  intro e h
  exact h.sourceOwned

/-- ME-10 — ℓ(source) ≥ ℓ(target) for valid memory→memory rows. -/
theorem edge_layer_rule :
    ∀ e : Edge, EdgeValid e → ∀ ms mt : Memory,
      edge_source e = .memory ms → edge_target e = .memory mt →
      (memory_kind mt).layer ≤ (memory_kind ms).layer := by
  intro e h ms mt hs ht
  refine h.layering _ _ ?_ ?_
  · rw [hs]; exact memoryKind_endpoint_layer (memory_kind ms)
  · rw [ht]; exact memoryKind_endpoint_layer (memory_kind mt)

/-- U-2 / P4 — a Fact source reaches only Fact targets. A Fact asserts no
    judgment, so it can never be the source of an interpretation: the
    interpretation is a Perspective, and E3 forbids a Fact row from pointing
    at anything above a Fact. THEOREM from the layer rule alone. -/
theorem fact_source_reaches_only_facts :
    ∀ e : Edge, EdgeLayeringValid e →
      (edge_source e).memoryKind? = some .Fact →
      ∀ kt : MemoryKind, (edge_target e).memoryKind? = some kt → kt = .Fact := by
  intro e hlayer hs kt ht
  have hsl := nodeRef_layer_of_memoryKind (edge_source e) .Fact hs
  have htl := nodeRef_layer_of_memoryKind (edge_target e) kt ht
  have hle := hlayer (MemoryKind.layer .Fact) (MemoryKind.layer kt) hsl htl
  cases kt with
  | Fact => rfl
  | Abstraction => exact absurd hle (by decide)
  | Perspective => exact absurd hle (by decide)

-- ============================================================
-- E5 — the primary key IS the row
-- ============================================================

/-- The primary key: `(source_kind, source_id, target_kind, target_id, kind)`,
    in the kernel's endpoint spelling. -/
def edgeKey (e : Edge) : (EndpointKind × Id) × (EndpointKind × Id) × EdgeKind :=
  ((edge_source e).addr, (edge_target e).addr, edge_kind e)

/-- E5 — two valid rows with the same primary key are the SAME ROW. The owner
    column cannot distinguish them (E2 pins it to the source), and there is no
    other column to differ in (E6) — so idempotency is structural rather than
    approximated by a content-derived id. Under `EdgeId` this was FALSE: a
    replayed write minted a fresh id and produced a genuinely different value,
    which is exactly why v0.0.7 needed a BLAKE3 identity hash and a partial
    unique index. -/
theorem edge_key_determines_row :
    ∀ e₁ e₂ : Edge, EdgeSourceOwned e₁ → EdgeSourceOwned e₂ →
      edge_source e₁ = edge_source e₂ →
      edge_target e₁ = edge_target e₂ →
      edge_kind e₁ = edge_kind e₂ →
      e₁ = e₂ := by
  intro e₁ e₂ ho₁ ho₂ hs ht hk
  obtain ⟨s₁, t₁, k₁, o₁⟩ := e₁
  obtain ⟨s₂, t₂, k₂, o₂⟩ := e₂
  cases hs
  cases ht
  cases hk
  have howner : o₁ = o₂ := by
    have h₁ : s₁.owner = o₁ := ho₁
    have h₂ : s₁.owner = o₂ := ho₂
    rw [← h₁, ← h₂]
  cases howner
  rfl

/-- Asserting one index row into a table. `ON CONFLICT … DO NOTHING` in the
    write path; set union here. -/
def assertEdge (edges : Set Edge) (e : Edge) : Set Edge :=
  fun x => x ∈ edges ∨ x = e

/-- E5 — re-asserting a row that is already present changes nothing. -/
theorem assert_present_row_changes_nothing :
    ∀ (edges : Set Edge) (e : Edge), e ∈ edges →
      ∀ x : Edge, x ∈ assertEdge edges e ↔ x ∈ edges := by
  intro edges e hmem x
  constructor
  · rintro (hx | rfl)
    · exact hx
    · exact hmem
  · intro hx
    exact Or.inl hx

-- ============================================================
-- E1 — both endpoints exist
-- ============================================================

/-- Endpoint membership in the admitted node tables. A FactEntity endpoint must
    name an admitted aggregate whose current Fact head is an admitted memory
    row. -/
def NodeRefInTables
    (memories : Set Memory) (goals : Set Goal) (factEntities : Set FactEntity) : NodeRef → Prop
  | .memory m => m ∈ memories
  | .goal g   => g ∈ goals
  | .factEntity e => e ∈ factEntities ∧ e.current.memory ∈ memories

/-- E1 — every index row resolves both of its endpoints in the node tables.
    The runtime spells this as the existence trigger; no row survives a write
    whose endpoints are not there. -/
def EdgeEndpointsExist
    (memories : Set Memory) (goals : Set Goal) (factEntities : Set FactEntity)
    (edges : Set Edge) : Prop :=
  ∀ e : Edge, e ∈ edges →
    NodeRefInTables memories goals factEntities (edge_source e) ∧
    NodeRefInTables memories goals factEntities (edge_target e)

/-- E1 ⇒ an endpoint ADDRESS names at most one admitted row. The index stores
    `(kind, id)` pairs, not row objects, so this is what makes the primary key
    below say anything about rows at all — and it is exactly the id-uniqueness
    the node tables already carry. -/
theorem nodeRef_addr_determines_row
    (memories : Set Memory) (goals : Set Goal) (factEntities : Set FactEntity)
    (huniqM : MemoryIdUnique memories) (huniqG : GoalIdUnique goals)
    (huniqF : FactEntityIdUnique factEntities) :
    ∀ r₁ r₂ : NodeRef,
      NodeRefInTables memories goals factEntities r₁ →
      NodeRefInTables memories goals factEntities r₂ →
      r₁.addr = r₂.addr → r₁ = r₂ := by
  intro r₁ r₂ h₁ h₂ haddr
  have hkind : r₁.endpointKind = r₂.endpointKind := congrArg Prod.fst haddr
  have hid : r₁.id = r₂.id := congrArg Prod.snd haddr
  cases r₁ with
  | memory m₁ =>
    cases r₂ with
    | memory m₂ => exact congrArg NodeRef.memory (huniqM m₁ m₂ h₁ h₂ hid)
    | goal _ => exact absurd hkind (memoryKind_endpointKind_ne_goal (memory_kind m₁))
    | factEntity _ =>
      exact absurd hkind (memoryKind_endpointKind_ne_head (memory_kind m₁))
  | goal g₁ =>
    cases r₂ with
    | memory m₂ => exact absurd hkind.symm (memoryKind_endpointKind_ne_goal (memory_kind m₂))
    | goal g₂ => exact congrArg NodeRef.goal (huniqG g₁ g₂ h₁ h₂ hid)
    | factEntity _ => exact (nomatch hkind)
  | factEntity e₁ =>
    cases r₂ with
    | memory m₂ => exact absurd hkind.symm (memoryKind_endpointKind_ne_head (memory_kind m₂))
    | goal _ => exact (nomatch hkind)
    | factEntity e₂ => exact congrArg NodeRef.factEntity (huniqF e₁ e₂ h₁.1 h₂.1 hid)

/-- E5, table shape — THE PRIMARY KEY. An admitted index table holds at most
    one row per `(source_kind, source_id, target_kind, target_id, kind)`,
    exactly the columns `edges_pkey` names. THEOREM: the addresses resolve to
    unique rows (E1 + id uniqueness), the endpoints are therefore equal, and
    the owner column cannot differ (E2). Nothing is assumed about uniqueness —
    it is what having no id buys. -/
theorem edge_table_key_unique
    (memories : Set Memory) (goals : Set Goal) (factEntities : Set FactEntity)
    (edges : Set Edge)
    (huniqM : MemoryIdUnique memories) (huniqG : GoalIdUnique goals)
    (huniqF : FactEntityIdUnique factEntities)
    (hendpoints : EdgeEndpointsExist memories goals factEntities edges)
    (hvalid : EdgeTableValid edges) :
    ∀ e₁ e₂ : Edge, e₁ ∈ edges → e₂ ∈ edges → edgeKey e₁ = edgeKey e₂ → e₁ = e₂ := by
  intro e₁ e₂ h₁ h₂ hkey
  have hsaddr : (edge_source e₁).addr = (edge_source e₂).addr := congrArg Prod.fst hkey
  have hrest := congrArg Prod.snd hkey
  have htaddr : (edge_target e₁).addr = (edge_target e₂).addr := congrArg Prod.fst hrest
  have hkind : edge_kind e₁ = edge_kind e₂ := congrArg Prod.snd hrest
  have hs := nodeRef_addr_determines_row memories goals factEntities huniqM huniqG huniqF
    (edge_source e₁) (edge_source e₂) (hendpoints e₁ h₁).1 (hendpoints e₂ h₂).1 hsaddr
  have ht := nodeRef_addr_determines_row memories goals factEntities huniqM huniqG huniqF
    (edge_target e₁) (edge_target e₂) (hendpoints e₁ h₁).2 (hendpoints e₂ h₂).2 htaddr
  exact edge_key_determines_row e₁ e₂ (hvalid e₁ h₁).sourceOwned (hvalid e₂ h₂).sourceOwned
    hs ht hkind

-- ============================================================
-- E4 + E7 — node content is the only producer, and it is a function
-- ============================================================

/-- What ONE node's content declares about other nodes.

    `origins` is the node write's derivation declaration (`derived_from`) —
    what the node says it was made from. `references` are the schema-declared
    reference fields of its payload. Both are read OFF the node; neither is a
    parameter beside it, which is E4: there is no third list, and no way to
    ask for a row without a node saying something.

    The kernel sees resolved endpoints because it cannot see payloads (CF-G):
    which payload fields are reference fields is schema-registry knowledge
    below the opacity boundary. What it can state is that the row set is a
    function of this declaration, and that the declaration belongs to the
    node. -/
structure NodeDeclaration where
  node       : NodeRef
  origins    : List NodeRef
  references : List NodeRef

/-- E4 — the index rows one node's content implies, and the ONLY way rows come
    to exist in this file. The kind is read off which list the target was
    declared in; the owner is the declaring node's, which is E2 by
    construction. -/
def NodeDeclaration.edges (d : NodeDeclaration) : Set Edge :=
  fun e =>
    edge_source e = d.node ∧
    edge_owner e = d.node.owner ∧
    ((edge_kind e = .origin ∧ edge_target e ∈ d.origins) ∨
     (edge_kind e = .reference ∧ edge_target e ∈ d.references))

/-- What a node write may declare: only targets at or below its own layer, and
    never itself. Checking legality HERE, on the declaration, is the point —
    the statement is what is admitted or refused, and the index follows. -/
structure NodeDeclarationValid (d : NodeDeclaration) : Prop where
  originsLegal : ∀ t : NodeRef, t ∈ d.origins →
    endpointsLayered d.node t ∧ d.node.addr ≠ t.addr
  referencesLegal : ∀ t : NodeRef, t ∈ d.references →
    endpointsLayered d.node t ∧ d.node.addr ≠ t.addr

/-- E7 — THE derivation: node content in, index rows out. A function, which is
    the whole of rebuildability; everything else in this file is a lemma about
    it. -/
def deriveEdges (content : Set NodeDeclaration) : Set Edge :=
  fun e => ∃ d : NodeDeclaration, d ∈ content ∧ e ∈ d.edges

/-- E7 — an index table is rebuildable when it is exactly what the store's node
    content derives. "Drop the table and re-derive it from node content and you
    get the same set back." -/
def EdgeTableRebuildable (content : Set NodeDeclaration) (edges : Set Edge) : Prop :=
  ∀ e : Edge, e ∈ edges ↔ e ∈ deriveEdges content

/-- E7 — the derived table is rebuildable from the content it was derived
    from. The rebuild is idempotent because it is a function. -/
theorem derived_table_rebuildable :
    ∀ content : Set NodeDeclaration, EdgeTableRebuildable content (deriveEdges content) := by
  intro _ _
  exact Iff.rfl

/-- E7 — rebuilding is deterministic: two tables rebuilt from the same node
    content hold the same rows. This is what makes the index droppable. -/
theorem rebuild_deterministic :
    ∀ (content : Set NodeDeclaration) (edges₁ edges₂ : Set Edge),
      EdgeTableRebuildable content edges₁ →
      EdgeTableRebuildable content edges₂ →
      ∀ e : Edge, e ∈ edges₁ ↔ e ∈ edges₂ := by
  intro content edges₁ edges₂ h₁ h₂ e
  exact (h₁ e).trans (h₂ e).symm

/-- E4 — every derived row is backed by a declaration on its own source node,
    and its kind says WHICH declaration. No row is a free-standing act. -/
theorem derived_edge_kind_follows_operation :
    ∀ (content : Set NodeDeclaration) (e : Edge), e ∈ deriveEdges content →
      ∃ d : NodeDeclaration, d ∈ content ∧ edge_source e = d.node ∧
        ((edge_kind e = .origin ∧ edge_target e ∈ d.origins) ∨
         (edge_kind e = .reference ∧ edge_target e ∈ d.references)) := by
  intro content e h
  obtain ⟨d, hd, hsource, _, hkind⟩ := h
  exact ⟨d, hd, hsource, hkind⟩

/-- E4 — an `origin` row exists only where a node write carried a derivation
    declaration naming that target. -/
theorem origin_row_needs_a_derivation_declaration :
    ∀ (content : Set NodeDeclaration) (e : Edge),
      e ∈ deriveEdges content → edge_kind e = .origin →
      ∃ d : NodeDeclaration, d ∈ content ∧ edge_source e = d.node ∧
        edge_target e ∈ d.origins := by
  intro content e h hkind
  obtain ⟨d, hd, hsource, harm⟩ := derived_edge_kind_follows_operation content e h
  rcases harm with ⟨_, htarget⟩ | ⟨href, _⟩
  · exact ⟨d, hd, hsource, htarget⟩
  · rw [hkind] at href
    exact (nomatch href)

/-- E4 — a `reference` row exists only where a schema-declared reference field
    of the source's payload named that target. -/
theorem reference_row_needs_a_declared_reference_field :
    ∀ (content : Set NodeDeclaration) (e : Edge),
      e ∈ deriveEdges content → edge_kind e = .reference →
      ∃ d : NodeDeclaration, d ∈ content ∧ edge_source e = d.node ∧
        edge_target e ∈ d.references := by
  intro content e h hkind
  obtain ⟨d, hd, hsource, harm⟩ := derived_edge_kind_follows_operation content e h
  rcases harm with ⟨horigin, _⟩ | ⟨_, htarget⟩
  · rw [hkind] at horigin
    exact (nomatch horigin)
  · exact ⟨d, hd, hsource, htarget⟩

/-- E4, the case the rule must ACCOMMODATE — a write that declares no
    derivation is legal and simply contributes no `origin` rows. An
    interpretation Perspective is exactly this: it grounds through its
    references and consumes nothing, so there is no manifest to skip and
    nothing for one to prove (Causa.Operators). -/
theorem declaration_without_origins_writes_no_origin_rows :
    ∀ d : NodeDeclaration, d.origins = [] →
      ∀ e : Edge, e ∈ d.edges → edge_kind e = .reference := by
  intro d hnone e he
  obtain ⟨_, _, harm⟩ := he
  rcases harm with ⟨_, htarget⟩ | ⟨hkind, _⟩
  · rw [hnone] at htarget
    exact (nomatch htarget)
  · exact hkind

/-- E4 inhabitation — the zero-origin write is not a corner case argued in
    prose but a constructible declaration: a Perspective that declares only
    references. Its derived rows are all `reference` rows, and it is valid
    whenever its subjects are legal endpoints. -/
def interpretationDeclaration (p : Memory) (subjects : List NodeRef) : NodeDeclaration where
  node := .memory p
  origins := []
  references := subjects

theorem interpretation_declaration_writes_only_references :
    ∀ (p : Memory) (subjects : List NodeRef) (e : Edge),
      e ∈ (interpretationDeclaration p subjects).edges → edge_kind e = .reference :=
  fun p subjects => declaration_without_origins_writes_no_origin_rows
    (interpretationDeclaration p subjects) rfl

/-- E7 ⇒ E2 + E3 — a legal declaration derives VALID rows. Ownership is not
    checked after the fact: the row is owned by the node that made the
    statement because that is how the derivation builds it. -/
theorem declared_edges_valid :
    ∀ d : NodeDeclaration, NodeDeclarationValid d →
      ∀ e : Edge, e ∈ d.edges → EdgeValid e := by
  intro d hd e he
  obtain ⟨hsource, howner, harm⟩ := he
  have hlegal : endpointsLayered d.node (edge_target e) ∧ d.node.addr ≠ (edge_target e).addr := by
    rcases harm with ⟨_, htarget⟩ | ⟨_, htarget⟩
    · exact hd.originsLegal _ htarget
    · exact hd.referencesLegal _ htarget
  refine ⟨?_, ?_, ?_⟩
  · show (edge_source e).owner = edge_owner e
    rw [hsource, howner]
  · show endpointsLayered (edge_source e) (edge_target e)
    rw [hsource]
    exact hlegal.1
  · show (edge_source e).addr ≠ (edge_target e).addr
    rw [hsource]
    exact hlegal.2

/-- E7 (headline) — a store whose node content is legal rebuilds into a VALID
    index table. E2 and E3 are consequences of the derivation, not separate
    admission gates run over the result. -/
theorem rebuilt_table_valid :
    ∀ (content : Set NodeDeclaration) (edges : Set Edge),
      (∀ d : NodeDeclaration, d ∈ content → NodeDeclarationValid d) →
      EdgeTableRebuildable content edges →
      EdgeTableValid edges := by
  intro content edges hcontent hbuilt e he
  obtain ⟨d, hd, hedge⟩ := (hbuilt e).mp he
  exact declared_edges_valid d (hcontent d hd) e hedge

/-- E5 ∘ E7 — replaying a write asserts nothing new. The write re-derives the
    same rows (E7), those rows are already present, and a present row cannot
    be duplicated by a differing id because there is none (E5). -/
theorem replay_asserts_nothing_new :
    ∀ (content : Set NodeDeclaration) (edges : Set Edge),
      EdgeTableRebuildable content edges →
      ∀ e : Edge, e ∈ deriveEdges content →
        ∀ x : Edge, x ∈ assertEdge edges e ↔ x ∈ edges := by
  intro content edges hbuilt e hderived
  exact assert_present_row_changes_nothing edges e ((hbuilt e).mpr hderived)

-- ============================================================
-- The Goal side of node content (doc 16 §Flavor Migration)
-- ============================================================

/-- A Goal row's declaration is EXACTLY its three topology columns, in the
    order the write asserts them, resolved to rows. Ids are what a row holds;
    E1 is what resolves them. -/
def GoalDeclarationValid (g : Goal) (d : NodeDeclaration) : Prop :=
  d.node = .goal g ∧
  d.origins = [] ∧
  d.references.map NodeRef.id = goalDeclaredTargetIds g

/-- N4 residue — every row a Goal declares is a `reference` row. A Goal never
    declares a derivation, and the kind vocabulary has no causal variant left,
    so a Goal→Fact row carries no observer-independent causal claim: the
    perspectival claim that used to ride on a `Causal` edge is now a node. -/
theorem goal_declared_rows_are_references :
    ∀ (g : Goal) (d : NodeDeclaration), GoalDeclarationValid g d →
      ∀ e : Edge, e ∈ d.edges → edge_kind e = .reference := by
  intro g d hd e he
  exact declaration_without_origins_writes_no_origin_rows d hd.2.1 e he

/-- The index rows a Goal write asserts, counted from its declaration rather
    than from the table — one per declared id, which is what the write path
    reports back (`goal_topology_edge_count`). -/
theorem goal_declared_row_count :
    ∀ (g : Goal) (d : NodeDeclaration), GoalDeclarationValid g d →
      d.references.length = (goalDeclaredTargetIds g).length := by
  intro g d hd
  have h := hd.2.2
  calc d.references.length = (d.references.map NodeRef.id).length := (List.length_map _ _).symm
    _ = (goalDeclaredTargetIds g).length := by rw [h]

-- ============================================================
-- Interpretation is a node (doc 16 §The Model)
-- ============================================================

/-- An interpretation — a causal or non-causal claim about existing nodes — is
    a PERSPECTIVE whose payload references its subjects
    (`proxima_core.interpretation_v1`), never an edge. The index only records
    that the Perspective points at its subjects; the claim, its reason and its
    confidence are the node's payload. This definition is where U-2's
    commitment lives now that the kind vocabulary has nowhere to put a Causal
    or Interpretive row. -/
def interpretationOf (edges : Set Edge) (p : Memory) (subject : NodeRef) : Prop :=
  memory_kind p = .Perspective ∧
  ∃ e : Edge, e ∈ edges ∧ edge_kind e = .reference ∧
    edge_source e = .memory p ∧ edge_target e = subject

/-- P3c / U-2 — an interpretation is never a Fact. A Fact asserts no judgment,
    so it cannot occupy the interpreting position; and by E3 a Fact-sourced row
    could not reach an Abstraction or a Perspective to interpret it even if the
    vocabulary allowed it (`fact_source_reaches_only_facts`). -/
theorem interpretation_is_never_a_fact :
    ∀ (edges : Set Edge) (p : Memory) (subject : NodeRef),
      interpretationOf edges p subject → memory_kind p ≠ .Fact := by
  intro edges p subject h hfact
  rw [h.1] at hfact
  exact (nomatch hfact)

/-- An interpretation's index rows are `reference` rows — the claim is in the
    node, so the index has nothing to carry. -/
theorem interpretation_rows_are_references :
    ∀ (edges : Set Edge) (p : Memory) (subject : NodeRef),
      interpretationOf edges p subject →
        ∃ e : Edge, e ∈ edges ∧ edge_kind e = .reference ∧
          edge_source e = .memory p ∧ edge_target e = subject := by
  intro _ _ _ h
  exact h.2

-- ============================================================
-- The E1–E7 guarantee, machine-checked: no Causa axioms
-- ============================================================

-- E1 is table validity and E4's accommodation of the zero-origin write is a
-- theorem, not a carve-out; the rest are proved from the row shape and the
-- derivation. Every E1–E7 theorem below is axiom-free OUTRIGHT — not merely
-- free of Causa axioms, but of `propext` and `Quot.sound` too.
--
-- `#guard_msgs` is what makes that a CHECK rather than a claim. A bare
-- `#print axioms` only prints: a proof that started depending on an axiom
-- would emit a different line and the build would still succeed, so the
-- guarantee would decay in silence — the exact failure mode COVERAGE.md's
-- preamble describes. With the expected message pinned in the docstring, a
-- changed axiom surface is a BUILD ERROR, and so is a theorem that stops
-- existing. Verified by negative control: corrupting one expectation fails
-- the build with "Docstring on `#guard_msgs` does not match".
--
-- `scripts/check-lean-axioms.py` is the complementary half — it pins the set
-- of DECLARED `Causa` axioms at zero, across the whole kernel. This block
-- pins what these particular theorems CONSUME.

/-- info: 'Causa.edge_source_owned' does not depend on any axioms -/
#guard_msgs in
#print axioms edge_source_owned

/-- info: 'Causa.edge_layer_rule' does not depend on any axioms -/
#guard_msgs in
#print axioms edge_layer_rule

/-- info: 'Causa.fact_source_reaches_only_facts' does not depend on any axioms -/
#guard_msgs in
#print axioms fact_source_reaches_only_facts

/-- info: 'Causa.edge_key_determines_row' does not depend on any axioms -/
#guard_msgs in
#print axioms edge_key_determines_row

/-- info: 'Causa.edge_table_key_unique' does not depend on any axioms -/
#guard_msgs in
#print axioms edge_table_key_unique

/-- info: 'Causa.derived_edge_kind_follows_operation' does not depend on any axioms -/
#guard_msgs in
#print axioms derived_edge_kind_follows_operation

/-- info: 'Causa.origin_row_needs_a_derivation_declaration' does not depend on any axioms -/
#guard_msgs in
#print axioms origin_row_needs_a_derivation_declaration

/-- info: 'Causa.reference_row_needs_a_declared_reference_field' does not depend on any axioms -/
#guard_msgs in
#print axioms reference_row_needs_a_declared_reference_field

/-- info: 'Causa.declaration_without_origins_writes_no_origin_rows' does not depend on any axioms -/
#guard_msgs in
#print axioms declaration_without_origins_writes_no_origin_rows

/-- info: 'Causa.declared_edges_valid' does not depend on any axioms -/
#guard_msgs in
#print axioms declared_edges_valid

/-- info: 'Causa.rebuilt_table_valid' does not depend on any axioms -/
#guard_msgs in
#print axioms rebuilt_table_valid

/-- info: 'Causa.rebuild_deterministic' does not depend on any axioms -/
#guard_msgs in
#print axioms rebuild_deterministic

/-- info: 'Causa.replay_asserts_nothing_new' does not depend on any axioms -/
#guard_msgs in
#print axioms replay_asserts_nothing_new

/-- info: 'Causa.goal_declared_rows_are_references' does not depend on any axioms -/
#guard_msgs in
#print axioms goal_declared_rows_are_references

end Causa
