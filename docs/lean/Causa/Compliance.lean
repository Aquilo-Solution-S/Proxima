/-
Causa — Compliance

The ONLY delete path in the system (doc 13; ST-13). The cognitive lifecycle is
append-only (Facts immutable; A/P/Goals supersede); erasure is the separate,
out-of-band lifecycle. The whole of it reduces to ONE idea: a reference counter.

Every entity is owned by a Group (Causa.Owner) — its access scope. The owning
group's MEMBERSHIP is that reference count. Sharing adds members; a user
dropping removes them (`Group.drop`). When the owning group has no members left,
the entity is ABANDONED — reference count zero — and may be hard-deleted.
Nothing else licenses deletion: live data (data someone still owns) is never
wiped. "Core wipes data only when a user drops" is exactly this.

The entity row never mutates: the immutable cognitive content names one stable
group, and ALL mutability — sharing, dropping, compliance — lives in the group
roster. So erasure needs no field mutation and no append-only exception; it is
the observable emptiness of the owning group, not an opaque `erased` flag.

Engine/controller mechanics are NOT kernel faces (recorded in COVERAGE.md):
suppression/dedup keys (CO-15..20), pause/resume dispatch gates (CO-9/10),
export serialization (CO-11), the admin op/outcome protocol (CO-2..14),
audit-row content (CO-21..29), external side effects (CO-30..33), owner-policy
defaults and GDPR article mappings (CO-46..58). The kernel fixes only the RULE:
abandoned ⇔ wipeable, plus the cascade from a node to its edges.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Edges
import Causa.Authorization

namespace Causa

-- ============================================================
-- Abandonment — the single delete trigger (doc 13 §Operations; ST-13)
-- ============================================================

/-- Reference count zero: no user holds a role in the owning group, so nobody
    owns the entity. The ONE condition that licenses a hard delete. A definition,
    not an opaque `erased` axiom: the kernel states *when* a wipe is lawful (the
    owning group is empty), never that substrate rows are physically gone — that
    is the engine performing the wipe. `Group.drop` (Causa.Owner) removes a
    member; when the last is gone, `abandoned` holds. -/
def abandoned (o : Owner) : Prop := ∀ u : User, o u = none

/-- CO-7 / ST-13 — the GDPR theorem: when a user drops out of their own personal
    group, that group abandons, so everything they solely own becomes wipeable.
    The entire erasure contract for a personal owner, discharged as a theorem
    over `Group.drop`. No `erased` / `erasure_removes_cognitive` axiom remains. -/
theorem drop_personal_abandoned (u : User) :
    abandoned ((Owner.ofUser u).drop u) := by
  intro v
  by_cases h : v = u <;> simp [Group.drop, Owner.ofUser, h]

/-- CO-7' edge face — cascade soundness (re-cast of the old `erasure_removes_edges`,
    now a one-liner): a VALID edge whose source endpoint has an abandoned owner is
    itself abandoned, because a valid edge inherits its source's owner
    (`edge_source_owned`, projected from `EdgeCoreValid`). Wiping abandoned nodes
    therefore licenses wiping their edges — the cascade is sound. THEOREM, no axiom. -/
theorem source_abandoned_cascades_to_edge
    (registry : RelationRegistry) (e : Edge) (hv : EdgeCoreValid registry e) :
    abandoned (edge_source e).owner → abandoned (edge_owner e) := by
  intro h
  rw [← edge_source_owned registry e hv]
  exact h

/-- The retention boundary: World is never abandoned — every user is a member at
    `viewer` — so published/public data is never auto-wiped by the abandonment
    rule. Any user witnesses the owning group's non-emptiness. THEOREM. -/
theorem world_never_abandoned (u : User) : ¬ abandoned world := by
  intro h
  have hu := h u
  simp [world] at hu

end Causa
