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
abandoned ⇔ wipeable, source abandonment cascades to source-owned edges, and
target abandonment redacts/suppresses target projection rather than changing edge
ownership.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Edges
import Causa.Authorization
import Causa.EdgeAuthorization

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

/-- Target-side erasure state for source-owned edges: the target endpoint's
    owner is abandoned. This is NOT the edge owner's erasure state. -/
def edge_target_abandoned (e : Edge) : Prop := abandoned (edge_target e).owner

/-- A target projection is available exactly when the requester may read the
    target endpoint and that target has not been abandoned. -/
def edge_target_available (requester : User) (e : Edge) : Prop :=
  edge_target_readable requester e ∧ ¬ edge_target_abandoned e

/-- Redaction state for a readable source-owned edge: the edge row can be shown
    from its source side, but the target projection cannot be rendered because the
    target is unreadable or erased. -/
def edge_target_redacted (requester : User) (e : Edge) : Prop :=
  edge_read_admitted requester e ∧ ¬ edge_target_available requester e

/-- If the source-owned edge row is visible but the requester cannot read the
    target, the target projection is redacted. -/
theorem target_unreadable_redacts_edge_target
    (requester : User) (e : Edge)
    (hread : edge_read_admitted requester e)
    (hunreadable : ¬ edge_target_readable requester e) :
    edge_target_redacted requester e := by
  exact ⟨hread, by
    rintro ⟨htarget, _⟩
    exact hunreadable htarget⟩

/-- If the source-owned edge row is visible but the target owner is abandoned,
    the target projection is redacted. Target erasure affects projection, not
    source-owned edge ownership. -/
theorem target_abandoned_redacts_edge_target
    (requester : User) (e : Edge)
    (hread : edge_read_admitted requester e)
    (htarget : edge_target_abandoned e) :
    edge_target_redacted requester e := by
  exact ⟨hread, by
    rintro ⟨_, hnotAbandoned⟩
    exact hnotAbandoned htarget⟩

/-- Target abandonment alone is not a delete license for a source-owned edge: if
    the source owner remains live, a valid edge's owner remains live even when
    the target owner is abandoned. The target consequence is redaction/suppression
    (`target_abandoned_redacts_edge_target`), not source cascade. -/
theorem target_abandoned_does_not_abandon_source_owned_edge
    (registry : RelationRegistry) (e : Edge) (hv : EdgeCoreValid registry e)
    (hsourceLive : ¬ abandoned (edge_source e).owner)
    (_htarget : edge_target_abandoned e) :
    ¬ abandoned (edge_owner e) := by
  intro hedge
  apply hsourceLive
  rw [edge_source_owned registry e hv]
  exact hedge

/-- The retention boundary: World is never abandoned — every user is a member at
    `viewer` — so published/public data is never auto-wiped by the abandonment
    rule. Any user witnesses the owning group's non-emptiness. THEOREM. -/
theorem world_never_abandoned (u : User) : ¬ abandoned world := by
  intro h
  have hu := h u
  simp [world] at hu

end Causa
