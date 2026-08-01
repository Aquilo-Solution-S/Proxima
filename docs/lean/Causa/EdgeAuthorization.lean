/-
Causa — Edge authorization

ONE uniform admission rule replaces the per-relation `ownerPolicy` /
`targetAccessPolicy` matrix (doc 16 §Ownership and visibility):

  the row is owned by the SOURCE owner (E2), and the write is admitted iff the
  writer holds write authority on the SOURCE and read authority on the TARGET
  at write time.

Two consequences the retired matrix used to spell out per descriptor:

  * a cross-owner TARGET is admissible — E2 constrains the source owner only,
    which is what makes cross-owner provenance expressible
    (`cross_owner_target_admitted`);
  * supersession never crosses Owners, not by policy but because it is not an
    edge at all — it is a lineage pointer on the row, and a row supersedes its
    own prior head (Causa.Memory, Causa.Goals).

Reading is source-local: seeing the row is the source's read gate, and
rendering the target is a separate gate whose failure REDACTS the endpoint
rather than suppressing the row (Causa.Compliance).

No verb writes an edge directly, so this gate runs as part of the node write
that declares the row (doc 02 §The Directionality Rule).
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

/-- Source-owned row read: seeing the row itself is source-local. Target
    details are a separate projection gate, not part of row ownership. -/
def edge_read_admitted (requester : User) (e : Edge) : Prop :=
  may_read requester (edge_source e).owner (edge_source e).accessKind

/-- Target projection gate: the target endpoint may be shown only if the
    requester can read the target endpoint's own owner/kind. -/
def edge_target_readable (requester : User) (e : Edge) : Prop :=
  may_read requester (edge_target e).owner (edge_target e).accessKind

/-- For a valid source-owned row, source-local read can equivalently be checked
    against the persisted owner column. The target owner is intentionally
    absent from this theorem. -/
theorem edge_read_admitted_source_owned :
    ∀ (requester : User) (e : Edge), EdgeValid e →
      (edge_read_admitted requester e ↔
        may_read requester (edge_owner e) (edge_source e).accessKind) := by
  intro requester e hv
  unfold edge_read_admitted
  rw [edge_source_owned e hv]

/-- AUTH-EDGE — the uniform admission rule, in full. There is no third
    conjunct: no relation row to look up, no owner policy cell, no
    target-access policy. -/
def edge_write_admitted (requester : User) (e : Edge) : Prop :=
  EdgeValid e ∧
  may_write requester (edge_source e).owner (edge_source e).accessKind ∧
  may_read requester (edge_target e).owner (edge_target e).accessKind

/-- Write admission implies ordinary row validity. -/
theorem edge_write_admitted_valid :
    ∀ requester e, edge_write_admitted requester e → EdgeValid e := by
  intro _ _ h
  exact h.1

/-- Write admission always includes source write authority. -/
theorem edge_write_admitted_source_write :
    ∀ requester e, edge_write_admitted requester e →
      may_write requester (edge_source e).owner (edge_source e).accessKind := by
  intro _ _ h
  exact h.2.1

/-- Write admission always includes target READ authority — never target write.
    A node may point at something it is not allowed to change. -/
theorem edge_write_admitted_target_read :
    ∀ requester e, edge_write_admitted requester e → edge_target_readable requester e := by
  intro _ _ h
  exact h.2.2

/-- E2 constrains the SOURCE owner only: a valid row whose target belongs to
    another Owner is admitted as soon as the writer may read that target.
    Nothing else is consulted, which is the whole content of "one uniform rule
    replaces the per-descriptor matrix". -/
theorem cross_owner_target_admitted :
    ∀ (requester : User) (e : Edge), EdgeValid e →
      may_write requester (edge_source e).owner (edge_source e).accessKind →
      edge_target_readable requester e →
      edge_write_admitted requester e := by
  intro requester e hvalid hwrite hread
  exact ⟨hvalid, hwrite, hread⟩

/-- An admitted write's row is owned by the source, so the writer's own write
    authority covers the row it creates. -/
theorem edge_write_admitted_owns_row :
    ∀ requester e, edge_write_admitted requester e →
      may_write requester (edge_owner e) (edge_source e).accessKind := by
  intro requester e h
  rw [← edge_source_owned e h.1]
  exact h.2.1

end Causa
