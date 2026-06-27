/-
Causa — Authorization (group-ownership realign; role-graded access)

Access leaves the entity row entirely (spec §10). A Group maps each member to a
`Role` (Causa.Owner): two capability ceilings over the access ladder F<A<P<G —
how high one may READ and how high one may WRITE. Read and write are
independent, so a source-ingest role (write Facts only) never widens into a
general editor.

The entity is named by its single `is_home` write Owner; `reaches` gives the
Owners it is shared into (its `entity_owner` rows). The host resolves the DATA
— who populates each group at what role, and which Owners an entity reaches —
ONCE; the kernel fixes only the RULE (read/write = the role's ceiling over the
kind) and names the share relation. World is the universal read-only group. No
grant or visibility flag lives on the entity (invariant #5); the retired
owner-space `AccessGrant` / `MemoryAction` layer is gone.
-/

import Causa.Prelude
import Causa.Owner

namespace Causa

/-- The reserved World group — public read (§2.2 / §6). Every user is a member
    at `viewer`: reads all kinds, writes none. A definition; World's read-only
    character is now a theorem (`world_read_only`), not an axiom. -/
def world : Owner := fun _ => some Role.viewer

-- ============================================================
-- The gates (per kind) — spec §3 / §10 invariants 1–2
-- ============================================================

/- An entity has exactly ONE owner — a Group (Causa.Owner). Access is entirely
   the requester's role in that group: a read-only share is a viewer-role
   membership, write is an editor-or-higher role. There is NO share set above
   the owner, because the Group already IS the sharing mechanism — to share an
   entity you move it to (or create) a group holding the desired members at the
   desired roles; to publish it you transfer it to World (the universal viewer
   group). One owner per entity ⇒ the "single write owner" invariant is
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

/-- AUTH-MANAGE — a request may META-MANAGE a group `o` (write its membership /
    role map) iff `o` is not a personal group AND the requester holds a role
    there with management authority. The personal-group exclusion is a group
    property, not merely a role one: personal groups are auto-derived and
    immutable. -/
def may_manage (r : User) (o : Owner) : Prop :=
  ¬ Owner.isPersonal o ∧ ∃ x : Role, o r = some x ∧ x.manages

/-- AUTH-1 — write implies read: whoever may write an entity may read it.
    THEOREM — same owning group, and `Role.write_le_read` (read ceiling ≥ write
    ceiling). -/
theorem may_write_implies_read :
    ∀ (r : User) (o : Owner) (k : AccessKind),
      may_write r o k → may_read r o k := by
  rintro r o k ⟨x, hx, hw⟩
  exact ⟨x, hx, Nat.lt_of_lt_of_le hw x.write_le_read⟩

/-- AUTH-2 — World is read-only: an entity owned by World cannot be written.
    THEOREM — World maps everyone to `viewer`, whose write ceiling is 0, so
    `k.rank < 0` is absurd. -/
theorem world_read_only :
    ∀ (r : User) (k : AccessKind), ¬ may_write r world k := by
  rintro r k ⟨x, hx, hw⟩
  simp only [world, Option.some.injEq] at hx
  subst hx
  exact Nat.not_lt_zero _ hw

/-- AUTH-3 — World is universally readable: an entity owned by World is readable
    by every requester (World maps everyone to `viewer`, read ceiling 4 > any
    kind rank). THEOREM. -/
theorem world_universally_readable :
    ∀ (r : User) (k : AccessKind), may_read r world k := by
  intro r k
  exact ⟨Role.viewer, rfl, by cases k <;> simp [Role.mayRead, Role.viewer, AccessKind.rank]⟩

/-- AUTH-MANAGE-personal — personal groups forbid meta-management: no requester
    may manage a user's personal group. THEOREM — the gate requires
    `¬ Owner.isPersonal o`, and a personal group is, by construction, personal. -/
theorem personal_forbids_manage :
    ∀ (u r : User), ¬ may_manage r (Owner.ofUser u) := by
  rintro u r ⟨hnp, _⟩
  exact hnp ⟨u, rfl⟩

/-- AUTH-MANAGE-world — the World group cannot be managed either: every member
    is a `viewer` (`manage = false`), so no one holds a managing role there.
    THEOREM — independent of the personal-group rule. -/
theorem world_forbids_manage : ∀ r : User, ¬ may_manage r world := by
  rintro r ⟨_, x, hx, hm⟩
  simp only [world, Option.some.injEq] at hx
  subst hx
  simp [Role.manages, Role.viewer] at hm

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
