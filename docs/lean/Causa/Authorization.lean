/-
Causa — Authorization (group-ownership realign; role-graded access)

Access leaves the entity row entirely (spec §10). A Group maps each member to a
`Role` (Causa.Owner): two capability ceilings over the access ladder F<A<P<G —
how high one may READ and how high one may WRITE. Read and write are
independent, so an `Ingest` owner role (write Facts only) never widens into a
general editor.

Each entity has a SINGLE owning Group. There is no `is_home`/`reaches`/
`entity_owner` share layer (removed with the share set, D11): a read-only share
is just a viewer-role membership in that one group, and sharing beyond that is
an owner-to-owner TRANSFER of the entity into another group. The host resolves
the DATA — who populates each group at what role — ONCE; the kernel fixes only
the RULE (read/write = the role's ceiling over the kind). There is no
universal-read owner: an entity is exactly as readable as its owning group. No
grant or visibility flag lives on the entity (invariant #5); the retired
owner-space `AccessGrant` / `MemoryAction` layer is gone.
-/

import Causa.Prelude
import Causa.Owner
import Causa.Identity

namespace Causa

-- ============================================================
-- The gates (per kind) — spec §3 / §10 invariants 1–2
-- ============================================================

/- An entity has exactly ONE owner — a Group (Causa.Owner). Access is entirely
   the requester's role in that group: a read-only share is a viewer-role
   membership, write is an editor-or-higher role. There is NO share set above
   the owner, because the Group already IS the sharing mechanism — to share an
   entity you move it to (or create) a group holding the desired members at the
   desired roles. One owner per entity ⇒ the "single write owner" invariant is
   structural, and per-entity sharing is per-entity group choice. -/

/-- AUTH-READ (invariant #2) — a request may READ an entity of kind `k` iff the
    requester holds a role in the entity's owning group whose read ceiling
    covers `k`. -/
def may_read (r : User) (o : Owner) (k : AccessKind) : Prop :=
  ∃ x : Role, o r = some x ∧ x.mayRead k

/-- AUTH-WRITE (invariant #1, "single write owner") — a request may WRITE an
    entity of kind `k` iff the requester holds a role in its owning group whose
    write ceiling covers `k`. One owner group, so there is one write scope. -/
def may_write (r : User) (o : Owner) (k : AccessKind) : Prop :=
  ∃ x : Role, o r = some x ∧ x.mayWrite k

/-- Server-resolved read gate: rows store `OwnerRef`; the trusted host resolves
    it through `OwnerState` before the pure kernel rule runs. -/
def may_read_in (s : OwnerState) (r : User) (o : OwnerRef) (k : AccessKind) : Prop :=
  may_read r (s.resolve o) k

/-- Server-resolved write gate: callers never supply the resolved Owner map. -/
def may_write_in (s : OwnerState) (r : User) (o : OwnerRef) (k : AccessKind) : Prop :=
  may_write r (s.resolve o) k

/-- AUTH-MANAGE — a request may META-MANAGE a group `o` (write its membership /
    role map) iff `o` is not a personal group AND the requester holds a role
    there with management authority. The personal-group exclusion is a group
    property, not merely a role one: personal groups are auto-derived and
    immutable. -/
def may_manage (r : User) (o : Owner) : Prop :=
  ¬ Owner.isPersonal o ∧ ∃ x : Role, o r = some x ∧ x.manages

/-- Server-resolved management gate over stable owner references. -/
def may_manage_in (s : OwnerState) (r : User) (o : OwnerRef) : Prop :=
  may_manage r (s.resolve o)

/-- AUTH-1 — write implies read: whoever may write an entity may read it.
    THEOREM — same owning group, and `Role.write_le_read` (read ceiling ≥ write
    ceiling). -/
theorem may_write_implies_read :
    ∀ (r : User) (o : Owner) (k : AccessKind),
      may_write r o k → may_read r o k := by
  rintro r o k ⟨x, hx, hw⟩
  exact ⟨x, hx, Nat.lt_of_lt_of_le hw x.write_le_read⟩

/-- AUTH-1 over the stable owner boundary: resolution does not weaken the core
    role law. -/
theorem may_write_in_implies_read_in :
    ∀ (s : OwnerState) (r : User) (o : OwnerRef) (k : AccessKind),
      may_write_in s r o k → may_read_in s r o k := by
  intro s r o k h
  exact may_write_implies_read r (s.resolve o) k h

/-- AUTH-NO-UNIVERSAL-READ — access IS membership: a requester holding no role
    in the owning group can neither read nor write the entity, whatever its
    kind. THEOREM. This is what stands where the World lane stood: no owner is
    readable by everyone, so no owner needs an exception carved out of the
    read/write gates. -/
theorem non_member_denied :
    ∀ (r : User) (o : Owner) (k : AccessKind),
      o r = none → ¬ may_read r o k ∧ ¬ may_write r o k := by
  intro r o k hout
  constructor
  · rintro ⟨x, hx, _⟩; rw [hout] at hx; exact Option.noConfusion hx
  · rintro ⟨x, hx, _⟩; rw [hout] at hx; exact Option.noConfusion hx

/-- The same law over the stable owner boundary: server resolution introduces no
    universally readable reference. -/
theorem owner_state_non_member_denied :
    ∀ (s : OwnerState) (r : User) (o : OwnerRef) (k : AccessKind),
      s.resolve o r = none →
      ¬ may_read_in s r o k ∧ ¬ may_write_in s r o k := by
  intro s r o k hout
  exact non_member_denied r (s.resolve o) k hout

/-- AUTH-MANAGE-personal — personal groups forbid meta-management: no requester
    may manage a user's personal group. THEOREM — the gate requires
    `¬ Owner.isPersonal o`, and a personal group is, by construction, personal. -/
theorem personal_forbids_manage :
    ∀ (u r : User), ¬ may_manage r (Owner.ofUser u) := by
  rintro u r ⟨hnp, _⟩
  exact hnp ⟨u, rfl⟩

/-- The stable personal owner reference resolves to exactly that user's personal
    group. Visibility through the owner table therefore reduces to identity. -/
theorem owner_state_personal_visible :
    ∀ (s : OwnerState) (u r : User),
      visible (s.resolve (.personal u)) r ↔ r = u := by
  intro s u r
  rw [s.personal_resolves u]
  exact visible_personal u r

/-- Personal owner refs remain unmanageable after server resolution. -/
theorem owner_state_personal_forbids_manage :
    ∀ (s : OwnerState) (u r : User), ¬ may_manage_in s r (.personal u) := by
  intro s u r h
  unfold may_manage_in at h
  rw [s.personal_resolves u] at h
  exact personal_forbids_manage u r h

/-- Read access through a stable owner ref depends only on the server-resolved
    owner map for that ref. -/
theorem may_read_in_resolve_eq :
    ∀ (s₁ s₂ : OwnerState) (r : User) (o : OwnerRef) (k : AccessKind),
      s₁.resolve o = s₂.resolve o →
      (may_read_in s₁ r o k ↔ may_read_in s₂ r o k) := by
  intro s₁ s₂ r o k h
  unfold may_read_in
  rw [h]

/-- Write access through a stable owner ref depends only on the server-resolved
    owner map for that ref. -/
theorem may_write_in_resolve_eq :
    ∀ (s₁ s₂ : OwnerState) (r : User) (o : OwnerRef) (k : AccessKind),
      s₁.resolve o = s₂.resolve o →
      (may_write_in s₁ r o k ↔ may_write_in s₂ r o k) := by
  intro s₁ s₂ r o k h
  unfold may_write_in
  rw [h]

/-- NEST-safe — mounting a sub-group at a cap cannot escalate write authority:
    if the cap may not write kind `k`, no requester gains write on the mounted
    group (the mounted role's write ceiling is capped below the cap's). The
    security law of capped nesting. THEOREM, no axiom. -/
theorem mount_cannot_escalate
    (g : Group) (cap : Role) (k : AccessKind) (h : ¬ cap.mayWrite k) (r : User) :
    ¬ may_write r (Group.mount g cap) k := by
  rintro ⟨x, hx, hw⟩
  simp only [Group.mount] at hx
  cases hgr : g r with
  | none => rw [hgr] at hx; simp at hx
  | some x' =>
    rw [hgr] at hx
    simp at hx
    subst hx
    simp only [Role.mayWrite, Role.meet] at hw h
    omega

/-- NEST-union — the union grants at least each side's access: a requester that
    may read kind `k` via either group may read it via their union (join never
    reduces a member's capability). The write case is analogous. THEOREM. -/
theorem union_grants_each (a b : Group) (r : User) (k : AccessKind) :
    (may_read r a k ∨ may_read r b k) → may_read r (Group.union a b) k := by
  rintro (⟨x, hax, hx⟩ | ⟨y, hby, hy⟩)
  · cases hbr : b r with
    | none => exact ⟨x, by simp [Group.union, hax, hbr], hx⟩
    | some y =>
      refine ⟨Role.join x y, by simp [Group.union, hax, hbr], ?_⟩
      simp only [Role.mayRead, Role.join] at hx ⊢; omega
  · cases har : a r with
    | none => exact ⟨y, by simp [Group.union, har, hby], hy⟩
    | some x =>
      refine ⟨Role.join x y, by simp [Group.union, har, hby], ?_⟩
      simp only [Role.mayRead, Role.join] at hy ⊢; omega

end Causa
