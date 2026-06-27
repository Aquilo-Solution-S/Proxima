/-
Causa — Authorization (group-ownership realign)

Access leaves the entity row entirely (spec §10). Two host-resolved
relations carry it: `group_membership` gives a User the roles of the groups
they belong to; `entity_owner` gives each entity its reachable Owners —
exactly one `is_home` write owner plus read-only share Owners plus the
implicit World group. The request pipeline resolves the requester's read /
write sets `S_read` / `S_write` ONCE; the kernel names only the gates those
sets must satisfy. No grant or visibility flag lives on the entity: the
retired owner-space `AccessGrant` / `MemoryAction` layer is gone, and with it
the "is public" / "is shared" denormalized state (invariant #5).
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

/-- The reserved World group — public read (§2.2 / §6). An ordinary Group
    Owner, distinguished only by being in every read set and never a write
    target. -/
axiom world : Owner
axiom world_is_group : ∃ g : GroupId, world = Principal.group g

/-- `S_read r o` — Owner `o` is in requester `r`'s read set: `r`'s own
    principal, every group `r` belongs to (`visible`, Causa.Owner), and
    World. Host-resolved from group_membership; the kernel takes it as the
    request's read access set and fixes only the floor laws below. -/
axiom S_read  : Principal → Owner → Prop
/-- `S_write r o` — Owner `o` is in requester `r`'s write set: an Owner `r`
    reaches through a non-Viewer membership. World is never here. -/
axiom S_write : Principal → Owner → Prop

/-- AR-self — a requester always reaches its own principal for reads. -/
axiom S_read_self  : ∀ r : Principal, S_read r r
/-- AR-world — World is implicitly in every read set (invariant #4, read half). -/
axiom S_read_world : ∀ r : Principal, S_read r world
/-- AW-read — write authority implies read authority: a writer can read what
    it may write. -/
axiom S_write_subset_read : ∀ (r : Principal) (o : Owner), S_write r o → S_read r o
/-- AW-world — World is read-only: never a write target (invariant #4, write
    half). No write owner is ever World. -/
axiom world_never_writable : ∀ r : Principal, ¬ S_write r world

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
def may_read (r : Principal) (home : Owner) : Prop :=
  ∃ o : Owner, reaches home o ∧ S_read r o

/-- AUTH-WRITE (invariant #1, "single write owner") — a request may WRITE an
    entity iff its one home Owner is in the requester's write set. Shares
    never confer write; there is no second source of write authority. -/
def may_write (r : Principal) (home : Owner) : Prop :=
  S_write r home

/-- AUTH-1 — write implies read: whoever may write an entity may read it
    (its home is reachable and write ⊆ read). THEOREM. -/
theorem may_write_implies_read :
    ∀ (r : Principal) (home : Owner), may_write r home → may_read r home := by
  intro r home hw
  exact ⟨home, reaches_home home, S_write_subset_read r home hw⟩

/-- AUTH-2 — World is read-only: no requester may write an entity whose home
    Owner is World (there is none). THEOREM. -/
theorem world_read_only : ∀ r : Principal, ¬ may_write r world := by
  intro r hw
  exact world_never_writable r hw

/-- AUTH-3 — World is universally readable: every requester may read a
    World-published entity (World is in every read set and is reachable as a
    share). THEOREM. -/
theorem world_universally_readable :
    ∀ (r : Principal) (home : Owner), reaches home world → may_read r home := by
  intro r home hreach
  exact ⟨world, hreach, S_read_world r⟩

end Causa
