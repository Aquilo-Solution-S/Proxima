/-
Causa — Authorization (group-ownership realign)

Access leaves the entity row entirely (spec §10). Two host-resolved
relations carry it: `group_membership` gives a User the roles of the groups
they belong to; `entity_owner` gives each entity its reachable Owners —
exactly one `is_home` write owner plus read-only share Owners plus the
implicit World group. The request pipeline resolves the requester's read /
group memberships and the entity's reachable Owners ONCE. Read access is then
membership directly — `visible o r` (`r ∈ o`, Causa.Owner) — with nothing
below it; the kernel keeps only the write set `S_write` opaque and names the
floor laws it must satisfy. The requester is a `User` (a person). No grant or
visibility flag lives on the entity: the retired owner-space
`AccessGrant` / `MemoryAction` layer is gone, and with it the "is public" /
"is shared" denormalized state (invariant #5).
-/

import Causa.Prelude
import Causa.Owner

namespace Causa

/-- Membership roles (group_membership). Host-assigned; the kernel commits to
    the authority each conveys: every relation reads, only the non-Viewer
    relations write (Ingest writes source events, Editor authors, Admin
    configures). The closed four-constructor shape is the axiom. -/
inductive Relation where
  | admin | editor | viewer | ingest
  deriving DecidableEq, Repr

/-- Relations that confer write authority — everything but Viewer. -/
def Relation.writes : Relation → Prop
  | .viewer => False
  | _       => True

/-- The reserved World group — public read (§2.2 / §6). The universal group:
    every user is a member. A definition now that an Owner is a set of users;
    World is distinguished only by being in every read set and never a write
    target (the floor laws below), not by any stored flag. -/
def world : Owner := fun _ => True

/- Read access IS membership. `visible o r` (Causa.Owner, `r ∈ o`) is the read
   predicate directly — there is nothing below visibility. The old opaque
   `S_read` set and its floor laws collapse into membership: because World is
   the universal group, even world-readability is a membership fact, not a
   separate assumption. The host still resolves the DATA (which users populate
   each group); the kernel fixes the RULE. -/

/-- AR-self — a requester always reads its own personal group. THEOREM:
    membership in the singleton `{r}` is identity. -/
theorem read_self : ∀ r : User, visible (Owner.ofUser r) r :=
  fun r => (visible_personal r r).mpr rfl

/-- AR-world — World is readable by everyone. THEOREM: World is the universal
    group, so every user is a member. -/
theorem world_visible : ∀ r : User, visible world r := by
  intro _; exact True.intro

/-- `S_write r o` — Owner `o` is in requester `r`'s write set: `r` reaches `o`
    through a non-Viewer membership. World is never here. Host-resolved; the
    kernel keeps it opaque and names only the floor laws below (the write-side
    opacity is the next decision). -/
axiom S_write : User → Owner → Prop

/-- AW-read — write authority implies membership: a writer is a member of what
    it may write, so it can read it. -/
axiom S_write_implies_visible : ∀ (r : User) (o : Owner), S_write r o → visible o r
/-- AW-world — World is read-only: never a write target (invariant #4, write
    half). No write owner is ever World. -/
axiom world_never_writable : ∀ r : User, ¬ S_write r world

-- ============================================================
-- Reachability and the gates (spec §3 / §10 invariants 1–2)
-- ============================================================

/-- An entity's reachable Owners — its `entity_owner` rows. The entity is
    named by its single `is_home` write Owner `home` (memory_owner /
    goal_owner in the cognitive layer); `reaches home o` additionally holds
    for every read-only share Owner and for World once published. -/
axiom reaches : Owner → Owner → Prop
/-- RE-home — an entity is always reachable as its own home Owner. -/
axiom reaches_home : ∀ home : Owner, reaches home home

/-- AUTH-READ (invariant #2, "read = reachability") — a request may READ an
    entity iff some reachable Owner of it is in the requester's read set.
    This is the only read path; nothing on the entity row is consulted. -/
def may_read (r : User) (home : Owner) : Prop :=
  ∃ o : Owner, reaches home o ∧ visible o r

/-- AUTH-WRITE (invariant #1, "single write owner") — a request may WRITE an
    entity iff its one home Owner is in the requester's write set. Shares
    never confer write; there is no second source of write authority. -/
def may_write (r : User) (home : Owner) : Prop :=
  S_write r home

/-- AUTH-1 — write implies read: whoever may write an entity may read it
    (its home is reachable and write ⊆ read). THEOREM. -/
theorem may_write_implies_read :
    ∀ (r : User) (home : Owner), may_write r home → may_read r home := by
  intro r home hw
  exact ⟨home, reaches_home home, S_write_implies_visible r home hw⟩

/-- AUTH-2 — World is read-only: no requester may write an entity whose home
    Owner is World (there is none). THEOREM. -/
theorem world_read_only : ∀ r : User, ¬ may_write r world := by
  intro r hw
  exact world_never_writable r hw

/-- AUTH-3 — World is universally readable: every requester may read a
    World-published entity (World is in every read set and is reachable as a
    share). THEOREM. -/
theorem world_universally_readable :
    ∀ (r : User) (home : Owner), reaches home world → may_read r home := by
  intro r home hreach
  exact ⟨world, hreach, world_visible r⟩

end Causa
