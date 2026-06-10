# Decision: goal_parents_same_owner kept despite doc-citation gap

**Date:** 2026-06-11 (axiom-minimization pass)
**Status:** decided — flagged for Heinrich's review

## The finding

The adversarial verifier confirmed that `goal_parents_same_owner`'s cited
passage (doc 06 §Scoping: "Cross-owner Goal assignment and cross-owner
evidence are rejected") does not actually cover DAG parents — the §Scoping
table enumerates Goal row, Self-Perspective, `core/inspires` edge,
`motivated-by` edge, and Lifecycle Fact, but not `parent_goal_ids`. Strictly,
no doc passage states that a Goal's DAG parents share its Owner.

## Decision

KEEP the axiom, re-grounded on doc 04 §Execution model and isolation: "Owner
is the access boundary. Cross-owner reads and edges are invalid." The parent
relation is stored edge-like (`goal_parents` table) and a cross-owner parent
link would pierce the access boundary exactly the way a cross-owner edge
would. Removing an isolation guarantee overnight on a citation technicality is
the wrong direction; if the docs intend cross-owner goal hierarchies (e.g.
org-wide parent goals over personal sub-goals), that is a deliberate design
extension to make in writing, not a transcription default.

## Effect

Axiom retained; its doc-comment now cites doc 04 §Isolation rather than
doc 06 §Scoping. Surfaced here per the surface-don't-silently-pick rule.
