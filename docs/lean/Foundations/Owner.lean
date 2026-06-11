/-
Proxima Foundations — Owner

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

Per-memory ACL (`AccessGrant`) is a v2+ extension layered above
Owner — deliberately absent here.
-/

import Foundations.Prelude

namespace Proxima

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
    decision `2026-06-11-org-out-of-kernel.md`). The engine's
    `owner_org_id` billing annotation has no kernel face. -/
def Owner : Type := Principal

def owner_principal (o : Owner) : Principal := o

-- ============================================================
-- Visibility — THE access rule
-- ============================================================

/-- ES-2 — the access rule, verbatim from doc 01 §Owner:

      visible(m, requester) iff
          m.principal == User(requester.user_id)
        ∨ ( m.principal == Group(g) ∧ requester ∈ members(g) )

    A definition, not an axiom: the kernel fixes the rule's content.
    ES-3: org does not exist at this layer — it is a billing
    dimension on engine storage rows, never an access predicate and
    never part of identity. -/
def visible (o : Owner) (requester : UserId) : Prop :=
  match owner_principal o with
  | .user u  => u = requester
  | .group g => requester ∈ group_members g

end Proxima
