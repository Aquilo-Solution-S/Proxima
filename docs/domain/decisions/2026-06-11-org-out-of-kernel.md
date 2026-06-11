# Decision: org is not a kernel concept — Owner = Principal

**Date:** 2026-06-11 (morning review of the kernel transcription)
**Status:** decided by Heinrich

## The question

Heinrich, reviewing the four overnight decisions: "the organization
principle probably should not be inside the kernel of Proxima. In the
end we have Owner and Group of Owners, and you may have access to
modify groups. But organization sounds not very generic."

## The finding that made it precise

The kernel already had no `Principal.Org` variant and `visible` was a
function of the principal alone — but `Owner` was defined as
`Principal × OrgId` (with the v1 group→org denormalization subtype).
While `org_id` never entered the *access rule*, it DID enter **Owner
equality**, and Owner equality is the premise of every structural
gate: `ftoa_batch_exclusive`'s same-owner premise, single-owner edge
scope, `goal_parents_same_owner`, the citations owner-match, and the
doc 11 dedup UNIQUE key. Doc 01's own example has the same user
emitting under `u.personal_org` vs an org — two distinct Owners whose
memories could never be edge-linked and whose batches never unified.
"Billing dimension, never an access predicate" was true for reads but
false for the graph topology: a billing label silently shaped
ontology.

## Decision

Org is demoted to the engine entirely:

1. **Kernel:** `Owner := Principal` (closed sum `user | group`).
   `OrgId` and `group_org` axioms removed; the denormalization subtype
   and the ES-1 theorem dissolve (nothing left to denormalize).
   `visible` unchanged. No other kernel file referenced org (verified:
   zero references outside Owner.lean).
2. **Engine:** `owner_org_id` stays as a storage column — a billing /
   quota annotation filled from `group.org_id` (group principals) or
   the user's personal org (user principals). It appears in NO
   uniqueness key and NO predicate. Doc 11's `cited_objects` UNIQUE
   key drops `owner_org_id`.
3. **Docs renegotiated in writing** (kernel-wins rule): 01 §Owner
   rewritten; 06 §Scoping, 07 §Owner Columns + §ID Types context,
   11 §Tables, 12 §dispatch updated.

Semantic consequence (intended): gate premises now compare principal
only, so the same user under two orgs is ONE identity — parallel
personal/work universes per org are no longer a structural accident.
Org-wide sharing remains expressible exactly as before, via the
`<org>-everyone` group.

## Alternatives considered

- **Keep the record, loosen the gates** — keep `org_id` on Owner but
  rewrite every owner-equality premise to principal-equality. Fixes
  the leak, keeps the non-generic field, touches every gate: worst of
  both.
- **Keep as-is** — defensible only if the per-org identity split was
  intended. It was not.

## Engine drift to watch (project ② / membrane work)

Any Rust code that compares the full `Owner` struct (including
`org_id`) for gate exclusivity, edge validation, or dedup now
diverges from the kernel and must move to principal-only comparison.
The `cited_objects` UNIQUE constraint must drop `owner_org_id` when
the schema lands / migrates. WH multi-org tenancy (project ③) maps to
Groups + engine billing metadata at the membrane — no kernel friction.
