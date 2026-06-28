/-
Causa — Edge authorization

RelationDescriptor owns row-shape policy in `Causa.Edges`; requester-specific
write/read projection composes that descriptor policy with the group access gates
in `Causa.Authorization`.

The kernel law is narrow:

  * every edge write requires write authority on the source endpoint;
  * the relation descriptor selects no/read/write authority on the target;
  * edge-row read is source-local for source-owned edges;
  * target rendering is a separate target read gate;
  * target consent/co-ownership is not global row ownership — it is relation
    policy.
-/

import Causa.Edges
import Causa.Authorization

namespace Causa

/-- Memory kind as access-ladder kind. -/
def MemoryKind.accessKind : MemoryKind → AccessKind
  | .Fact => .fact
  | .Abstraction => .abstraction
  | .Perspective => .perspective

/-- Node endpoint as access-ladder kind. -/
def NodeRef.accessKind : NodeRef → AccessKind
  | .memory m => (memory_kind m).accessKind
  | .goal _ => .goal
  | .factEntity _ => .fact

/-- Requester-side target gate selected by a relation descriptor. -/
def targetAccessSatisfied
    (requester : User)
    (policy : RelationTargetAccessPolicy)
    (target : NodeRef) : Prop :=
  match policy with
  | .None => True
  | .Read => may_read requester target.owner target.accessKind
  | .Write => may_write requester target.owner target.accessKind

/-- A descriptor's target write-admission policy for this edge. -/
def EdgeTargetAccessValidWith
    (requester : User) (d : RelationDescriptor) (e : Edge) : Prop :=
  targetAccessSatisfied requester d.targetAccessPolicy (edge_target e)

/-- Source-owned edge read: seeing the edge row itself is source-local. Target
    details are a separate projection gate, not part of edge ownership. -/
def edge_read_admitted (requester : User) (e : Edge) : Prop :=
  may_read requester (edge_source e).owner (edge_source e).accessKind

/-- Target projection gate: the target endpoint may be shown only if the
    requester can read the target endpoint's own owner/kind. -/
def edge_target_readable (requester : User) (e : Edge) : Prop :=
  may_read requester (edge_target e).owner (edge_target e).accessKind

/-- For a valid source-owned edge, source-local edge read can equivalently be
    checked against the persisted edge owner. The target owner is intentionally
    absent from this theorem. -/
theorem edge_read_admitted_source_owned :
    ∀ registry requester e, EdgeCoreValid registry e →
      (edge_read_admitted requester e ↔
        may_read requester (edge_owner e) (edge_source e).accessKind) := by
  intro registry requester e hv
  unfold edge_read_admitted
  rw [edge_source_owned registry e hv]

/-- Requester-sensitive edge write admission. Descriptor validity supplies the
    relation row; source write is universal; target read/write is descriptor
    policy. -/
def edge_write_admitted (registry : RelationRegistry) (requester : User) (e : Edge) : Prop :=
  ∃ d : RelationDescriptor,
    d ∈ registry.descriptors ∧
    EdgeValidWith d e ∧
    may_write requester (edge_source e).owner (edge_source e).accessKind ∧
    EdgeTargetAccessValidWith requester d e

/-- Write admission implies ordinary core row validity under the same registry. -/
theorem edge_write_admitted_core_valid :
    ∀ registry requester e, edge_write_admitted registry requester e → EdgeCoreValid registry e := by
  intro registry requester e h
  rcases h with ⟨d, hregistered, hvalid, _, _⟩
  exact ⟨d, hregistered, hvalid⟩

/-- Write admission always includes source write authority. -/
theorem edge_write_admitted_source_write :
    ∀ registry requester e, edge_write_admitted registry requester e →
      may_write requester (edge_source e).owner (edge_source e).accessKind := by
  intro registry requester e h
  rcases h with ⟨_, _, _, hsource, _⟩
  exact hsource

end Causa
