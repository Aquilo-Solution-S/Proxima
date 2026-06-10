/-
Proxima Foundations — Owner

The scoping primitive (doc 01 §Owner). Every Event, Memory, and Goal
carries an Owner. Two distinct concerns, deliberately split:

  - **principal** — access scope. Who may see this.
  - **org_id**   — billing unit. Measured for data usage / quota.

`org_id` NEVER enters the access rule (doc 01: "Access rule (`org_id`
never enters)"; doc 06 §Scoping: "`org_id` is not an access
predicate"). The kernel encodes this by making `visible` a function
of the principal alone.

**No `Principal.Org` variant.** Org-wide visibility is expressed as a
default `<org>-everyone` group whose membership auto-syncs with org
membership (doc 01 v1 constraints). The two-constructor shape of
`Principal` is itself the axiom.

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
axiom OrgId   : Type

-- ============================================================
-- Principal — access scope
-- ============================================================

/-- Access scope: personal (only that user) or group-shared (group
    members). Closed sum — doc 01 §Owner. -/
inductive Principal where
  | user  (u : UserId)
  | group (g : GroupId)

-- ============================================================
-- Owner
-- ============================================================

axiom Owner : Type
axiom owner_principal : Owner → Principal
axiom owner_org       : Owner → OrgId

/-- v1: a Group lives in exactly one org (doc 01: "Group lives in one
    org: `group.org_id` set at creation"). Cross-org groups are v2+. -/
axiom group_org : GroupId → OrgId

/-- Group membership. Lives in usermanager app-side; the kernel
    commits only to the membership predicate the access rule needs. -/
axiom group_members : GroupId → Set UserId

/-- ES-1 — org denormalization (doc 01 §Owner): when the principal is
    a Group, the Owner's org is the group's org. A memory's
    `owner.org_id` is denormalised from `group.org_id`. -/
axiom owner_org_denormalized :
  ∀ (o : Owner) (g : GroupId),
    owner_principal o = .group g → owner_org o = group_org g

-- ============================================================
-- Visibility — THE access rule
-- ============================================================

/-- ES-2 — the access rule, verbatim from doc 01 §Owner:

      visible(m, requester) iff
          m.principal == User(requester.user_id)
        ∨ ( m.principal == Group(g) ∧ requester ∈ members(g) )

    A definition, not an axiom: the kernel fixes the rule's content.
    ES-3: `owner_org` does not appear — org_id is a billing
    dimension, never an access predicate. -/
def visible (o : Owner) (requester : UserId) : Prop :=
  match owner_principal o with
  | .user u  => u = requester
  | .group g => requester ∈ group_members g

end Proxima
