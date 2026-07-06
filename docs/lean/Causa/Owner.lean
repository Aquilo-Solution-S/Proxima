/-
Causa — Owner

The scoping primitive (doc 01 §Owner). Every Memory and Goal carries an Owner:
its access scope.

Ontology (realign 2026-06-28): **a resolved Owner is always a Group**, and a
Group maps each member to a `Role`. Stable persisted owner references are modeled
separately in `Causa.Identity` (`OwnerRef`); this file models the resolved
Leopard-style result. The one irreducible atom is `User` (a role-bearing
identity atom: human user, configured Agent, service actor); groups, resolved
owners, roles, and World are structural over it.

A `Role` is two independent capability ceilings over the access ladder
F < A < P < G — how high a member may READ and how high they may WRITE (read and
write are separate, so an `Ingest` owner role never widens into a general editor)
— plus a `manage` flag for META-MANAGEMENT of the group itself (membership and
role assignment). A "user" is the special case of a Group: the personal group,
automatically present for every user, in which that user holds the maximal
`personal` role and no one else is a member. Personal groups are auto-derived
and forbid meta-management (`personal_forbids_manage`).

**Org is not a kernel concept** (decision `2026-06-11-org-out-of-kernel.md`):
billing/quota attribution is engine metadata, never a kernel face; org-wide
visibility is a default `<org>-everyone` group.

The resolved Owner is the SINGLE owning Group used by authorization checks. Row
storage should point at a stable `OwnerRef`; the host resolves that reference to
this group-shaped `Owner` before the kernel rule runs. There is no separate
`is_home`/`reaches`/`entity_owner` reachability layer (removed with the share
set, D11): read-only sharing is a viewer-role membership in that one group, and
publishing is transfer to World. One entity, one owner group.
-/

import Causa.Prelude

namespace Causa

-- ============================================================
-- The one irreducible atom
-- ============================================================

/-- The identity atom: an engine-minted token (UUIDv7 in the engine).
    EQUALITY is all the kernel asks of it. This replaces the former opaque
    identity atom: the representation stays sealed (private constructor and
    field — no projection or construction outside this module), but equality
    is now computable and the mint `User.ofToken` is visible to proofs.
    Concrete user records (who exists, attributes, catalogs) remain inputs
    from the world — flavor/engine sidecar data, never kernel fields. -/
structure User where
  private mk ::
  private token : String
  deriving DecidableEq, Repr

/-- The only public way to obtain a `User`: mint from an engine token. -/
def User.ofToken (t : String) : User := ⟨t⟩

/-- Distinct tokens mint distinct users — identity IS the token. -/
theorem User.ofToken_inj {s t : String}
    (h : User.ofToken s = User.ofToken t) : s = t := by
  cases h
  rfl

/-- Kernel witness inhabitant — a proof artifact for closed non-vacuity
    witnesses, not a real identity. Real users are engine-minted. -/
instance : Inhabited User := ⟨User.ofToken "causa-kernel-witness"⟩

-- ============================================================
-- The access ladder and roles
-- ============================================================

/-- The four core entity kinds, ranked for ACCESS. This is the access axis
    (who may touch what); it is distinct from the edge-directionality layer ℓ
    in `Causa.Edges`, which keeps Goals outside the F/A/P comparison (ME-14).
    Here all four are linearly ranked. -/
inductive AccessKind where
  | fact | abstraction | perspective | goal
  deriving DecidableEq, Repr

def AccessKind.rank : AccessKind → Nat
  | .fact => 0 | .abstraction => 1 | .perspective => 2 | .goal => 3

/-- A role = two capability ceilings over the access ladder plus a
    meta-management flag. `read` / `write` are layer counts (0 = none, 1 = F,
    2 = F+A, 3 = F+A+P, 4 = all incl. Goals); capability over a kind `k` is
    `k.rank < ceiling`. `manage` is the authority to write the GROUP's own role
    map (add/remove members, assign roles) — distinct from writing entities.
    `write_le_read` makes "write ⊆ read" a structural field, not an axiom — a
    member can always read at least what it may write. -/
structure Role where
  read  : Nat
  write : Nat
  manage : Bool
  write_le_read : write ≤ read

def Role.mayRead  (x : Role) (k : AccessKind) : Prop := k.rank < x.read
def Role.mayWrite (x : Role) (k : AccessKind) : Prop := k.rank < x.write
/-- Meta-management authority: may add/remove members and assign roles in the
    group. A group-level rule still gates the use of it — personal groups
    forbid management entirely (see `personal_forbids_manage`). -/
def Role.manages (x : Role) : Prop := x.manage = true

/-- Maximal ENTITY authority over one's own scope, but NOT an admin role: the
    personal group is auto-derived and unmanageable, so `manage := false`. -/
def Role.personal : Role := ⟨4, 4, false, by omega⟩
/-- Read every kind, write none, no management. -/
def Role.viewer   : Role := ⟨4, 0, false, by omega⟩
/-- Source ingest: Facts in and out only. -/
def Role.ingest   : Role := ⟨1, 1, false, by omega⟩
/-- Author up to Perspectives, not Goals (illustrative preset). -/
def Role.editor   : Role := ⟨4, 3, false, by omega⟩
/-- Group admin: full read/write plus meta-management of the group. -/
def Role.admin    : Role := ⟨4, 4, true, by omega⟩

-- ============================================================
-- Role lattice — combine roles across membership paths
-- ============================================================

/-- Meet (cap): the WEAKER of two roles per capability. Used to cap a mounted
    sub-group's inherited authority. `write_le_read` is preserved (min is
    monotone). -/
def Role.meet (x y : Role) : Role :=
  ⟨min x.read y.read, min x.write y.write, x.manage && y.manage, by
    have hx := x.write_le_read; have hy := y.write_le_read; omega⟩

/-- Join: the STRONGER of two roles per capability. Used when a user is a member
    via two paths — the higher capability wins. `write_le_read` is preserved
    (max is monotone). -/
def Role.join (x y : Role) : Role :=
  ⟨max x.read y.read, max x.write y.write, x.manage || y.manage, by
    have hx := x.write_le_read; have hy := y.write_le_read; omega⟩

-- ============================================================
-- Group and Owner
-- ============================================================

/-- A Group maps each member to their `Role`; `none` = not a member. This is the
    resolved authorization view, not the stable persisted owner-table handle.
    `abbrev` keeps it reducible. -/
abbrev Group : Type := User → Option Role

/-- A resolved Owner is always a Group (doc 01 §Owner). Entity rows eventually
    carry stable `OwnerRef`s; authorization uses the host-resolved `Owner`. -/
abbrev Owner : Type := Group

/-- A user/agent as an Owner: the personal group, in which only that identity
    is a member and holds the maximal `personal` role. -/
def Owner.ofUser (u : User) : Owner :=
  fun x => if x = u then some Role.personal else none

/-- A personal owner contains its own user at the personal role. -/
theorem Owner.ofUser_self (u : User) :
    Owner.ofUser u u = some Role.personal := by
  simp [Owner.ofUser]

/-- A personal owner contains no other user. -/
theorem Owner.ofUser_other {u x : User} (h : x ≠ u) :
    Owner.ofUser u x = none := by
  simp [Owner.ofUser, h]

/-- An Owner is personal iff it is some user's personal group. -/
def Owner.isPersonal (o : Owner) : Prop := ∃ u : User, o = Owner.ofUser u

/-- The reserved World group — public read. Every user is a member at `viewer`:
    reads all kinds, writes none. Defined at the resolved-owner layer; the stable
    `.world` owner reference is introduced in `Causa.Identity`. -/
def world : Owner := fun _ => some Role.viewer

-- ============================================================
-- Membership
-- ============================================================

/-- ES-2 — a requester is a member of (visible to) an Owner iff it holds any
    role there. Read/write authority is then the role's ceiling over the kind
    (see `Causa.Authorization`). For a personal owner, membership reduces to
    identity (`visible_personal`): "only the user can join it". A definition,
    not an axiom. ES-3: org does not exist at this layer. -/
def visible (o : Owner) (requester : User) : Prop := o requester ≠ none

/-- Computable membership test — the Bool face of `visible`. -/
def Group.member? (g : Group) (u : User) : Bool := (g u).isSome

instance (o : Owner) (u : User) : Decidable (visible o u) :=
  match h : o u with
  | some _ => .isTrue (by simp [visible, h])
  | none   => .isFalse (by simp [visible, h])

/-- The personal-owner reduction: membership in a user's own group is exactly
    being that user. -/
theorem visible_personal (u requester : User) :
    visible (Owner.ofUser u) requester ↔ requester = u := by
  simp only [visible, Owner.ofUser]
  split <;> simp_all

-- ============================================================
-- Group composition (nesting) — Level 2
-- ============================================================

/- Nesting needs NO new kernel primitive: a nested group RESOLVES to an ordinary
   `Owner` (`User → Option Role`), and the gates are total over it. The
   combinators below are how the host composes that resolution; the kernel never
   sees the nesting tree (materializing it — e.g. a Leopard view — is the host's
   expensive job). Group-as-member is not a kernel notion, and a recursive
   `Group → Option Role` field would be non-strictly-positive anyway. -/

/-- The empty group: no user has a role. -/
def Group.empty : Group := fun _ => none

/-- The universal group at a fixed role: every user has that role. -/
def Group.everyone (r : Role) : Group := fun _ => some r

/-- The universal group at any role is distinct from the empty group. -/
theorem Group.everyone_ne_empty (r : Role) : Group.everyone r ≠ Group.empty := by
  intro h
  have hval := congrFun h default
  simp [Group.everyone, Group.empty] at hval

/-- Mount sub-group `g` into a parent at role `cap`: every member of `g` joins at
    their g-role capped by `cap` (lattice meet). Produces an ordinary `Owner`;
    the no-escalation guarantee is `mount_cannot_escalate` (Authorization). -/
def Group.mount (g : Group) (cap : Role) : Group :=
  fun u => (g u).map (fun x => Role.meet x cap)

/-- Union of two groups: a user's role is the join of their roles in each (the
    higher capability when a member of both). -/
def Group.union (a b : Group) : Group :=
  fun u =>
    match a u, b u with
    | some x, some y => some (Role.join x y)
    | some x, none   => some x
    | none,   some y => some y
    | none,   none   => none

/-- Drop a user from a group — the membership change behind sharing-removal and
    compliance erasure: the user loses their role, everyone else is unchanged.
    This is the ONLY mutable face of ownership; the entity row that names this
    group never changes (cognitive content stays append-only). When the last
    member is dropped the group is abandoned (`Causa.Compliance.abandoned`). -/
def Group.drop (g : Group) (u : User) : Group :=
  fun x => if x = u then none else g x

/-- Dropping a user removes that user's own role. -/
theorem Group.drop_self (g : Group) (u : User) :
    Group.drop g u u = none := by
  simp [Group.drop]

/-- Dropping one user leaves all other users unchanged. -/
theorem Group.drop_other {g : Group} {u x : User} (h : x ≠ u) :
    Group.drop g u x = g x := by
  simp [Group.drop, h]

end Causa
