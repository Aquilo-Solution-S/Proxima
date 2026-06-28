/-
Causa — Edge write admission

RelationDescriptor owns row-shape policy in `Causa.Edges`; requester-specific
write admission composes that descriptor policy with the group access gates in
`Causa.Authorization`.

The kernel law is narrow:

  * every edge write requires write authority on the source endpoint;
  * the relation descriptor selects no/read/write authority on the target;
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

/-- Requester-sensitive edge write admission. Descriptor validity supplies the
    relation row; source write is universal; target read/write is descriptor
    policy. -/
def edge_write_admitted (requester : User) (e : Edge) : Prop :=
  ∃ d : RelationDescriptor,
    EdgeValidWith d e ∧
    may_write requester (edge_source e).owner (edge_source e).accessKind ∧
    EdgeTargetAccessValidWith requester d e

/-- Write admission implies ordinary core row validity. -/
theorem edge_write_admitted_core_valid :
    ∀ requester e, edge_write_admitted requester e → EdgeCoreValid e := by
  intro requester e h
  rcases h with ⟨d, hvalid, _, _⟩
  exact ⟨d, hvalid⟩

/-- Write admission always includes source write authority. -/
theorem edge_write_admitted_source_write :
    ∀ requester e, edge_write_admitted requester e →
      may_write requester (edge_source e).owner (edge_source e).accessKind := by
  intro requester e h
  rcases h with ⟨_, _, hsource, _⟩
  exact hsource

end Causa
