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
-- Reachability — the entity's share set (spec §3 / §10 invariants 1–2)
-- ============================================================

/-- An entity's reachable Owners — its `entity_owner` rows. The entity is named
    by its single `is_home` write Owner `home`; `reaches home o` also holds for
    every read-only share Owner and for World once published. Host-resolved. -/
axiom reaches : Owner → Owner → Prop
/-- RE-home — an entity is always reachable as its own home Owner. -/
axiom reaches_home : ∀ home : Owner, reaches home home

-- ============================================================
-- The gates (per kind)
-- ============================================================

/-- AUTH-READ (invariant #2) — a request may READ an entity of kind `k` iff
    some Owner the entity is shared into has the requester holding a role whose
    read ceiling covers `k`. Nothing on the entity row is consulted. -/
def may_read (r : User) (home : Owner) (k : AccessKind) : Prop :=
  ∃ o : Owner, reaches home o ∧ ∃ x : Role, o r = some x ∧ x.mayRead k

/-- AUTH-WRITE (invariant #1, "single write owner") — a request may WRITE an
    entity of kind `k` iff the requester holds a role at its one home Owner
    whose write ceiling covers `k`. Shares never confer write. -/
def may_write (r : User) (home : Owner) (k : AccessKind) : Prop :=
  ∃ x : Role, home r = some x ∧ x.mayWrite k

/-- AUTH-MANAGE — a request may META-MANAGE a group `o` (write its membership /
    role map) iff `o` is not a personal group AND the requester holds a role
    there with management authority. The personal-group exclusion is a group
    property, not merely a role one: personal groups are auto-derived and
    immutable. -/
def may_manage (r : User) (o : Owner) : Prop :=
  ¬ Owner.isPersonal o ∧ ∃ x : Role, o r = some x ∧ x.manages

/-- AUTH-1 — write implies read: whoever may write an entity may read it.
    THEOREM — from `Role.write_le_read` (read ceiling ≥ write ceiling) and
    `reaches_home` (the home Owner is reachable). -/
theorem may_write_implies_read :
    ∀ (r : User) (home : Owner) (k : AccessKind),
      may_write r home k → may_read r home k := by
  rintro r home k ⟨x, hx, hw⟩
  exact ⟨home, reaches_home home, x, hx, Nat.lt_of_lt_of_le hw x.write_le_read⟩

/-- AUTH-2 — World is read-only: no requester may write a World-homed entity.
    THEOREM — World maps everyone to `viewer`, whose write ceiling is 0, so
    `k.rank < 0` is absurd. -/
theorem world_read_only :
    ∀ (r : User) (k : AccessKind), ¬ may_write r world k := by
  rintro r k ⟨x, hx, hw⟩
  simp only [world, Option.some.injEq] at hx
  subst hx
  exact Nat.not_lt_zero _ hw

/-- AUTH-3 — World is universally readable: every requester may read a
    World-shared entity (World maps everyone to `viewer`, read ceiling 4 > any
    kind rank). THEOREM. -/
theorem world_universally_readable :
    ∀ (r : User) (home : Owner) (k : AccessKind),
      reaches home world → may_read r home k := by
  intro r home k hreach
  exact ⟨world, hreach, Role.viewer, rfl, by cases k <;> simp [Role.mayRead, Role.viewer, AccessKind.rank]⟩

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

end Causa
