/-
Causa — Owner

The scoping primitive (doc 01 §Owner). Every Event, Memory, and Goal
carries an Owner: the access scope. Ontologically, Owner IS the
principal — personal (User) or group-shared (Group). The closed
two-constructor shape of `Principal` is itself the axiom.

**Org is not a kernel concept** (renegotiated 2026-06-11, decision
`2026-06-11-org-out-of-kernel.md`). Billing/quota attribution
(`org_id`) is engine metadata annotated on storage rows. Previously
`Owner` was `Principal × OrgId`: while org never entered the access
rule, it DID enter Owner equality — and Owner equality is the premise
of every structural gate (F→A batch gate, single-owner edge scope,
goal parents, citations owner-match), so a billing label silently
shaped graph topology. With Owner = Principal, gate premises compare
principal only: the same user under two orgs is one identity.

**No `Principal.Org` variant.** Org-wide visibility is expressed as a
default `<org>-everyone` group whose membership auto-syncs with org
membership (doc 01 v1 constraints).

Owner here is the single `is_home` WRITE owner. Read-only sharing — the
former "v2+ `AccessGrant`" — is now realized as `entity_owner` reachability
rows and modeled in `Causa.Authorization` (`reaches` / `may_read`), never as
a flag on the entity. So an entity carries one Owner (this home) yet is
reachable by many; `visible` below is the per-Owner reachability primitive
those read/write sets are built from, not the whole access rule.
-/

import Causa.Prelude

namespace Causa

-- ============================================================
-- Identity slots
-- ============================================================

axiom UserId  : Type
axiom GroupId : Type

-- ============================================================
-- Principal — access scope
-- ============================================================

/-- Access scope: personal (only that user) or group-shared (group
    members). Closed sum — doc 01 §Owner. -/
inductive Principal where
  | user  (u : UserId)
  | group (g : GroupId)

/-- Group membership. Lives in usermanager app-side; the kernel
    commits only to the membership predicate the access rule needs. -/
axiom group_members : GroupId → Set UserId

-- ============================================================
-- Owner
-- ============================================================

/-- Owner IS the principal (doc 01 §Owner, renegotiated 2026-06-11 —
    decision `2026-06-11-org-out-of-kernel.md`). The former engine
    `owner_org_id` billing annotation was dropped from Core storage
    entirely in S0 (Track B, 2026-06); tenancy is now a flavor/app
    concern, with no kernel face. -/
def Owner : Type := Principal

def owner_principal (o : Owner) : Principal := o

-- ============================================================
-- Visibility — per-Owner reachability (the read-set primitive)
-- ============================================================

/-- ES-2 — when a requester reaches a single Owner principal, verbatim from
    doc 01 §Owner:

      visible(o, requester) iff
          o.principal == User(requester)
        ∨ ( o.principal == Group(g) ∧ requester ∈ members(g) )

    A definition, not an axiom: the kernel fixes the rule's content. Under
    group-ownership this is no longer the WHOLE entity access rule — an
    entity is reachable through any of its `entity_owner` Owners — but the
    per-Owner test `S_read` is assembled from (self ∪ groups ∪ World); see
    `Causa.Authorization.may_read` for the set-reachability gate. ES-3: org
    does not exist at this layer — never an access predicate, never identity. -/
def visible (o : Owner) (requester : UserId) : Prop :=
  match owner_principal o with
  | .user u  => u = requester
  | .group g => requester ∈ group_members g

end Causa
