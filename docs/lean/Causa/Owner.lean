/-
Causa — Owner

The scoping primitive (doc 01 §Owner). Every Memory and Goal carries
an Owner: its access scope.

Ontology (realign 2026-06-28): **an Owner is always a Group**, and a
Group IS the set of users permitted to join it. A "user" is the special
case — the singleton group only that user can join. There is no
User/Group sum and no group-handle primitive: the one irreducible atom
is `User` (a person); groups, owners, and World are all structural over
it. Owner equality is therefore extensional — an owner *is* its
membership — which is exactly the identity-collapse the access layer
wants (the same person under two billing labels is one identity).

**Org is not a kernel concept** (decision `2026-06-11-org-out-of-kernel.md`).
Billing/quota attribution is engine metadata, never a kernel face;
org-wide visibility is expressed as a default `<org>-everyone` group
whose membership auto-syncs with org membership (doc 01 v1 constraints).

Owner here is the single `is_home` WRITE owner. Read-only sharing is
realized as `entity_owner` reachability rows in `Causa.Authorization`
(`reaches` / `may_read`), never as a flag on the entity: an entity
carries one home Owner yet is reachable by many. `visible` below is the
per-Owner reachability primitive those read/write sets are built from,
not the whole access rule.
-/

import Causa.Prelude

namespace Causa

-- ============================================================
-- The one irreducible atom
-- ============================================================

/-- A person — the single identity primitive of the kernel. The kernel
    cannot define the set of persons structurally; their existence is
    its one input from the world. Identity is the whole content of
    `User`: two users differ because they are different inhabitants.
    Attributes (name, preferences, …) are flavor sidecar data over this
    atom, never kernel fields — a field no proof reads is dead trusted
    weight, so properties arrive only when a theorem consumes them. -/
axiom User : Type

-- ============================================================
-- Group — a set of users
-- ============================================================

/-- A Group IS the set of users permitted to join it. No separate
    group-handle primitive: a group's identity is exactly its
    membership (extensional). `abbrev` keeps it reducible so the `Set`
    membership instance resolves through `Group` / `Owner`. -/
abbrev Group : Type := Set User

-- ============================================================
-- Owner — always a Group
-- ============================================================

/-- An Owner is always a Group (doc 01 §Owner). The access scope of
    every Memory and Goal. -/
abbrev Owner : Type := Group

/-- A user as an Owner: the personal group only that user can join — the
    singleton `{u}` (written explicitly, the minimal `Set` carries no
    singleton notation). This is what "a User" denotes at the access
    layer; a User is the special case of a Group. -/
def Owner.ofUser (u : User) : Owner := fun x => x = u

/-- An Owner is personal iff it is some user's singleton group. -/
def Owner.isPersonal (o : Owner) : Prop := ∃ u : User, o = Owner.ofUser u

-- ============================================================
-- Visibility — per-Owner reachability (the read-set primitive)
-- ============================================================

/-- ES-2 — a requester reaches an Owner iff they are a member of its
    group:

      visible(o, requester) iff requester ∈ o

    For a personal owner this reduces to identity
    (`requester ∈ Owner.ofUser u ↔ requester = u`) — "only the user can
    join it". A definition, not an axiom: the kernel fixes the rule's
    content. Under group-ownership this is the per-Owner test; an entity
    is reachable through any of its `entity_owner` Owners, assembled into
    `S_read` (see `Causa.Authorization.may_read`). ES-3: org does not
    exist at this layer — never an access predicate, never identity. -/
def visible (o : Owner) (requester : User) : Prop := requester ∈ o

/-- The personal-owner reduction, made explicit: reaching a user's own
    group is exactly being that user. -/
theorem visible_personal (u requester : User) :
    visible (Owner.ofUser u) requester ↔ requester = u := Iff.rfl

end Causa
