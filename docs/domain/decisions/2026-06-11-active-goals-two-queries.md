# Decision: two active-goal queries, one kernel carrier

**Date:** 2026-06-11 (kernel transcription, review round r1)
**Status:** decided — flagged for Heinrich's review

## The tension

`docs/06-goals-and-self.md` defines active goals twice with different scopes:

- §Goal Entity: `G_active(owner)` = current Goal heads where state = Active —
  owner-global.
- §Goal Assignment: `active_goals(instance)` = follow `core/inspires`
  assignments into the instance's current Self-Perspective, take supersession
  heads, filter state = Active — instance-scoped.

The codex review (r1) flagged that the kernel transcribed only the first and
silently dropped the second.

## Decision

The kernel carries `activeGoals (o : Owner)` (the §Goal Entity query) and
documents the duality at the definition site. The instance-scoped query stays
engine-level because it requires the NAMED relation constant `core/inspires`,
and the kernel deliberately models relation **classes and shapes**, not named
relation ids (same stance that keeps `core/derived-from` and
`proxima-goal/motivated-by` as vocabulary rather than kernel constants — the
shapes are pinned via `atogoal_edge_shape`, `ftoa_edge_shape`, etc.).

These are two different scopes of the same head/Active filter — a layering
choice in the doc, not a contradiction. If the named-relation-constants stance
ever changes (e.g. a future slice axiomatizes `core/inspires` to formalize
Self), `active_goals(instance)` becomes definable in-kernel from the same
parts.

## Effect

- `Goals.lean activeGoals` doc-comment cross-references this decision.
- COVERAGE.md GO-12 row updated: assignment SHAPE is kernel-carried
  (Structural Goal→Perspective via descriptor masks), traversal query is
  engine-level with this decision as the recorded reason.
